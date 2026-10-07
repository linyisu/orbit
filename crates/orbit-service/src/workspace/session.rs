use std::{collections::BTreeMap, path::Path, sync::Arc};

use anyhow::{Result, ensure};
use orbit_tools::approval::ApprovalPreview;
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, mpsc, oneshot};

use super::protocol::{Approval, Mode, PAGE_BYTES, PAGE_EVENTS, Page, SessionInfo, Update};

mod actor;
mod progress;

const COMMAND_CAPACITY: usize = 16;

pub struct Handle {
    pub(super) questions: super::questions::Questions,
    _configuration: Option<super::configuration::Installed>,
    pub info: SessionInfo,
    pub policy: orbit_protocol::SessionPolicy,
    pub view: Arc<Mutex<View>>,
    requests: mpsc::Sender<Envelope>,
    pub workspace: Arc<std::sync::RwLock<orbit_runtime::filesystem::Workspace>>,
    pub store: orbit_store::store::Store,
}

pub enum Command {
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
        mode: Mode,
        model: Option<String>,
    },
    SetModel {
        model: String,
        effort: Option<String>,
    },
    Steer {
        message: String,
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
    updates: Vec<(Update, usize)>,
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
        let bytes = serde_json::to_vec(&update).map_or(PAGE_BYTES, |bytes| bytes.len());
        self.updates.push((update, bytes));
    }

    pub fn page(&self, after: usize) -> Result<Page> {
        ensure!(
            after <= self.updates.len(),
            "event cursor no longer exists; reopen the session"
        );
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
        let limit = PAGE_BYTES.saturating_sub(progress_bytes);
        let mut bytes = 0;
        let updates: Vec<_> = self.updates[after..]
            .iter()
            .take(PAGE_EVENTS)
            .take_while(|(_, size)| {
                if bytes > 0 && bytes + size > limit {
                    return false;
                }
                bytes += size;
                true
            })
            .map(|(update, _)| update.clone())
            .collect();
        let next = after + updates.len();
        Ok(Page {
            planning: self.planning.clone(),
            user_inputs: self.user_inputs.clone(),
            tool_progress: if next == self.updates.len() {
                progress
            } else {
                Vec::new()
            },
            timing: self.clock.as_ref().map(Clock::snapshot),
            next,
            more: next < self.updates.len(),
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
        })
    }
}

impl Handle {
    pub async fn running(&self) -> bool {
        let (result, response) = oneshot::channel();
        if self.command(Command::Activity { result }).await.is_err() {
            return false;
        }
        response.await.unwrap_or(false)
    }

    pub async fn subagent_page(&self, agent: &str, after: usize) -> Result<Page> {
        self.authorize_subagent(agent).await?;
        let snapshot = self.store.load(agent).await?;
        let mut view = View {
            updates: Vec::new(),
            busy: snapshot
                .runs
                .last()
                .is_some_and(|run| run.status == "running"),
            model: snapshot.session.model,
            approvals: BTreeMap::new(),
            clock: None,
            progress: Vec::new(),
            user_inputs: Vec::new(),
            planning: Default::default(),
        };
        for run in snapshot.runs {
            view.push(Update::Started {
                presentation: presentation(&run),
                prompt: run.prompt,
            });
            for event in run.events {
                view.push(Update::agent(event));
            }
            if run.status != "running" {
                view.push(Update::Finished {
                    elapsed_ms: run.elapsed_ms,
                    status: run.status,
                });
            }
        }
        view.page(after)
    }

    pub async fn authorize_subagent(&self, agent: &str) -> Result<()> {
        ensure!(
            self.store.subagent_parent(agent).await?.as_deref() == Some(&self.info.id),
            "该记录不是当前会话的子代理。"
        );
        Ok(())
    }

    pub fn spawn(
        setup: crate::PreparedWorkspace,
        configuration: Option<super::configuration::Installed>,
    ) -> Self {
        for diagnostic in &setup.resources.diagnostics {
            eprintln!("{diagnostic}");
        }
        let info = super::profile::describe(&setup);
        let policy = setup.resources.policy.clone();
        let mut view = View {
            updates: Vec::new(),
            busy: false,
            model: info.model.clone(),
            approvals: BTreeMap::new(),
            clock: None,
            progress: Vec::new(),
            user_inputs: Vec::new(),
            planning: Default::default(),
        };
        for run in &setup.session.snapshot.runs {
            view.push(Update::Started {
                presentation: presentation(run),
                prompt: run.prompt.clone(),
            });
            for event in &run.events {
                view.push(Update::agent(event.clone()));
            }
            view.push(Update::Finished {
                elapsed_ms: run.elapsed_ms,
                status: run.status.clone(),
            });
        }
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
        Self {
            questions,
            _configuration: configuration,
            info,
            policy,
            view,
            requests,
            workspace,
            store,
        }
    }

    /// Only expose attachments published in this session's transcript.
    pub async fn has_image(&self, path: &str) -> bool {
        self.view.lock().await.updates.iter().any(|(update, _)| {
            matches!(update, Update::Started { presentation, .. }
                if presentation.image_assets.iter().any(|image| image.path == path))
        })
    }

    pub async fn current_info(&self) -> SessionInfo {
        self.view
            .lock()
            .await
            .updates
            .iter()
            .rev()
            .find_map(|(update, _)| match update {
                Update::Profile(info) => Some((**info).clone()),
                _ => None,
            })
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

    pub async fn command(&self, command: Command) -> Result<()> {
        let (reply, response) = oneshot::channel();
        self.requests
            .send(Envelope { command, reply })
            .await
            .map_err(|_| anyhow::anyhow!("workspace session stopped"))?;
        response.await?
    }
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
    mode: Mode,
    model: &Option<String>,
) -> String {
    let bytes = serde_json::to_vec(&(prompt, images, compact, mode, model))
        .expect("serializable workspace request");
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests;
