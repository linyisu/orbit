use std::{collections::BTreeMap, path::Path, sync::Arc};

use anyhow::{Result, ensure};
use orbit_tools::approval::ApprovalPreview;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, mpsc, oneshot};

use super::protocol::{Approval, Mode, PAGE_BYTES, PAGE_EVENTS, Page, SessionInfo, Update};

mod actor;
mod progress;
mod replay;

const COMMAND_CAPACITY: usize = 16;

pub struct Handle {
    pub(super) questions: super::questions::Questions,
    _configuration: Option<super::configuration::Installed>,
    pub info: SessionInfo,
    pub(super) clients: std::sync::atomic::AtomicUsize,
    pub policy: orbit_protocol::SessionPolicy,
    pub view: Arc<Mutex<View>>,
    progress_delivery: Mutex<progress::Deliveries>,
    history: Vec<orbit_store::transcript::RunIndex>,
    requests: mpsc::Sender<Envelope>,
    pub workspace: Arc<std::sync::RwLock<orbit_runtime::filesystem::Workspace>>,
    pub store: orbit_store::store::Store,
}

pub enum Command {
    UsageContext {
        result: oneshot::Sender<(orbit_model::types::ModelConfig, bool)>,
    },
    RetireUnobserved {
        result: oneshot::Sender<bool>,
    },
    Plan {
        action: orbit_protocol::plan::Action,
    },
    EnableUserInput,
    AnswerUserInput {
        id: String,
        answers: Option<Vec<orbit_protocol::user_input::Answer>>,
    },
    Activity {
        result: oneshot::Sender<bool>,
    },
    ConfigureModels {
        configuration: Option<orbit_protocol::Configuration>,
    },
    SendSubagent {
        agent: String,
        message: String,
    },
    SetMode {
        mode: Mode,
    },
    Close,
    Action {
        action: orbit_protocol::SessionAction,
        result: oneshot::Sender<Result<orbit_protocol::Report>>,
    },
    Start {
        request_id: String,
        prompt: String,
        images: Vec<orbit_protocol::ImageUpload>,
        compact: bool,
        model: Option<String>,
    },
    SetModel {
        model: String,
        effort: Option<String>,
    },
    Steer {
        message: String,
        accepted: oneshot::Sender<i64>,
    },
    Undo {
        path: Option<String>,
    },
    Cancel,
    Approve {
        id: u64,
        allow: bool,
        remember: bool,
    },
}

struct Envelope {
    command: Command,
    reply: oneshot::Sender<Result<()>>,
}

pub struct View {
    updates: replay::Log,
    published: usize,
    profile: Option<SessionInfo>,
    images: std::collections::BTreeSet<String>,
    pub busy: bool,
    pub model: String,
    approvals: BTreeMap<u64, ApprovalPreview>,
    clock: Option<Clock>,
    progress: Vec<orbit_protocol::ToolProgress>,
    user_inputs: Vec<orbit_protocol::user_input::Record>,
    planning: orbit_protocol::plan::State,
}

struct Clock {
    started: std::time::Instant,
    paused: Option<std::time::Instant>,
    pauses: std::time::Duration,
}

impl Clock {
    fn new() -> Self {
        Self {
            started: std::time::Instant::now(),
            paused: None,
            pauses: std::time::Duration::ZERO,
        }
    }
    fn pause(&mut self) {
        self.paused.get_or_insert_with(std::time::Instant::now);
    }
    fn resume(&mut self) {
        if let Some(since) = self.paused.take() {
            self.pauses += since.elapsed();
        }
    }
    fn snapshot(&self) -> orbit_protocol::RunTiming {
        let elapsed = self
            .paused
            .unwrap_or_else(std::time::Instant::now)
            .duration_since(self.started)
            .saturating_sub(self.pauses);
        orbit_protocol::RunTiming {
            elapsed_ms: elapsed.as_millis().min(u64::MAX as u128) as u64,
            paused: self.paused.is_some(),
        }
    }
}

#[cfg(test)]
fn presentation(run: &orbit_store::types::Run) -> orbit_protocol::RunPresentation {
    orbit_protocol::RunPresentation {
        replayed: true,
        images: run.images.iter().map(|image| image.name.clone()).collect(),
        image_assets: run.images.clone(),
        compact: run.prompt == "/compact",
        queued_input: run
            .events
            .iter()
            .take_while(|event| {
                !matches!(event, orbit_agent::events::AgentEvent::TurnStarted { .. })
            })
            .any(|event| matches!(event, orbit_agent::events::AgentEvent::InputReceived { .. })),
    }
}

impl View {
    fn push(&mut self, update: Update) {
        if let Update::Profile(info) = &update {
            self.profile = Some((**info).clone());
        }
        if let Update::Started { presentation, .. } = &update {
            // Uploads are visible before their first execution checkpoint is
            // flushed. Retain this run's ownership through that small window.
            self.images = presentation
                .image_assets
                .iter()
                .map(|image| image.path.clone())
                .collect();
        }
        self.updates.push(update, self.published);
    }

    #[cfg(test)]
    pub fn page(&mut self, after: usize) -> Result<Page> {
        let updates = self.updates.page(after)?;
        self.published = self.published.max(after + updates.len());
        Ok(self.page_state(after, updates, self.updates.len()))
    }

    fn page_state(&self, after: usize, updates: Vec<Update>, total: usize) -> Page {
        let mut progress_bytes = 0;
        let progress: Vec<_> = self
            .progress
            .iter()
            .take_while(|item| {
                let size = serde_json::to_vec(item).map_or(PAGE_BYTES, |bytes| bytes.len());
                if progress_bytes + size > PAGE_BYTES / 2 {
                    return false;
                }
                progress_bytes += size;
                true
            })
            .cloned()
            .collect();
        let next = after + updates.len();
        Page {
            planning: self.planning.clone(),
            user_inputs: self.user_inputs.clone(),
            tool_progress: if next == total { progress } else { Vec::new() },
            timing: self.clock.as_ref().map(Clock::snapshot),
            next,
            more: next < total,
            busy: self.busy,
            model: self.model.clone(),
            updates,
            approvals: self
                .approvals
                .iter()
                .map(|(id, preview)| Approval {
                    id: *id,
                    preview: preview.clone(),
                })
                .collect(),
        }
    }
}

impl Handle {
    pub async fn page(&self, viewer: u64, after: usize) -> Result<Page> {
        let prefix = self.history.last().map_or(0, |run| run.end());
        let updates = if after < prefix {
            history_page(&self.store, &self.info.id, &self.history, after).await?
        } else {
            let reader = {
                let mut view = self.view.lock().await;
                let reader = view.updates.reader(after - prefix)?;
                // Freeze even a partially returned page before releasing the
                // lock. A concurrent append cannot change its last fragment.
                view.published = view.published.max(view.updates.len());
                reader
            };
            tokio::task::spawn_blocking(move || reader.read(PAGE_BYTES / 2)).await??
        };
        let view = self.view.lock().await;
        let mut page = view.page_state(after, updates, prefix + view.updates.len());
        drop(view);
        let mut delivery = self.progress_delivery.lock().await;
        delivery.reset_started_tools(viewer, &page.updates);
        delivery.filter(viewer, &mut page.tool_progress);
        Ok(page)
    }

    pub async fn release_viewer(&self, viewer: u64) {
        self.questions.release(viewer).await;
        self.reset_progress_delivery(viewer).await;
    }

    pub(super) async fn reset_progress_delivery(&self, viewer: u64) {
        self.progress_delivery.lock().await.forget(viewer);
    }

    pub async fn running(&self) -> bool {
        let (result, response) = oneshot::channel();
        if self.command(Command::Activity { result }).await.is_err() {
            return false;
        }
        response.await.unwrap_or(false)
    }

    pub async fn subagent_page(&self, agent: &str, after: usize) -> Result<Page> {
        self.authorize_subagent(agent).await?;
        let history = self.store.transcript_index(agent).await?;
        let total = history.last().map_or(0, |run| run.end());
        ensure!(
            after <= total,
            "event cursor no longer exists; reopen the session"
        );
        let updates = history_page(&self.store, agent, &history, after).await?;
        let view = View {
            updates: Default::default(),
            published: 0,
            profile: None,
            images: Default::default(),
            busy: history.last().is_some_and(|run| !run.finished),
            model: self.store.session(agent).await?.model,
            approvals: BTreeMap::new(),
            clock: None,
            progress: Vec::new(),
            user_inputs: Vec::new(),
            planning: Default::default(),
        };
        Ok(view.page_state(after, updates, total))
    }

    pub async fn authorize_subagent(&self, agent: &str) -> Result<()> {
        ensure!(
            self.store.subagent_parent(agent).await?.as_deref() == Some(&self.info.id),
            "该记录不是当前会话的子代理。"
        );
        Ok(())
    }

    pub async fn spawn(
        setup: crate::PreparedWorkspace,
        configuration: Option<super::configuration::Installed>,
    ) -> Result<Self> {
        for diagnostic in &setup.resources.diagnostics {
            eprintln!("{diagnostic}");
        }
        let info = super::profile::describe(&setup);
        let policy = setup.resources.policy.clone();
        let history = if setup.session.is_persisted() {
            setup
                .session
                .store
                .transcript_index(setup.session.id())
                .await?
        } else {
            Vec::new()
        };
        let mut view = View {
            updates: Default::default(),
            published: 0,
            profile: None,
            images: Default::default(),
            busy: false,
            model: info.model.clone(),
            approvals: BTreeMap::new(),
            clock: None,
            progress: Vec::new(),
            user_inputs: Vec::new(),
            planning: Default::default(),
        };
        view.push(Update::Profile(Box::new(info.clone())));
        let view = Arc::new(Mutex::new(view));
        let (requests, envelopes) = mpsc::channel::<Envelope>(COMMAND_CAPACITY);
        let workspace = Arc::new(std::sync::RwLock::new(setup.resources.workspace.clone()));
        let store = setup.session.store.clone();
        let questions = super::questions::Questions::new(&setup);
        tokio::spawn(actor::run(
            setup,
            view.clone(),
            questions.clone(),
            workspace.clone(),
            envelopes,
        ));
        Ok(Self {
            questions,
            _configuration: configuration,
            info,
            clients: std::sync::atomic::AtomicUsize::new(0),
            policy,
            view,
            progress_delivery: Default::default(),
            history,
            requests,
            workspace,
            store,
        })
    }

    /// Only expose attachments published in this session's transcript.
    pub async fn has_image(&self, path: &str) -> bool {
        if self.view.lock().await.images.contains(path) {
            return true;
        }
        self.store
            .session_has_attachment(&self.info.id, path)
            .await
            .unwrap_or(false)
    }

    pub async fn current_info(&self) -> SessionInfo {
        self.view
            .lock()
            .await
            .profile
            .clone()
            .unwrap_or_else(|| self.info.clone())
    }

    pub async fn action(
        &self,
        action: orbit_protocol::SessionAction,
    ) -> Result<orbit_protocol::Report> {
        let (result, response) = oneshot::channel();
        self.command(Command::Action { action, result }).await?;
        response.await?
    }

    pub async fn usage_context(&self) -> Result<(orbit_model::types::ModelConfig, bool)> {
        let (result, response) = oneshot::channel();
        self.command(Command::UsageContext { result }).await?;
        Ok(response.await?)
    }

    pub async fn command(&self, command: Command) -> Result<()> {
        let (reply, response) = oneshot::channel();
        self.requests
            .send(Envelope { command, reply })
            .await
            .map_err(|_| anyhow::anyhow!("workspace session stopped"))?;
        response.await?
    }
}

async fn history_page(
    store: &orbit_store::store::Store,
    session: &str,
    history: &[orbit_store::transcript::RunIndex],
    after: usize,
) -> Result<Vec<Update>> {
    let mut updates = Vec::new();
    let mut bytes = 0;
    for run in &history[history.partition_point(|run| run.end() <= after)..] {
        let position = after + updates.len();
        let page = store
            .transcript_run_page(session, run, position, PAGE_EVENTS - updates.len())
            .await?;
        for update in page {
            let size = serde_json::to_vec(&update)?.len();
            if bytes > 0 && bytes + size > PAGE_BYTES / 2 {
                return Ok(updates);
            }
            bytes += size;
            updates.push(update);
        }
        if updates.len() == PAGE_EVENTS || after + updates.len() < run.end() {
            break;
        }
    }
    Ok(updates)
}

fn approval_key(preview: &ApprovalPreview, workspace: &Path) -> Option<String> {
    let target = match preview {
        ApprovalPreview::Write(preview) => {
            return orbit_tools::approval::file_grant_key(&preview.path, workspace);
        }
        ApprovalPreview::Delete(preview) => {
            return orbit_tools::approval::file_grant_key(&preview.path, workspace);
        }
        ApprovalPreview::Command(preview) if preview.git.is_some() => {
            preview.git_grant_key(workspace)?
        }
        ApprovalPreview::Command(preview) => preview.request_grant_key(workspace),
        ApprovalPreview::Mcp(_) | ApprovalPreview::Capability(_) => return None,
    };
    Some(format!("{target}:{}", serde_json::to_string(preview).ok()?))
}

fn request_fingerprint(
    prompt: &str,
    images: &[orbit_protocol::ImageUpload],
    compact: bool,
    model: &Option<String>,
) -> String {
    let bytes = serde_json::to_vec(&(prompt, images, compact, model))
        .expect("serializable workspace request");
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests;
