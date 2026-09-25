use std::{
    path::Path,
    path::PathBuf,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, ensure};
use orbit_agent::events::AgentEvent;
use orbit_model::types::Attachment;
use orbit_model::types::ChatMessage;
use orbit_model::types::ReasoningEffort;
use orbit_store::{
    store::{SessionWriter, Store},
    types::Snapshot,
};

use crate::tui::app::{ToolStatus, TranscriptItem, UiState};

pub fn database_path(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(std::path::absolute(path)?);
    }

    let data = data_directory()?;

    Ok(data.join("orbit/orbit.db"))
}

pub fn data_directory() -> Result<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(user_data_directory)
        .context("set XDG_DATA_HOME or a user data directory to store Orbit data")
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
                "{}  {}  {}",
                s.id,
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
    pub command_output: orbit_tools::run_command::CommandOutputDirectory,
    pub git_baseline: Option<serde_json::Value>,
    pub audit_workspace: Option<orbit_runtime::filesystem::Workspace>,
    pub audit_limits: orbit_tools::run_command::CommandLimits,
    active: bool,
    journal: Option<std::sync::Arc<dyn orbit_agent::checkpoint::ExecutionJournal>>,
}

pub struct PreparedResume {
    writer: SessionWriter,
    pub snapshot: Snapshot,
    git_baseline: Option<serde_json::Value>,
}

/// Replay saved events in bounded batches. This is local display work, not
/// another agent run; it never invokes a tool or appends a stored event.
#[derive(Default)]
pub struct HistoryReplay {
    run: usize,
    event: usize,
    run_start: Option<usize>,
    done: bool,
}

impl HistoryReplay {
    pub fn completed_runs(&self) -> usize {
        self.run
    }

    pub fn advance(
        &mut self,
        snapshot: &Snapshot,
        state: &mut UiState,
        max_events: usize,
        budget: Duration,
    ) -> bool {
        if self.done {
            return true;
        }

        let started = Instant::now();
        let mut processed = 0;
        state.workspace = snapshot.session.workspace.clone();

        while self.run < snapshot.runs.len() && processed < max_events && started.elapsed() < budget
        {
            let run = &snapshot.runs[self.run];

            if self.run_start.is_none() {
                self.run_start = Some(state.transcript.len());
                state.input.history.push(run.prompt.clone());

                if run.prompt == "/compact" {
                    state
                        .transcript
                        .push(TranscriptItem::Command(run.prompt.clone()));
                } else {
                    // Do not reset the reader's scroll position between runs.
                    let (scroll, follow) = (state.scroll, state.follow_bottom);
                    state.submit(run.prompt.clone());
                    state.scroll = scroll;
                    state.follow_bottom = follow;
                }
            }

            if let Some(event) = run.events.get(self.event) {
                // Receipt fingerprints and archived summaries can be large;
                // the transcript has no presentation for these metadata events.
                if !matches!(
                    event,
                    AgentEvent::CompactionRecorded { .. } | AgentEvent::VerificationRecorded { .. }
                ) {
                    state.apply(event.clone());
                }

                self.event += 1;
                processed += 1;
                continue;
            }

            match run.status.as_str() {
                "completed" => state.apply(AgentEvent::Completed {
                    turns: state.turn,
                    tool_calls: state.tool_calls,
                }),
                "cancelled" => state.apply(AgentEvent::Cancelled),
                _ => {
                    state.status = crate::tui::app::RunStatus::Failed;
                    state.finish_clock();
                }
            }

            if let Some(elapsed_ms) = run.elapsed_ms {
                state.elapsed = Duration::from_millis(elapsed_ms);
                state.started_at = None;
            }

            // Only scan the just-finished run, rather than all preceding runs.
            let first = self.run_start.take().unwrap_or_default();

            for item in &mut state.transcript[first..] {
                if let TranscriptItem::Tool(tool) = item
                    && tool.status == ToolStatus::Running
                {
                    tool.status = ToolStatus::Failed;
                    tool.output = "运行已结束或中断，未保存工具结果；不会自动重试。".into();
                }
            }

            state.invalidate_transcript(first);
            self.run += 1;
            self.event = 0;
            processed += 1;
        }

        if self.run == snapshot.runs.len() {
            self.done = true;
        }

        self.done
    }
}

impl SessionState {
    pub fn import_image(&self, source: &Path) -> Result<Attachment> {
        orbit_tools::image_assets::ImageAssets::for_database(self.store.path()).import_path(source)
    }

    pub fn import_clipboard_image(
        &self,
        width: usize,
        height: usize,
        bytes: Vec<u8>,
    ) -> Result<Attachment> {
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_millis();

        orbit_tools::image_assets::ImageAssets::for_database(self.store.path()).import_rgba(
            format!("clipboard-{timestamp}.png"),
            width,
            height,
            bytes,
        )
    }

    pub async fn open(path: PathBuf, resume: Option<String>, model: String) -> Result<Self> {
        Self::open_in(path, resume, model, &std::env::current_dir()?).await
    }

    pub async fn open_in(
        path: PathBuf,
        resume: Option<String>,
        model: String,
        directory: &Path,
    ) -> Result<Self> {
        let store = Store::open(path).await?;
        let workspace = directory.canonicalize()?.to_string_lossy().into_owned();
        let project = store.resolve_project(&workspace).await?;
        let creating = resume.is_none();
        let writer = if let Some(id) = resume {
            let saved = store.session(&id).await?;
            let equivalent = store.equivalent_project_ids(&project.id).await?;
            ensure!(
                equivalent.contains(&saved.project_id),
                "session belongs to workspace {}",
                saved.workspace
            );
            ensure!(!saved.demo, "演示会话已不再受支持，请创建新会话。");

            store.acquire(&id).await?
        } else {
            store.create(workspace.clone(), model, false).await?
        };

        let git_baseline = if creating {
            let baseline = orbit_tools::git_inspect::capture_baseline(Path::new(&workspace));

            if let Some(baseline) = &baseline {
                store.save_git_baseline(writer.id(), baseline).await?;
            }

            baseline
        } else {
            store.git_baseline(writer.id()).await?
        };

        let snapshot = store.load(writer.id()).await?;
        let scratch = std::sync::Arc::new(std::sync::RwLock::new(
            store.path().with_extension("workspaces").join(writer.id()),
        ));
        let command_output = std::sync::Arc::new(std::sync::RwLock::new(
            store
                .path()
                .with_extension("command-output")
                .join(writer.id()),
        ));

        Ok(Self {
            scratch,
            command_output,
            git_baseline,
            store,
            writer,
            snapshot,
            audit_workspace: None,
            audit_limits: Default::default(),
            active: false,
            journal: None,
        })
    }

    pub async fn move_to(
        &mut self,
        workspace: orbit_runtime::filesystem::Workspace,
        limits: orbit_tools::run_command::CommandLimits,
    ) -> Result<()> {
        ensure!(!self.active, "请先结束当前响应再移动会话");
        let baseline = orbit_tools::git_inspect::capture_baseline(workspace.root());
        let session = self
            .writer
            .move_to(workspace.root(), baseline.clone())
            .await?;

        self.snapshot.session = session;
        self.git_baseline = baseline;
        self.audit_workspace = Some(workspace);
        self.audit_limits = limits;
        Ok(())
    }

    pub async fn initialize_audit(
        &mut self,
        workspace: orbit_runtime::filesystem::Workspace,
        limits: orbit_tools::run_command::CommandLimits,
    ) -> Result<()> {
        self.audit_limits = limits.clone();
        self.audit_workspace = Some(workspace.clone());

        // Never label the current state of an old session as its starting state.
        if self.snapshot.runs.is_empty()
            && self.store.workspace_baseline(self.id()).await?.is_none()
            && let Ok(baseline) = orbit_runtime::execution::fingerprint_configured(
                workspace,
                tokio_util::sync::CancellationToken::new(),
                limits.into(),
            )
            .await
        {
            self.store
                .save_workspace_baseline(self.id(), &baseline)
                .await?;
        }

        Ok(())
    }

    pub fn calibration(&self, profile: &str) -> orbit_agent::calibration::TokenCalibration {
        let mut result = orbit_agent::calibration::TokenCalibration::default();

        for event in self.snapshot.runs.iter().flat_map(|r| &r.events) {
            if let AgentEvent::RequestMetrics {
                profile: key,
                estimated_input,
                usage,
                ..
            } = event
                && key == profile
            {
                result.observe(
                    *estimated_input,
                    usage.as_ref().and_then(|u| u.prompt_tokens),
                );
            }
        }

        result
    }

    pub fn id(&self) -> &str {
        self.writer.id()
    }

    #[cfg(test)]
    pub fn restore_ui(&self, state: &mut UiState) {
        let mut replay = HistoryReplay::default();

        while !replay.advance(&self.snapshot, state, usize::MAX, Duration::MAX) {}
    }

    pub fn saved_model_profile(&self) -> (&str, Option<ReasoningEffort>) {
        (
            &self.snapshot.session.model,
            self.snapshot
                .session
                .reasoning_effort
                .as_deref()
                .and_then(ReasoningEffort::from_str),
        )
    }

    pub async fn set_model_profile(
        &mut self,
        model: String,
        reasoning_effort: Option<ReasoningEffort>,
    ) -> Result<()> {
        self.writer
            .set_model_profile(
                model.clone(),
                reasoning_effort
                    .map(ReasoningEffort::as_str)
                    .map(str::to_owned),
            )
            .await?;
        self.snapshot.session.model = model;
        self.snapshot.session.reasoning_effort = reasoning_effort
            .map(ReasoningEffort::as_str)
            .map(str::to_owned);

        Ok(())
    }

    pub async fn rename(&mut self, title: String) -> Result<()> {
        self.writer.set_title(title.clone()).await?;
        self.snapshot.session.title = title;
        Ok(())
    }

    pub async fn begin(&mut self, prompt: String, model: String) -> Result<()> {
        self.writer.begin(prompt, model).await?;
        self.active = true;
        self.journal = None;
        Ok(())
    }

    pub fn execution_journal(
        &mut self,
    ) -> Result<std::sync::Arc<dyn orbit_agent::checkpoint::ExecutionJournal>> {
        let journal = self.writer.execution_journal()?;
        self.journal = Some(journal.clone());
        Ok(journal)
    }

    pub async fn finish(&mut self, status: &str, history: Option<Vec<ChatMessage>>) -> Result<()> {
        if let Some(journal) = &self.journal {
            journal.flush().await?;
        }

        self.writer.finish(status, Vec::new(), history).await?;
        self.active = false;
        self.journal = None;
        self.snapshot = self.store.load(self.id()).await?;

        Ok(())
    }

    pub async fn close(&mut self) -> Result<()> {
        if self.active {
            self.finish("cancelled", None).await?;
        }

        Ok(())
    }

    pub async fn resumable(&self) -> Result<Vec<orbit_store::types::Session>> {
        let equivalent = self
            .store
            .equivalent_project_ids(&self.snapshot.session.project_id)
            .await?;

        Ok(self
            .store
            .list()
            .await?
            .into_iter()
            .filter(|saved| {
                equivalent.contains(&saved.project_id)
                    && !saved.demo
                    && saved.id != self.id()
                    && !saved.title.is_empty()
            })
            .collect())
    }

    #[cfg(test)]
    pub async fn resume(&mut self, id: &str) -> Result<()> {
        ensure!(!self.active, "请先取消当前响应，再恢复会话。");
        if id == self.id() {
            return Ok(());
        }

        let prepared = Self::prepare_resume(
            self.store.clone(),
            self.snapshot.session.project_id.clone(),
            id.to_owned(),
        )
        .await?;
        self.install_resume(prepared);

        Ok(())
    }

    /// Load under the destination lease without releasing or changing the
    /// current session. Safe to cancel before install_resume.
    pub async fn prepare_resume(
        store: Store,
        project: String,
        id: String,
    ) -> Result<PreparedResume> {
        let saved = store.session(&id).await?;
        let equivalent = store.equivalent_project_ids(&project).await?;
        ensure!(
            equivalent.contains(&saved.project_id),
            "该会话属于其他项目，请在对应目录恢复。"
        );
        ensure!(!saved.demo, "演示会话已不再受支持，请创建新会话。");

        let writer = store.acquire(&id).await?;
        let snapshot = store.load(&id).await?;
        let git_baseline = store.git_baseline(&id).await?;

        Ok(PreparedResume {
            writer,
            snapshot,
            git_baseline,
        })
    }

    #[cfg(test)]
    pub fn install_resume(&mut self, prepared: PreparedResume) {
        drop(self.swap_resume(prepared));
    }

    /// Replace the selected session while retaining the previous writer lease.
    /// A TUI tab can later restore that lease without replaying or reacquiring it.
    pub fn swap_resume(&mut self, prepared: PreparedResume) -> PreparedResume {
        let previous = PreparedResume {
            writer: std::mem::replace(&mut self.writer, prepared.writer),
            snapshot: std::mem::replace(&mut self.snapshot, prepared.snapshot),
            git_baseline: std::mem::replace(&mut self.git_baseline, prepared.git_baseline),
        };
        self.journal = None;

        if let Some(ws) = &self.audit_workspace {
            ws.clear_session_grants();
        }

        *self.scratch.write().expect("scratch lock poisoned") = self
            .store
            .path()
            .with_extension("workspaces")
            .join(self.id());
        *self
            .command_output
            .write()
            .expect("command output lock poisoned") = self
            .store
            .path()
            .with_extension("command-output")
            .join(self.id());

        previous
    }

    pub async fn prepare_new(&self, model: String) -> Result<PreparedResume> {
        ensure!(!self.active, "cannot switch sessions during a run");

        let writer = self
            .store
            .create(self.snapshot.session.workspace.clone(), model, false)
            .await?;
        let snapshot = self.store.load(writer.id()).await?;
        let git_baseline =
            orbit_tools::git_inspect::capture_baseline(Path::new(&snapshot.session.workspace));

        if let Some(baseline) = &git_baseline {
            self.store.save_git_baseline(writer.id(), baseline).await?;
        }

        Ok(PreparedResume {
            writer,
            snapshot,
            git_baseline,
        })
    }

    #[cfg(test)]
    pub async fn new_session(&mut self, model: String) -> Result<()> {
        let prepared = self.prepare_new(model).await?;
        self.install_resume(prepared);

        if let Some(workspace) = self.audit_workspace.clone() {
            self.initialize_audit(workspace, self.audit_limits.clone())
                .await?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn prepared_resume_is_atomic_and_releases_lease_when_cancelled() {
        let tmp = tempfile::tempdir().unwrap();
        let mut current = SessionState::open(tmp.path().join("sessions.db"), None, "model".into())
            .await
            .unwrap();
        let target = current.id().to_owned();
        current.new_session("model".into()).await.unwrap();
        let original = current.id().to_owned();
        let lease = current.store.acquire(&target).await.unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            SessionState::prepare_resume(
                current.store.clone(),
                current.snapshot.session.project_id.clone(),
                target.clone(),
            ),
        )
        .await
        .unwrap();
        let error = match result {
            Err(e) => e,
            Ok(_) => panic!("locked session was opened"),
        };

        assert!(error.to_string().contains("已在其他 Orbit"));
        assert_eq!(current.id(), original);

        drop(lease);
        let prepared = SessionState::prepare_resume(
            current.store.clone(),
            current.snapshot.session.project_id.clone(),
            target.clone(),
        )
        .await
        .unwrap();

        assert_eq!(current.id(), original);
        assert!(current.store.acquire(&target).await.is_err());

        drop(prepared);

        // Cancelling a prepared switch releases the destination without
        // releasing the writer for the conversation still visible in the UI.
        assert!(current.store.acquire(&target).await.is_ok());
        assert!(current.store.acquire(&original).await.is_err());
    }

    #[tokio::test]
    async fn replay_is_incremental_exact_and_preserves_scroll_and_saved_elapsed_time() {
        use orbit_model::types::{StreamEvent, ToolCall};

        let tmp = tempfile::tempdir().unwrap();
        let session = SessionState::open(tmp.path().join("sessions.db"), None, "model".into())
            .await
            .unwrap();
        let mut snapshot = session.snapshot.clone();
        snapshot.runs = (0..3)
            .map(|index| {
                let call = ToolCall::function(
                    format!("tool-{index}"),
                    "read_file",
                    serde_json::json!({ "path": "test" }),
                );
                let mut events = vec![AgentEvent::TurnStarted { turn: 1 }];
                events.extend(
                    (0..40).map(|_| AgentEvent::Model(StreamEvent::TextDelta("中文".into()))),
                );
                events.push(AgentEvent::ToolStarted { call: call.clone() });

                if index != 1 {
                    events.push(AgentEvent::ToolFinished {
                        call,
                        content: "result".into(),
                        changes: vec![],
                    });
                }

                orbit_store::types::Run {
                    prompt: format!("prompt-{index}"),
                    status: "completed".into(),
                    elapsed_ms: Some(12345),
                    events,
                }
            })
            .collect();
        let mut ui = UiState::new("model");
        ui.input.insert("draft");
        ui.scroll = 5;
        ui.follow_bottom = false;
        let mut replay = HistoryReplay::default();

        assert!(!replay.advance(&snapshot, &mut ui, 2, Duration::MAX));
        assert_eq!(ui.input.history.len(), 1);
        assert!(matches!(ui.transcript.last(), Some(TranscriptItem::Assistant(s)) if s == "中文"));

        let mut batches = 1;

        while !replay.advance(&snapshot, &mut ui, 7, Duration::MAX) {
            batches += 1;
        }

        assert!(batches > 10);
        assert_eq!(ui.input.history, vec!["prompt-0", "prompt-1", "prompt-2"]);
        assert_eq!(ui.input.text, "draft");
        assert_eq!(ui.scroll, 5);
        assert!(!ui.follow_bottom);
        assert_eq!(ui.elapsed, Duration::from_millis(12345));
        assert!(ui.started_at.is_none());

        let texts: Vec<_> = ui
            .transcript
            .iter()
            .filter_map(|item| {
                if let TranscriptItem::Assistant(s) = item {
                    Some(s)
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(texts.len(), 3);
        assert!(texts.iter().all(|s| **s == "中文".repeat(40)));

        let statuses: Vec<_> = ui
            .transcript
            .iter()
            .filter_map(|item| {
                if let TranscriptItem::Tool(t) = item {
                    Some(t.status.clone())
                } else {
                    None
                }
            })
            .collect();

        assert_eq!(
            statuses,
            vec![
                ToolStatus::Finished,
                ToolStatus::Failed,
                ToolStatus::Finished
            ]
        );

        let count = ui.transcript.len();

        assert!(replay.advance(&snapshot, &mut ui, 7, Duration::MAX));
        assert_eq!(ui.transcript.len(), count);
        assert!(
            session
                .store
                .load(session.id())
                .await
                .unwrap()
                .runs
                .is_empty()
        );
    }

    #[tokio::test]
    async fn imports_images_into_the_orbit_asset_directory_by_content_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let state = SessionState::open(tmp.path().join("sessions.db"), None, "model".into())
            .await
            .unwrap();
        let source = tmp.path().join("capture.png");
        image::RgbaImage::from_pixel(1, 1, image::Rgba([1, 2, 3, 255]))
            .save(&source)
            .unwrap();
        let first = state.import_image(&source).unwrap();
        let second = state.import_image(&source).unwrap();

        assert_eq!(first, second);

        let Attachment::Image {
            path, mime_type, ..
        } = first;

        assert_eq!(mime_type, "image/png");
        assert!(path.starts_with(tmp.path().join("sessions.attachments")));

        let clipboard = state
            .import_clipboard_image(1, 1, vec![10, 20, 30, 255])
            .unwrap();
        let Attachment::Image {
            name,
            path,
            mime_type,
        } = clipboard;

        assert!(name.starts_with("clipboard-") && name.ends_with(".png"));
        assert_eq!(mime_type, "image/png");
        assert!(path.starts_with(tmp.path().join("sessions.attachments")));
    }

    #[tokio::test]
    async fn restored_completed_run_keeps_its_persisted_elapsed_time() {
        let tmp = tempfile::tempdir().unwrap();
        let database = tmp.path().join("sessions.db");
        let mut state = SessionState::open(database.clone(), None, "model".into())
            .await
            .unwrap();
        let id = state.id().to_owned();
        state
            .begin("question".into(), "model".into())
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(5)).await;
        state
            .finish(
                "completed",
                Some(vec![
                    ChatMessage::user("question"),
                    ChatMessage::assistant("answer"),
                ]),
            )
            .await
            .unwrap();
        drop(state);

        let restored = SessionState::open(database, Some(id), "model".into())
            .await
            .unwrap();

        assert!(
            restored.snapshot.runs[0]
                .elapsed_ms
                .is_some_and(|ms| ms >= 5)
        );

        let mut ui = UiState::new("model");
        restored.restore_ui(&mut ui);

        assert!(ui.elapsed >= Duration::from_millis(5));
    }

    #[tokio::test]
    async fn resumed_session_keeps_its_selected_model_profile() {
        let tmp = tempfile::tempdir().unwrap();
        let database = tmp.path().join("sessions.db");
        let mut state = SessionState::open(database.clone(), None, "qwen::default".into())
            .await
            .unwrap();
        let id = state.id().to_owned();
        state
            .set_model_profile("deepseek::flash".into(), Some(ReasoningEffort::High))
            .await
            .unwrap();
        drop(state);

        let restored = SessionState::open(database, Some(id), "qwen::default".into())
            .await
            .unwrap();

        assert_eq!(
            restored.saved_model_profile(),
            ("deepseek::flash", Some(ReasoningEffort::High))
        );
    }

    #[tokio::test]
    async fn renamed_session_keeps_its_identity_and_title_after_resume() {
        let tmp = tempfile::tempdir().unwrap();
        let database = tmp.path().join("sessions.db");
        let mut state = SessionState::open(database.clone(), None, "model".into())
            .await
            .unwrap();
        let id = state.id().to_owned();
        state.rename("解析器重构".into()).await.unwrap();

        assert_eq!(state.snapshot.session.title, "解析器重构");

        drop(state);

        let restored = SessionState::open(database, Some(id.clone()), "model".into())
            .await
            .unwrap();

        assert_eq!(restored.id(), id);
        assert_eq!(restored.snapshot.session.title, "解析器重构");
    }

    #[tokio::test]
    async fn switching_restores_context_preserves_old_session_and_checks_scope_and_locks() {
        let tmp = tempfile::tempdir().unwrap();
        let mut state = SessionState::open(tmp.path().join("sessions.db"), None, "model".into())
            .await
            .unwrap();
        let first = state.id().to_owned();
        let scratch = state.scratch.clone();
        let first_scratch = scratch.read().unwrap().clone();
        let command_output = state.command_output.clone();
        let first_command_output = command_output.read().unwrap().clone();
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

        state.new_session("model".into()).await.unwrap();
        let second = state.id().to_owned();

        assert_ne!(first, second);
        assert_ne!(*scratch.read().unwrap(), first_scratch);
        assert_ne!(*command_output.read().unwrap(), first_command_output);
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
        let legacy_demo = state
            .store
            .create(
                state.snapshot.session.workspace.clone(),
                "model".into(),
                true,
            )
            .await
            .unwrap();

        assert!(state.resume(foreign.id()).await.is_err());
        assert!(state.resume(legacy_demo.id()).await.is_err());
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
        assert_eq!(*command_output.read().unwrap(), first_command_output);
        assert_eq!(state.snapshot.history, history);
        assert_eq!(state.snapshot.runs.len(), 1);
        assert!(state.store.load(&second).await.is_ok());
        // The old lease is released after a successful switch.
        assert!(state.store.acquire(&second).await.is_ok());

        drop(state);
        let reopened =
            SessionState::open(tmp.path().join("sessions.db"), Some(first), "model".into())
                .await
                .unwrap();

        assert_eq!(*reopened.scratch.read().unwrap(), first_scratch);
        assert_eq!(
            *reopened.command_output.read().unwrap(),
            first_command_output
        );
    }
    #[tokio::test]
    async fn calibration_restores_per_profile_without_cross_provider_contamination() {
        let tmp = tempfile::tempdir().unwrap();
        let mut session =
            SessionState::open(tmp.path().join("sessions.db"), None, "a::model".into())
                .await
                .unwrap();
        let id = session.id().to_owned();
        session
            .begin("hello".into(), "a::model".into())
            .await
            .unwrap();
        let journal = session.execution_journal().unwrap();

        for (profile, actual) in [("a::model", 200), ("b::model", 500)] {
            journal.record(AgentEvent::RequestMetrics {
                profile: profile.into(),
                estimated_input: 100,
                retries: Some(0),
                elapsed_ms: 10,
                usage: Some(orbit_model::types::Usage {
                    prompt_tokens: Some(actual),
                    completion_tokens: Some(1),
                    total_tokens: None,
                }),
            });
        }

        session
            .finish(
                "completed",
                Some(vec![
                    ChatMessage::user("hello"),
                    ChatMessage::assistant("hi"),
                ]),
            )
            .await
            .unwrap();
        drop(session);
        let session =
            SessionState::open(tmp.path().join("sessions.db"), Some(id), "a::model".into())
                .await
                .unwrap();

        assert_eq!(session.calibration("a::model").estimate(100), 200);
        assert_eq!(session.calibration("b::model").estimate(100), 500);

        let mut ui = UiState::new("a::model");
        session.restore_ui(&mut ui);

        assert_eq!(ui.calibration().estimate(100), 200);

        ui.model = "b::model".into();

        assert_eq!(ui.calibration().estimate(100), 500);

        ui.model = "new::model".into();

        assert_eq!(ui.calibration().estimate(100), 100);
    }

    #[tokio::test]
    async fn test_session_switch_and_resume_clears_session_grants() {
        let tmp = tempfile::tempdir().unwrap();
        let ws_dir = tempfile::tempdir().unwrap();
        let workspace = orbit_runtime::filesystem::Workspace::new(ws_dir.path()).unwrap();
        let mut session =
            SessionState::open(tmp.path().join("sessions.db"), None, "a::model".into())
                .await
                .unwrap();
        session.audit_workspace = Some(workspace.clone());

        // 1. Add ephemeral grant
        let outside = tempfile::tempdir().unwrap();
        let file = outside.path().join("file.txt");
        std::fs::write(&file, "data").unwrap();
        let res = orbit_runtime::filesystem::ResolvedPath::resolve(&file, None).unwrap();
        workspace
            .policy()
            .record_grant(orbit_runtime::policy::FilesystemGrant::read_only(
                orbit_runtime::capability::EnvironmentId::Local,
                res.clone(),
            ));

        assert_eq!(
            workspace
                .policy()
                .session_grants()
                .read()
                .unwrap()
                .grants()
                .len(),
            1
        );

        // 2. Switching to a new session clears ephemeral grants
        session.new_session("a::model".into()).await.unwrap();

        assert_eq!(
            workspace
                .policy()
                .session_grants()
                .read()
                .unwrap()
                .grants()
                .len(),
            0
        );

        // 3. Add grant again and verify resume clears it
        workspace
            .policy()
            .record_grant(orbit_runtime::policy::FilesystemGrant::read_only(
                orbit_runtime::capability::EnvironmentId::Local,
                res,
            ));

        assert_eq!(
            workspace
                .policy()
                .session_grants()
                .read()
                .unwrap()
                .grants()
                .len(),
            1
        );

        let writer2 = session
            .store
            .create(
                session.snapshot.session.workspace.clone(),
                "a::model".into(),
                false,
            )
            .await
            .unwrap();
        let second_id = writer2.id().to_owned();
        drop(writer2);

        let prepared = SessionState::prepare_resume(
            session.store.clone(),
            session.snapshot.session.project_id.clone(),
            second_id,
        )
        .await
        .unwrap();

        session.install_resume(prepared);

        assert_eq!(
            workspace
                .policy()
                .session_grants()
                .read()
                .unwrap()
                .grants()
                .len(),
            0
        );
    }

    #[tokio::test]
    async fn test_resume_preserves_attachments_losslessly_and_projects_for_text_only_model() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("orbit.db");
        let mut session = SessionState::open(db_path.clone(), None, "test::model".into())
            .await
            .unwrap();

        session
            .begin("hi".into(), "test::model".into())
            .await
            .unwrap();
        session
            .finish(
                "completed",
                Some(vec![
                    ChatMessage::user("hi"),
                    ChatMessage::user_with_attachments(
                        "check this",
                        vec![orbit_model::types::Attachment::Image {
                            name: "pic.png".into(),
                            mime_type: "image/png".into(),
                            path: "/path/to/pic.png".into(),
                        }],
                    ),
                ]),
            )
            .await
            .unwrap();

        let id = session.id().to_owned();
        drop(session);

        // Resume session: canonical history remains lossless
        let resumed = SessionState::open(db_path, Some(id), "test::model".into())
            .await
            .unwrap();

        assert_eq!(resumed.snapshot.history.len(), 2);

        let ChatMessage::User {
            content,
            attachments,
        } = &resumed.snapshot.history[1]
        else {
            panic!("expected user message");
        };

        assert_eq!(
            attachments.len(),
            1,
            "persisted history must retain attachments losslessly"
        );
        assert_eq!(content, "check this");

        // When projected for text-only model via model_messages, attachments are omitted
        let projected = orbit_agent::model_messages(&resumed.snapshot.history, false);
        let ChatMessage::User {
            content: proj_content,
            attachments: proj_attachments,
        } = &projected[1]
        else {
            panic!("expected projected user message");
        };

        assert!(
            proj_attachments.is_empty(),
            "projected request attachments must be emptied"
        );
        assert!(
            proj_content
                .contains("[Image omitted: current model does not support image input: pic.png]")
        );
    }

    #[tokio::test]
    async fn test_full_roundtrip_vision_to_text_to_vision_persists_attachments_losslessly() {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("orbit.db");

        // Round 1: Vision model session with image attachment
        let mut session = SessionState::open(db_path.clone(), None, "test::vision-model".into())
            .await
            .unwrap();
        session
            .begin("look at image".into(), "test::vision-model".into())
            .await
            .unwrap();
        let history = vec![
            ChatMessage::user("look at image"),
            ChatMessage::user_with_attachments(
                "analyze this",
                vec![orbit_model::types::Attachment::Image {
                    name: "photo.png".into(),
                    mime_type: "image/png".into(),
                    path: "/path/to/photo.png".into(),
                }],
            ),
            ChatMessage::assistant("I see a mountain in photo.png"),
        ];
        session.finish("completed", Some(history)).await.unwrap();
        let id = session.id().to_owned();
        drop(session);

        // Round 2: Resume with text-only model (supports_vision: false)
        let mut session_text =
            SessionState::open(db_path.clone(), Some(id.clone()), "test::text-model".into())
                .await
                .unwrap();
        // Model messages projected for text-only model omits image
        let projected = orbit_agent::model_messages(&session_text.snapshot.history, false);

        assert_eq!(projected.len(), 3);

        let ChatMessage::User {
            attachments,
            content,
        } = &projected[1]
        else {
            panic!()
        };

        assert!(attachments.is_empty());
        assert!(
            content
                .contains("[Image omitted: current model does not support image input: photo.png]")
        );

        // User sends a follow-up text message on text-only model and finishes
        session_text
            .begin("what was that mountain?".into(), "test::text-model".into())
            .await
            .unwrap();
        let mut history_turn2 = session_text.snapshot.history.clone();
        history_turn2.push(ChatMessage::user("what was that mountain?"));
        history_turn2.push(ChatMessage::assistant("It was Mount Fuji."));
        session_text
            .finish("completed", Some(history_turn2))
            .await
            .unwrap();
        drop(session_text);

        // Verify that in the database, the original image attachment was NOT lost!
        let session_vision = SessionState::open(
            db_path.clone(),
            Some(id.clone()),
            "test::vision-model".into(),
        )
        .await
        .unwrap();

        assert_eq!(session_vision.snapshot.history.len(), 5);

        let ChatMessage::User {
            attachments: final_attachments,
            ..
        } = &session_vision.snapshot.history[1]
        else {
            panic!()
        };

        assert_eq!(
            final_attachments.len(),
            1,
            "persisted history must still retain image attachment"
        );

        // Round 3: Projected for vision model (supports_vision: true)
        let projected_vision = orbit_agent::model_messages(&session_vision.snapshot.history, true);
        let ChatMessage::User {
            attachments: vis_attachments,
            ..
        } = &projected_vision[1]
        else {
            panic!()
        };

        assert_eq!(
            vis_attachments.len(),
            1,
            "vision projection must include the raw image attachment"
        );
    }

    #[tokio::test]
    #[ignore = "requires ORBIT_RECOVERY_DB and ORBIT_RECOVERY_SESSION"]
    async fn test_real_session_recovery_validation() {
        let real_db = match std::env::var("ORBIT_RECOVERY_DB") {
            Ok(v) if !v.is_empty() => std::path::PathBuf::from(v),
            _ => return,
        };

        let session_id = match std::env::var("ORBIT_RECOVERY_SESSION") {
            Ok(v) if !v.is_empty() => v,
            _ => return,
        };

        if !real_db.exists() {
            return;
        }

        let tmp = tempfile::tempdir().unwrap();
        let copy_db = tmp.path().join("orbit_copy.db");
        std::fs::copy(&real_db, &copy_db).unwrap();

        for suffix in ["-wal", "-shm", "-journal"] {
            let mut src = real_db.as_os_str().to_os_string();
            src.push(suffix);
            let src = std::path::PathBuf::from(src);

            if src.exists() {
                let mut dst = copy_db.as_os_str().to_os_string();
                dst.push(suffix);
                let _ = std::fs::copy(&src, std::path::PathBuf::from(dst));
            }
        }

        let store = orbit_store::store::Store::open(copy_db).await.unwrap();
        let saved = store.session(&session_id).await.unwrap();
        let prepared =
            SessionState::prepare_resume(store.clone(), saved.project_id.clone(), session_id)
                .await
                .unwrap();

        // Check history loaded without error
        assert!(!prepared.snapshot.history.is_empty());

        // 1. Project messages for text-only model: no image attachments
        let projected_text = orbit_agent::model_messages(&prepared.snapshot.history, false);

        for msg in &projected_text {
            if let ChatMessage::User { attachments, .. } = msg {
                assert!(
                    attachments.is_empty(),
                    "text-only model must receive 0 attachments"
                );
            }
        }

        // 2. Project messages for vision model: history remains valid
        let projected_vision = orbit_agent::model_messages(&prepared.snapshot.history, true);

        assert_eq!(projected_vision.len(), prepared.snapshot.history.len());
    }
}
