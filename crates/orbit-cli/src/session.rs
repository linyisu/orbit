use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use orbit_agent::events::AgentEvent;
use orbit_model::types::ChatMessage;
use orbit_store::{
    store::{SessionWriter, Store},
    types::Snapshot,
};

use crate::tui::app::{ToolStatus, TranscriptItem, UiState};

pub fn database_path(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(std::path::absolute(path)?);
    }
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(user_data_directory)
        .context("set --database or a user data directory to select a session database")?;
    Ok(data.join("orbit/orbit.db"))
}

fn user_data_directory() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
    }
    #[cfg(target_os = "macos")]
    {
        std::env::var_os("HOME")
            .map(|p| PathBuf::from(p).join("Library/Application Support"))
            .filter(|p| p.is_absolute())
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        std::env::var_os("HOME")
            .map(|p| PathBuf::from(p).join(".local/share"))
            .filter(|p| p.is_absolute())
    }
}

pub async fn list_text(store: &Store) -> Result<String> {
    let rows = store.list().await?;
    if rows.is_empty() {
        return Ok("暂无已保存会话。".into());
    }
    Ok(rows
        .into_iter()
        .map(|s| {
            format!(
                "{}  {}  {}  {}",
                s.id,
                if s.demo { "demo" } else { "live" },
                s.title.replace(['\n', '\r', '\t'], " "),
                s.workspace
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

pub struct SessionState {
    pub store: Store,
    writer: SessionWriter,
    pub snapshot: Snapshot,
    pub scratch: orbit_tools::run_command::ScratchDirectory,
    pending: Vec<AgentEvent>,
    active: bool,
    flushed_at: Instant,
}

impl SessionState {
    pub async fn open(
        path: PathBuf,
        resume: Option<String>,
        model: String,
        demo: bool,
    ) -> Result<Self> {
        let store = Store::open(path).await?;
        let workspace = std::env::current_dir()?
            .canonicalize()?
            .to_string_lossy()
            .into_owned();
        let project = store.resolve_project(&workspace).await?;
        let writer = if let Some(id) = resume {
            let saved = store.session(&id).await?;
            ensure!(
                saved.project_id == project.id,
                "session belongs to workspace {}",
                saved.workspace
            );
            ensure!(
                saved.demo == demo,
                "session mode differs; use --demo for a demo session"
            );
            store.acquire(&id).await?
        } else {
            store.create(workspace, model, demo).await?
        };
        let snapshot = store.load(writer.id()).await?;
        let scratch = std::sync::Arc::new(std::sync::RwLock::new(
            store.path().with_extension("workspaces").join(writer.id()),
        ));
        Ok(Self {
            scratch,
            store,
            writer,
            snapshot,
            pending: Vec::new(),
            active: false,
            flushed_at: Instant::now(),
        })
    }

    pub fn id(&self) -> &str {
        self.writer.id()
    }

    pub fn restore_ui(&self, state: &mut UiState) {
        state.workspace = self.snapshot.session.workspace.clone();
        for run in &self.snapshot.runs {
            state.input.history.push(run.prompt.clone());
            state.submit(run.prompt.clone());
            for event in &run.events {
                state.apply(event.clone());
            }
            match run.status.as_str() {
                "completed" => state.apply(AgentEvent::Completed {
                    turns: state.turn,
                    tool_calls: state.tool_calls,
                }),
                "cancelled" => state.apply(AgentEvent::Cancelled),
                "interrupted" | "running" => state
                    .fail("上次运行被中断；已恢复到最后一次成功完成的模型上下文，未重新执行工具。"),
                _ => state.fail("上次运行失败；模型上下文保留在上一轮成功状态。"),
            }
            for item in &mut state.transcript {
                if let TranscriptItem::Tool(tool) = item
                    && tool.status == ToolStatus::Running
                {
                    tool.status = ToolStatus::Failed;
                    tool.output = "运行已结束或中断，未保存工具结果；不会自动重试。".into();
                }
            }
        }
        if !self.snapshot.runs.is_empty() {
            state.transcript.push(TranscriptItem::Notice(format!(
                "会话 {} · 自动保存\n模型沿用当前配置：{}",
                self.id(),
                state.model
            )));
        }
    }

    pub async fn begin(&mut self, prompt: String, model: String) -> Result<()> {
        self.writer.begin(prompt, model).await?;
        self.active = true;
        self.flushed_at = Instant::now();
        Ok(())
    }

    pub fn record(&mut self, event: &AgentEvent) {
        self.pending.push(event.clone());
    }

    pub async fn flush_due(&mut self) -> Result<()> {
        if self.active
            && !self.pending.is_empty()
            && (self.pending.len() >= 256
                || self.flushed_at.elapsed() >= Duration::from_millis(250))
        {
            self.writer.append(self.pending.clone()).await?;
            self.pending.clear();
            self.flushed_at = Instant::now();
        }
        Ok(())
    }

    pub async fn finish(&mut self, status: &str, history: Option<Vec<ChatMessage>>) -> Result<()> {
        self.writer
            .finish(status, self.pending.clone(), history)
            .await?;
        self.pending.clear();
        self.active = false;
        Ok(())
    }

    pub async fn close(&mut self) -> Result<()> {
        if self.active {
            self.record(&AgentEvent::Cancelled);
            self.finish("cancelled", None).await?;
        }
        Ok(())
    }

    pub async fn resumable(&self) -> Result<Vec<orbit_store::types::Session>> {
        Ok(self
            .store
            .list()
            .await?
            .into_iter()
            .filter(|saved| {
                saved.project_id == self.snapshot.session.project_id
                    && saved.demo == self.snapshot.session.demo
                    && saved.id != self.id()
                    && !saved.title.is_empty()
            })
            .collect())
    }

    pub async fn resume(&mut self, id: &str) -> Result<()> {
        ensure!(!self.active, "请先取消当前响应，再恢复会话。");
        if id == self.id() {
            self.snapshot = self.store.load(id).await?;
            return Ok(());
        }
        let saved = self.store.session(id).await?;
        ensure!(
            saved.project_id == self.snapshot.session.project_id,
            "该会话属于其他项目，请在对应目录恢复。"
        );
        ensure!(
            saved.demo == self.snapshot.session.demo,
            "演示会话和真实 API 会话不能混用。"
        );
        // Acquire and load the destination before replacing the current writer.
        // A missing/locked session must leave the current session fully usable.
        let writer = self.store.acquire(id).await?;
        let snapshot = self.store.load(id).await?;
        self.writer = writer;
        self.snapshot = snapshot;
        *self.scratch.write().expect("scratch lock poisoned") = self
            .store
            .path()
            .with_extension("workspaces")
            .join(self.id());
        self.pending.clear();
        self.flushed_at = Instant::now();
        Ok(())
    }

    pub async fn new_session(&mut self, model: String, demo: bool) -> Result<()> {
        ensure!(!self.active, "cannot switch sessions during a run");
        let writer = self
            .store
            .create(self.snapshot.session.workspace.clone(), model, demo)
            .await?;
        let snapshot = self.store.load(writer.id()).await?;
        self.writer = writer;
        self.snapshot = snapshot;
        *self.scratch.write().expect("scratch lock poisoned") = self
            .store
            .path()
            .with_extension("workspaces")
            .join(self.id());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn switching_restores_context_preserves_old_session_and_checks_scope_and_locks() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state =
            SessionState::open(tmp.path().join("sessions.db"), None, "model".into(), true)
                .await
                .unwrap();
        let first = state.id().to_owned();
        let scratch = state.scratch.clone();
        let first_scratch = scratch.read().unwrap().clone();
        let history = vec![
            ChatMessage::user("first question"),
            ChatMessage::assistant("first answer"),
        ];
        state
            .begin("first question".into(), "model".into())
            .await
            .unwrap();
        assert!(state.resume("missing").await.is_err());
        state
            .finish("completed", Some(history.clone()))
            .await
            .unwrap();
        // Resuming the current session reloads the latest successful history.
        state.resume(&first).await.unwrap();
        assert_eq!(state.snapshot.history, history);
        state.new_session("model".into(), true).await.unwrap();
        let second = state.id().to_owned();
        assert_ne!(first, second);
        assert_ne!(*scratch.read().unwrap(), first_scratch);
        assert!(state.snapshot.history.is_empty());
        let foreign_path = tmp.path().join("foreign");
        std::fs::create_dir(&foreign_path).unwrap();
        let foreign = state
            .store
            .create(
                foreign_path.to_string_lossy().into_owned(),
                "model".into(),
                true,
            )
            .await
            .unwrap();
        let live = state
            .store
            .create(
                state.snapshot.session.workspace.clone(),
                "model".into(),
                false,
            )
            .await
            .unwrap();
        assert!(state.resume(foreign.id()).await.is_err());
        assert!(state.resume(live.id()).await.is_err());
        assert_eq!(
            state
                .resumable()
                .await
                .unwrap()
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            vec![first.as_str()]
        );
        let locked = state.store.acquire(&first).await.unwrap();
        assert!(state.resume(&first).await.is_err());
        assert_eq!(state.id(), second);
        assert!(state.resume("missing").await.is_err());
        assert_eq!(state.id(), second);
        drop(locked);
        state.resume(&first).await.unwrap();
        assert_eq!(*scratch.read().unwrap(), first_scratch);
        assert_eq!(state.snapshot.history, history);
        assert_eq!(state.snapshot.runs.len(), 1);
        assert!(state.store.load(&second).await.is_ok());
        // The old lease is released after a successful switch.
        assert!(state.store.acquire(&second).await.is_ok());
        drop(state);
        let reopened = SessionState::open(
            tmp.path().join("sessions.db"),
            Some(first),
            "model".into(),
            true,
        )
        .await
        .unwrap();
        assert_eq!(*reopened.scratch.read().unwrap(), first_scratch);
    }
}
