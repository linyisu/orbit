use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::Path,
    sync::Arc,
};

use anyhow::{Result, ensure};
use orbit_agent::events::AgentEvent;
use orbit_store::workspace_requests::RequestClaim;
use orbit_tools::approval::{ApprovalPreview, ApprovalRequest};
use sha2::{Digest, Sha256};
use tokio::sync::{Mutex, mpsc, oneshot};

use super::protocol::{Approval, Mode, PAGE_BYTES, PAGE_EVENTS, Page, SessionInfo, Update};

const COMMAND_CAPACITY: usize = 16;

pub struct Handle {
    _configuration: Option<super::configuration::Installed>,
    pub info: SessionInfo,
    pub view: Arc<Mutex<View>>,
    requests: mpsc::Sender<Envelope>,
    pub workspace: orbit_runtime::filesystem::Workspace,
    pub store: orbit_store::store::Store,
    pub tools: orbit_agent::tool::ToolRegistry,
}

pub enum Command {
    Start {
        request_id: String,
        prompt: String,
        mode: Mode,
        model: Option<String>,
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
        let mut bytes = 0;
        let updates: Vec<_> = self.updates[after..]
            .iter()
            .take(PAGE_EVENTS)
            .take_while(|(_, size)| {
                if bytes > 0 && bytes + size > PAGE_BYTES {
                    return false;
                }
                bytes += size;
                true
            })
            .map(|(update, _)| update.clone())
            .collect();
        let next = after + updates.len();
        Ok(Page {
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
        };
        for run in snapshot.runs {
            view.push(Update::Started { prompt: run.prompt });
            for event in run.events {
                view.push(Update::agent(event));
            }
            if run.status != "running" {
                view.push(Update::Finished { status: run.status });
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
        mut setup: crate::PreparedWorkspace,
        configuration: Option<super::configuration::Installed>,
    ) -> Self {
        for diagnostic in &setup.resources.diagnostics {
            eprintln!("{diagnostic}");
        }
        let budget = &setup.resources.budget;
        let info = SessionInfo {
            id: setup.session.id().into(),
            workspace: setup.session.snapshot.session.workspace.clone(),
            model: setup.resources.model_name.clone(),
            models: budget
                .model_options
                .iter()
                .map(crate::models::ModelOption::qualified_name)
                .collect(),
            context_tokens: budget.context_tokens,
            input_context_tokens: budget.input_context_tokens,
            max_output_tokens: budget.max_output_tokens,
        };
        let mut view = View {
            updates: Vec::new(),
            busy: false,
            model: info.model.clone(),
            approvals: BTreeMap::new(),
        };
        for run in &setup.session.snapshot.runs {
            view.push(Update::Started {
                prompt: run.prompt.clone(),
            });
            for event in &run.events {
                view.push(Update::agent(event.clone()));
            }
            view.push(Update::Finished {
                status: run.status.clone(),
            });
        }
        let view = Arc::new(Mutex::new(view));
        let (requests, mut envelopes) = mpsc::channel::<Envelope>(COMMAND_CAPACITY);
        let workspace = setup.resources.workspace.clone();
        let store = setup.session.store.clone();
        let tools = setup.resources.tools.clone();
        let state = view.clone();
        // The actor owns the runtime; clients only own connections to it.
        tokio::spawn(async move {
            let mut accepted = HashMap::<String, String>::new();
            let mut remembered = HashSet::<String>::new();
            let mut approval_id = 0;
            while let Some(envelope) = envelopes.recv().await {
                match envelope.command {
                    Command::Start {
                        request_id,
                        prompt,
                        mode,
                        model,
                    } => {
                        if request_id.is_empty()
                            || request_id.len() > 128
                            || prompt.trim().is_empty()
                            || prompt.len() > 1024 * 1024
                        {
                            let _ = envelope
                                .reply
                                .send(Err(anyhow::anyhow!("invalid workspace request")));
                            continue;
                        }
                        let identity = request_fingerprint(&prompt, mode, &model);
                        match setup
                            .session
                            .store
                            .claim_workspace_request(setup.session.id(), &request_id, &identity)
                            .await
                        {
                            Ok(RequestClaim::New) => {}
                            Ok(RequestClaim::Accepted) => {
                                let _ = envelope.reply.send(Ok(()));
                                continue;
                            }
                            Ok(RequestClaim::Rejected(error)) => {
                                let _ = envelope.reply.send(Err(anyhow::anyhow!(error)));
                                continue;
                            }
                            Err(error) => {
                                if setup.session.active_run_id().is_some() {
                                    let _ = setup.session.finish("failed", None).await;
                                }
                                let _ = envelope.reply.send(Err(error));
                                continue;
                            }
                        }
                        let started =
                            super::run::start(&mut setup, prompt.clone(), model.as_deref()).await;
                        let (mut events, task, cancellation) = match started {
                            Ok(started) => started,
                            Err(error) => {
                                let _ = setup
                                    .session
                                    .store
                                    .finish_workspace_request(
                                        setup.session.id(),
                                        &request_id,
                                        Some(format!("{error:#}")),
                                    )
                                    .await;
                                let _ = envelope.reply.send(Err(error));
                                continue;
                            }
                        };
                        let receipt = setup
                            .session
                            .store
                            .finish_workspace_request(setup.session.id(), &request_id, None)
                            .await;
                        accepted.clear();
                        accepted.insert(request_id, identity);
                        {
                            let mut view = state.lock().await;
                            view.busy = true;
                            view.model = setup.resources.model_name.clone();
                            view.push(Update::Profile(SessionInfo {
                                id: setup.session.id().into(),
                                workspace: setup.session.snapshot.session.workspace.clone(),
                                model: setup.resources.model_name.clone(),
                                models: setup
                                    .resources
                                    .budget
                                    .model_options
                                    .iter()
                                    .map(crate::models::ModelOption::qualified_name)
                                    .collect(),
                                context_tokens: setup.resources.budget.context_tokens,
                                input_context_tokens: setup.resources.budget.input_context_tokens,
                                max_output_tokens: setup.resources.budget.max_output_tokens,
                            }));
                            view.push(Update::Started { prompt });
                        }
                        let _ = envelope.reply.send(receipt);
                        let mut pending = HashMap::<u64, ApprovalRequest>::new();
                        let mut flush =
                            tokio::time::interval(std::time::Duration::from_millis(250));
                        loop {
                            tokio::select! {
                                event = events.recv() => match event {
                                    Some(event) => state.lock().await.push(Update::agent(event)),
                                    None => break,
                                },
                                _ = flush.tick() => {
                                    if let Err(error) = setup.session.flush_events().await {
                                        state.lock().await.push(Update::agent(AgentEvent::Failed {
                                            error: error.to_string(),
                                        }));
                                        cancellation.cancel();
                                    }
                                }
                                Some(request) = setup.resources.approvals.recv() => {
                                    let key = approval_key(
                                        &request.preview,
                                        Path::new(&setup.session.snapshot.session.workspace),
                                    );
                                    if mode == Mode::Yolo
                                        || key.as_ref().is_some_and(|key| remembered.contains(key))
                                        || (mode == Mode::Auto
                                            && orbit_tools::approval::auto_allows(
                                                &request.preview,
                                                Path::new(&setup.session.snapshot.session.workspace),
                                            ))
                                    {
                                        let _ = request.decision.send(true);
                                    } else {
                                        approval_id += 1;
                                        state
                                            .lock()
                                            .await
                                            .approvals
                                            .insert(approval_id, request.preview.clone());
                                        pending.insert(approval_id, request);
                                    }
                                }
                                Some(envelope) = envelopes.recv() => {
                                    let result = match envelope.command {
                                        Command::Cancel => {
                                            cancellation.cancel();
                                            pending.clear();
                                            state.lock().await.approvals.clear();
                                            Ok(())
                                        }
                                        Command::Approve {
                                            id,
                                            allow,
                                            remember,
                                        } => match pending.remove(&id) {
                                            Some(request) => {
                                                if allow
                                                    && remember
                                                    && let Some(key) = approval_key(
                                                        &request.preview,
                                                        Path::new(&setup.session.snapshot.session.workspace),
                                                    )
                                                {
                                                    remembered.insert(key);
                                                }
                                                state.lock().await.approvals.remove(&id);
                                                request
                                                    .decision
                                                    .send(allow)
                                                    .map_err(|_| anyhow::anyhow!("approval is no longer pending"))
                                            }
                                            None => Err(anyhow::anyhow!("approval is no longer pending")),
                                        },
                                        Command::Start {
                                            request_id,
                                            prompt,
                                            mode,
                                            model,
                                        } => {
                                            let identity = request_fingerprint(&prompt, mode, &model);
                                            if accepted.get(&request_id) == Some(&identity) {
                                                Ok(())
                                            } else {
                                                Err(anyhow::anyhow!(
                                                    "session is running; cancel or wait before submitting"
                                                ))
                                            }
                                        }
                                    };
                                    let _ = envelope.reply.send(result);
                                }
                            }
                        }
                        pending.clear();
                        let (status, event, history) =
                            crate::execution::outcome(task.await, Some(&cancellation));
                        let error = match event {
                            AgentEvent::Failed { error } => Some(error),
                            _ => None,
                        };
                        let saved = setup.session.finish(status, history).await;
                        let status = if saved.is_ok() { status } else { "failed" };
                        let mut view = state.lock().await;
                        if let Some(error) =
                            error.or_else(|| saved.err().map(|error| error.to_string()))
                        {
                            view.push(Update::agent(AgentEvent::Failed { error }));
                        }
                        view.push(Update::Finished {
                            status: status.into(),
                        });
                        view.busy = false;
                        view.approvals.clear();
                    }
                    Command::Cancel => {
                        let _ = envelope.reply.send(Ok(()));
                    }
                    Command::Approve { .. } => {
                        let _ = envelope
                            .reply
                            .send(Err(anyhow::anyhow!("no pending approval")));
                    }
                }
            }
        });
        Self {
            _configuration: configuration,
            info,
            view,
            requests,
            workspace,
            store,
            tools,
        }
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
            orbit_tools::approval::file_grant_key(&preview.path, workspace)?
        }
        ApprovalPreview::Delete(preview) => {
            orbit_tools::approval::file_grant_key(&preview.path, workspace)?
        }
        ApprovalPreview::Command(preview) if preview.git.is_some() => {
            preview.git_grant_key(workspace)?
        }
        ApprovalPreview::Command(preview) => preview.request_grant_key(workspace),
        ApprovalPreview::Mcp(_) | ApprovalPreview::Capability(_) => return None,
    };
    Some(format!("{target}:{}", serde_json::to_string(preview).ok()?))
}

fn request_fingerprint(prompt: &str, mode: Mode, model: &Option<String>) -> String {
    let bytes = serde_json::to_vec(&(prompt, mode, model)).expect("serializable workspace request");
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
