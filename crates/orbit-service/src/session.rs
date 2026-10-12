mod title;

use std::{path::Path, path::PathBuf};

use anyhow::{Context, Result, ensure};
use orbit_model::types::Attachment;
use orbit_model::types::ChatMessage;
use orbit_model::types::ReasoningEffort;
use orbit_store::{
    store::{SessionWriter, Store},
    types::{RuntimeSnapshot, Snapshot},
};

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
    let rows = store.list_resumable().await?;

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

/// A peer changed the session after the UI's last snapshot. Refresh and retry
/// before creating a run; this is not a user-facing failure.
#[derive(Debug)]
pub struct SessionRefreshRequired;

impl std::fmt::Display for SessionRefreshRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("session snapshot requires refresh")
    }
}

impl std::error::Error for SessionRefreshRequired {}

pub struct SessionState {
    pub store: Store,
    writer: Option<SessionWriter>,
    persisted: bool,
    pub snapshot: RuntimeSnapshot,
    pub command_output: orbit_runtime::execution::output::CommandOutputs,
    output_run: Option<orbit_runtime::execution::output::OutputRun>,
    pub git_baseline: Option<serde_json::Value>,
    pub audit_workspace: Option<orbit_runtime::filesystem::Workspace>,
    pub audit_limits: orbit_tools::run_command::CommandLimits,
    title_task: Option<crate::session::title::TitleTask>,
    active: bool,
    shared: bool,
    journal: Option<std::sync::Arc<dyn orbit_agent::checkpoint::ExecutionJournal>>,
}

async fn apply_title(
    store: &Store,
    task: &mut Option<crate::session::title::TitleTask>,
    writer: &Option<SessionWriter>,
    snapshot: &mut Snapshot,
) -> Result<bool> {
    if !task.as_ref().is_some_and(|task| task.is_finished()) {
        return Ok(false);
    }
    let temporary;
    let writer = match writer {
        Some(writer) => writer,
        None => {
            temporary = store.acquire(&snapshot.session.id).await?;
            &temporary
        }
    };
    let Some(title) = task.take().unwrap().result().await else {
        return Ok(false);
    };
    writer.set_title(title.clone()).await?;
    snapshot.session.title = title;
    Ok(true)
}

pub struct PreparedResume {
    store: Store,
    title_task: Option<crate::session::title::TitleTask>,
    writer: Option<SessionWriter>,
    persisted: bool,
    pub snapshot: RuntimeSnapshot,
    git_baseline: Option<serde_json::Value>,
}

#[cfg(test)]
impl PreparedResume {
    pub async fn poll_title(&mut self) -> Result<bool> {
        apply_title(
            &self.store,
            &mut self.title_task,
            &self.writer,
            &mut self.snapshot,
        )
        .await
    }
}

impl SessionState {
    pub(crate) fn metadata_pending(&self) -> bool {
        self.title_task.is_some()
    }

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
        Self::open_mode(path, resume, model, directory, false).await
    }

    pub async fn open_shared_in(
        path: PathBuf,
        resume: Option<String>,
        model: String,
        directory: &Path,
    ) -> Result<Self> {
        Self::open_mode(path, resume, model, directory, true).await
    }

    async fn open_mode(
        path: PathBuf,
        resume: Option<String>,
        model: String,
        directory: &Path,
        shared: bool,
    ) -> Result<Self> {
        let store = Store::open(path).await?;
        let workspace = directory.canonicalize()?.to_string_lossy().into_owned();
        let persisted = resume.is_some();
        let (snapshot, writer, git_baseline) = if let Some(id) = resume {
            let project = store.resolve_project(&workspace).await?;
            let saved = store.session(&id).await?;
            let equivalent = store.equivalent_project_ids(&project.id).await?;
            ensure!(
                equivalent.contains(&saved.project_id),
                "session belongs to workspace {}",
                saved.workspace
            );
            let writer = if shared {
                None
            } else {
                Some(store.acquire(&id).await?)
            };
            (
                store.load_runtime(&id).await?,
                writer,
                store.git_baseline(&id).await?,
            )
        } else {
            let session = store.draft(workspace, model).await?;
            (
                Snapshot {
                    session,
                    history: Vec::new(),
                    runs: Vec::new(),
                    recovery: None,
                }
                .into(),
                None,
                None,
            )
        };
        Ok(Self::from_prepared(
            PreparedResume {
                store,
                writer,
                persisted,
                snapshot,
                git_baseline,
                title_task: None,
            },
            shared,
        ))
    }

    fn from_prepared(prepared: PreparedResume, shared: bool) -> Self {
        let PreparedResume {
            store,
            writer,
            persisted,
            snapshot,
            git_baseline,
            title_task,
        } = prepared;
        Self {
            command_output: Default::default(),
            output_run: None,
            git_baseline,
            store,
            writer,
            persisted,
            snapshot,
            audit_workspace: None,
            audit_limits: Default::default(),
            title_task,
            active: false,
            shared,
            journal: None,
        }
    }

    pub async fn move_to(
        &mut self,
        workspace: orbit_runtime::filesystem::Workspace,
        limits: orbit_tools::run_command::CommandLimits,
    ) -> Result<()> {
        ensure!(!self.active, "请先结束当前响应再移动会话");
        if !self.persisted {
            let project = self.store.project_at(workspace.root()).await?;
            self.snapshot.session.workspace = workspace.root().to_string_lossy().into_owned();
            self.snapshot.session.project_id = project.map(|p| p.id).unwrap_or_default();
            self.audit_workspace = Some(workspace);
            self.audit_limits = limits;
            return Ok(());
        }
        let baseline = orbit_tools::git_inspect::capture_baseline(workspace.root());
        self.ensure_writer().await?;
        let session = self
            .writer
            .as_ref()
            .unwrap()
            .move_to(workspace.root(), baseline.clone())
            .await;

        self.release_idle_writer();
        self.snapshot.session = session?;
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
        if self.persisted
            && self.snapshot.runs.is_empty()
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
        self.snapshot
            .calibration
            .get(profile)
            .cloned()
            .unwrap_or_default()
    }

    pub fn id(&self) -> &str {
        &self.snapshot.session.id
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
        if !self.persisted {
            self.snapshot.session.model = model;
            self.snapshot.session.reasoning_effort =
                reasoning_effort.map(|value| value.as_str().to_owned());
            return Ok(());
        }
        self.ensure_writer().await?;
        let result = self
            .writer
            .as_ref()
            .unwrap()
            .set_model_profile(
                model.clone(),
                reasoning_effort
                    .map(ReasoningEffort::as_str)
                    .map(str::to_owned),
            )
            .await;

        self.release_idle_writer();
        result?;
        self.snapshot.session.model = model;
        self.snapshot.session.reasoning_effort = reasoning_effort
            .map(ReasoningEffort::as_str)
            .map(str::to_owned);

        Ok(())
    }

    pub async fn append_context(&mut self, message: ChatMessage) -> Result<()> {
        if !self.persisted {
            self.snapshot.history.push(message);
            return Ok(());
        }
        self.ensure_writer().await?;
        let result = self.writer.as_ref().unwrap().append_context(message).await;
        self.release_idle_writer();
        result?;
        self.store.refresh_runtime(&mut self.snapshot).await?;
        Ok(())
    }

    pub async fn rename(&mut self, title: String) -> Result<()> {
        if !self.persisted {
            self.title_task = None;
            self.snapshot.session.title = title;
            return Ok(());
        }
        self.ensure_writer().await?;
        let result = self.writer.as_ref().unwrap().set_title(title.clone()).await;
        self.release_idle_writer();
        result?;
        self.title_task = None;
        self.snapshot.session.title = title;
        Ok(())
    }

    pub async fn begin_with_title(
        &mut self,
        prompt: String,
        model: String,
        client: &orbit_model::client::ModelClient,
    ) -> Result<()> {
        let generate = prompt != "/compact"
            && self.snapshot.runs.is_empty()
            && self.snapshot.session.title.is_empty()
            && self.title_task.is_none();
        self.begin(prompt.clone(), model).await?;
        if generate {
            self.title_task = Some(crate::session::title::TitleTask::start(client, &prompt));
        }
        Ok(())
    }

    pub async fn poll_title(&mut self) -> Result<bool> {
        apply_title(
            &self.store,
            &mut self.title_task,
            &self.writer,
            &mut self.snapshot,
        )
        .await
    }

    pub async fn begin(&mut self, prompt: String, model: String) -> Result<()> {
        self.persist().await?;
        self.ensure_writer().await?;
        let matches = match self.store.matches_snapshot(&self.snapshot).await {
            Ok(latest) => latest,
            Err(error) => {
                self.release_idle_writer();
                return Err(error);
            }
        };

        if !matches
            || self.audit_workspace.as_ref().is_some_and(|workspace| {
                workspace.root() != Path::new(&self.snapshot.session.workspace)
            })
        {
            self.release_idle_writer();
            return Err(SessionRefreshRequired.into());
        }

        let output_run = self.command_output.begin_run()?;
        if let Err(error) = self.writer.as_mut().unwrap().begin(prompt, model).await {
            self.release_idle_writer();
            return Err(error);
        }

        self.active = true;
        self.output_run = Some(output_run);
        self.journal = None;
        Ok(())
    }

    pub fn execution_journal(
        &mut self,
    ) -> Result<std::sync::Arc<dyn orbit_agent::checkpoint::ExecutionJournal>> {
        let journal = self
            .writer
            .as_ref()
            .context("no active writer")?
            .execution_journal()?;

        self.journal = Some(journal.clone());
        Ok(journal)
    }

    pub async fn finish(&mut self, status: &str, history: Option<Vec<ChatMessage>>) -> Result<()> {
        // The executor has stopped. A later persistence error must not extend
        // the lifetime of temporary raw logs or expose them to a future task.
        self.output_run = None;
        if let Some(journal) = &self.journal {
            journal.flush().await?;
        }

        self.writer
            .as_mut()
            .context("no active writer")?
            .finish(status, Vec::new(), history)
            .await?;
        self.active = false;
        self.journal = None;
        self.store.refresh_runtime(&mut self.snapshot).await?;
        self.release_idle_writer();

        Ok(())
    }

    pub async fn close(&mut self) -> Result<()> {
        if self.active {
            self.finish("cancelled", None).await?;
        }

        Ok(())
    }

    pub async fn resumable(&self) -> Result<Vec<orbit_store::types::Session>> {
        let Some(project) = self
            .store
            .project_at(&self.snapshot.session.workspace)
            .await?
        else {
            return Ok(Vec::new());
        };
        let equivalent = self.store.equivalent_project_ids(&project.id).await?;

        Ok(self
            .store
            .list_resumable()
            .await?
            .into_iter()
            .filter(|saved| {
                equivalent.contains(&saved.project_id)
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
    #[cfg(test)]
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

        let writer = store.acquire(&id).await?;
        let snapshot = store.load_runtime(&id).await?;
        let git_baseline = store.git_baseline(&id).await?;

        Ok(PreparedResume {
            store: store.clone(),
            title_task: None,
            writer: Some(writer),
            persisted: true,
            snapshot,
            git_baseline,
        })
    }

    #[cfg(test)]
    pub fn install_resume(&mut self, prepared: PreparedResume) {
        drop(self.swap_resume(prepared));
    }

    pub async fn prepare_shared(
        store: Store,
        _project: String,
        id: String,
    ) -> Result<PreparedResume> {
        let snapshot = store.load_runtime(&id).await?;
        let git_baseline = store.git_baseline(&id).await?;
        Ok(PreparedResume {
            store,
            title_task: None,
            writer: None,
            persisted: true,
            snapshot,
            git_baseline,
        })
    }

    pub fn enable_shared(&mut self) {
        self.shared = true;
        self.release_idle_writer();
    }

    fn release_idle_writer(&mut self) {
        if self.shared && !self.active {
            self.writer = None;
        }
    }

    pub fn is_persisted(&self) -> bool {
        self.persisted
    }

    pub(crate) async fn persist(&mut self) -> Result<()> {
        if self.persisted {
            return Ok(());
        }
        self.writer = Some(
            self.store
                .persist_draft(&self.snapshot.session, &self.snapshot.history)
                .await?,
        );
        self.persisted = true;
        self.snapshot.session = self.store.session(self.id()).await?;
        self.git_baseline =
            orbit_tools::git_inspect::capture_baseline(Path::new(&self.snapshot.session.workspace));
        if let Some(baseline) = &self.git_baseline {
            self.store.save_git_baseline(self.id(), baseline).await?;
        }
        if let Some(workspace) = self.audit_workspace.clone() {
            self.initialize_audit(workspace, self.audit_limits.clone())
                .await?;
        }
        self.release_idle_writer();
        Ok(())
    }

    async fn ensure_writer(&mut self) -> Result<()> {
        ensure!(self.persisted, "会话尚未开始");
        if self.writer.is_none() {
            self.writer = Some(self.store.acquire(self.id()).await?);
        }
        Ok(())
    }

    pub fn active_run_id(&self) -> Option<i64> {
        self.writer.as_ref().and_then(SessionWriter::active_run_id)
    }

    pub(crate) async fn edit_queued_input(
        &mut self,
        expected_workspace: &str,
        id: i64,
        expected: &str,
        content: String,
    ) -> Result<()> {
        self.ensure_writer().await?;
        let result = self
            .writer
            .as_ref()
            .unwrap()
            .edit_queued_input(expected_workspace, id, expected, content)
            .await;
        self.release_idle_writer();
        result
    }

    pub(crate) async fn remove_queued_input(
        &mut self,
        expected_workspace: &str,
        id: i64,
        expected: &str,
    ) -> Result<()> {
        self.ensure_writer().await?;
        let result = self
            .writer
            .as_ref()
            .unwrap()
            .remove_queued_input(expected_workspace, id, expected)
            .await;
        self.release_idle_writer();
        result
    }

    pub async fn flush_events(&self) -> Result<()> {
        if let Some(journal) = &self.journal {
            journal.flush().await?;
        }
        Ok(())
    }

    /// Park the previous session's snapshot and title task. Shared TUI tabs hold
    /// no idle writer lease; exclusive callers retain their original lease.
    #[cfg(test)]
    pub fn swap_resume(&mut self, prepared: PreparedResume) -> PreparedResume {
        let previous = PreparedResume {
            store: self.store.clone(),
            title_task: std::mem::replace(&mut self.title_task, prepared.title_task),
            writer: std::mem::replace(&mut self.writer, prepared.writer),
            persisted: std::mem::replace(&mut self.persisted, prepared.persisted),
            snapshot: std::mem::replace(&mut self.snapshot, prepared.snapshot),
            git_baseline: std::mem::replace(&mut self.git_baseline, prepared.git_baseline),
        };

        self.journal = None;

        if let Some(ws) = &self.audit_workspace {
            ws.clear_session_grants();
        }

        self.output_run = None;

        previous
    }

    /// Exchange complete session ownership without rebinding a running task's paths or grants.
    pub fn replace_with(&mut self, prepared: PreparedResume) -> Self {
        let next = Self::from_prepared(prepared, self.shared);
        std::mem::replace(self, next)
    }

    pub async fn prepare_new_in(&self, model: String, directory: &Path) -> Result<PreparedResume> {
        let workspace = directory.canonicalize()?.to_string_lossy().into_owned();
        let session = self.store.draft(workspace, model).await?;
        Ok(PreparedResume {
            store: self.store.clone(),
            title_task: None,
            writer: None,
            persisted: false,
            snapshot: Snapshot {
                session,
                history: Vec::new(),
                runs: Vec::new(),
                recovery: None,
            }
            .into(),
            git_baseline: None,
        })
    }

    #[cfg(test)]
    pub async fn prepare_new(&self, model: String) -> Result<PreparedResume> {
        self.prepare_new_in(model, Path::new(&self.snapshot.session.workspace))
            .await
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
mod tests;
