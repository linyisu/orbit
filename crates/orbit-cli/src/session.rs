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
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/share")))
        .context("set --database or HOME to select a session database")?;
    Ok(data.join("orbit/orbit.db"))
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
        let writer = if let Some(id) = resume {
            let saved = store.load(&id).await?;
            ensure!(
                saved.session.workspace == workspace,
                "session belongs to workspace {}",
                saved.session.workspace
            );
            ensure!(
                saved.session.demo == demo,
                "session mode differs; use --demo for a demo session"
            );
            store.acquire(&id).await?
        } else {
            store.create(workspace, model, demo).await?
        };
        let snapshot = store.load(writer.id()).await?;
        Ok(Self {
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

    pub async fn clear(&mut self, model: String, demo: bool) -> Result<()> {
        ensure!(!self.active, "cannot switch sessions during a run");
        let writer = self
            .store
            .create(self.snapshot.session.workspace.clone(), model, demo)
            .await?;
        let snapshot = self.store.load(writer.id()).await?;
        self.writer = writer;
        self.snapshot = snapshot;
        Ok(())
    }
}
