use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::Arc,
    time::Duration,
};

use anyhow::{Result, ensure};
use orbit_agent::{
    events::AgentEvent,
    inbox::{AgentInbox, InboxMessage},
};
use orbit_store::workspace_requests::RequestClaim;
use orbit_tools::approval::ApprovalRequest;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use super::{Command, Envelope, Mode, Update, View, approval_key, request_fingerprint};
use crate::{
    PreparedWorkspace,
    workspace::{profile, questions::Questions},
};

struct Running {
    events: mpsc::Receiver<AgentEvent>,
    task: tokio::task::JoinHandle<
        Result<orbit_agent::agent::AgentResult, orbit_agent::error::AgentError>,
    >,
    cancellation: CancellationToken,
    inbox: AgentInbox,
}

struct Undo {
    task: tokio::task::JoinHandle<Result<String>>,
    cancellation: CancellationToken,
}

struct Actor {
    configuration: Option<crate::workspace::configuration::Installed>,
    workspace: Arc<std::sync::RwLock<orbit_runtime::filesystem::Workspace>>,
    setup: PreparedWorkspace,
    view: Arc<Mutex<View>>,
    questions: Questions,
    running: Option<Running>,
    undo: Option<Undo>,
    mode: Mode,
    accepted: HashMap<String, String>,
    remembered: HashSet<String>,
    pending: HashMap<u64, ApprovalRequest>,
    approval_id: u64,
}

pub(super) async fn run(
    setup: PreparedWorkspace,
    view: Arc<Mutex<View>>,
    questions: Questions,
    workspace: Arc<std::sync::RwLock<orbit_runtime::filesystem::Workspace>>,
    mut requests: mpsc::Receiver<Envelope>,
) {
    let mode = setup.resources.mode;
    let mut actor = Actor {
        configuration: None,
        workspace,
        setup,
        view,
        questions,
        running: None,
        undo: None,
        mode,
        accepted: HashMap::new(),
        remembered: HashSet::new(),
        pending: HashMap::new(),
        approval_id: 0,
    };
    let mut flush = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            envelope = requests.recv() => {
                let Some(envelope) = envelope else { break };
                if matches!(&envelope.command, Command::Close) {
                    if actor.active() || actor.questions.running().await {
                        let _ = envelope.reply.send(Err(anyhow::anyhow!("会话正在运行，无法删除")));
                        continue;
                    }
                    drop(actor);
                    let _ = envelope.reply.send(Ok(()));
                    return;
                }
                let result = actor.command(envelope.command).await;
                let _ = envelope.reply.send(result);
            }
            event = async { actor.running.as_mut().unwrap().events.recv().await }, if actor.running.is_some() => {
                match event {
                    Some(event) => actor.view.lock().await.push(Update::agent(event)),
                    None => actor.finish_run().await,
                }
            }
            result = async { (&mut actor.undo.as_mut().unwrap().task).await }, if actor.undo.is_some() => {
                actor.undo = None;
                actor.pending.clear();
                let mut view = actor.view.lock().await;
                view.busy = false;
                view.approvals.clear();
                let text = match result { Ok(Ok(text)) => text, Ok(Err(error)) => format!("撤销失败：{error:#}"), Err(error) => format!("撤销任务中断：{error}") };
                view.push(Update::Report(orbit_protocol::Report { title: "撤销文件写入".into(), text }));
            }
            Some(request) = actor.setup.resources.approvals.recv() => actor.approval(request).await,
            _ = flush.tick() => {
                if actor.running.is_some() && let Err(error) = actor.setup.session.flush_events().await {
                    actor.view.lock().await.push(Update::agent(AgentEvent::Failed { error: error.to_string() }));
                    actor.cancel();
                }
                if actor.setup.session.poll_title().await.unwrap_or(false) { actor.profile().await; }
            }
        }
    }
    actor.cancel();
}

impl Actor {
    fn busy(&self) -> bool {
        self.running.is_some() || self.undo.is_some()
    }

    fn active(&self) -> bool {
        self.busy()
            || self.setup.resources.tools.task_groups().any(|jobs| {
                jobs.snapshots(&[], false).map_or(true, |jobs| {
                    jobs.iter().any(|job| {
                        matches!(
                            job.status,
                            orbit_agent::background::JobStatus::Running
                                | orbit_agent::background::JobStatus::Cancelling
                        )
                    })
                })
            })
    }

    fn cancel(&self) {
        if let Some(run) = &self.running {
            run.cancellation.cancel();
        }
        if let Some(undo) = &self.undo {
            undo.cancellation.cancel();
        }
    }

    async fn profile(&self) {
        if let Ok(mut workspace) = self.workspace.write() {
            *workspace = self.setup.resources.workspace.clone();
        }
        self.questions.update(&self.setup);
        let mut view = self.view.lock().await;
        view.model = self.setup.resources.model_name.clone();
        view.push(Update::Profile(Box::new(profile::describe(&self.setup))));
    }

    async fn command(&mut self, command: Command) -> Result<()> {
        match command {
            Command::Activity { result } => {
                let _ = result.send(self.active() || self.questions.running().await);
            }
            Command::SendSubagent { agent, message } => {
                self.setup
                    .resources
                    .tools
                    .execute(
                        orbit_model::types::ToolCall::function(
                            super::super::request_id(),
                            "send_agent_message",
                            serde_json::json!({"agent_id": agent, "message": message}),
                        ),
                        CancellationToken::new(),
                    )
                    .await?;
            }
            Command::SetMode { mode } => {
                self.setup
                    .session
                    .store
                    .set_tui_mode(
                        self.setup.session.id(),
                        match mode {
                            Mode::Ask => "ask",
                            Mode::Auto => "auto",
                            Mode::Yolo => "YOLO",
                        },
                    )
                    .await?;
                self.setup.resources.mode = mode;
                self.mode = mode;
                self.profile().await;
            }
            Command::ConfigureModels { configuration } => {
                ensure!(!self.busy(), "请在当前任务结束后更新模型配置");
                let installed = match configuration {
                    Some(configuration) => Some(
                        crate::workspace::configuration::Installed::install(
                            configuration,
                            &self.setup.resources.options,
                        )
                        .await?,
                    ),
                    None => None,
                };
                let options = installed
                    .as_ref()
                    .map_or(&self.setup.resources.options, |value| &value.options);
                let mut config =
                    crate::config::FileConfig::load(&options.config, &options.auth_directory)?;
                crate::account_models::refresh(&mut config, false).await;
                let models = config.model_options()?;
                let selected = self.setup.resources.model_name.clone();
                let available = models
                    .iter()
                    .any(|model| model.qualified_name() == selected);
                let effort =
                    self.setup
                        .resources
                        .model
                        .config()
                        .reasoning_effort
                        .filter(|effort| {
                            models.iter().any(|model| {
                                model.qualified_name() == selected
                                    && model.reasoning_efforts.contains(effort)
                            })
                        });
                self.setup.resources.budget.model_options = models;
                if available {
                    profile::select(&mut self.setup, &selected, effort.map(|e| e.as_str())).await?;
                }
                if let Some(installed) = installed {
                    self.setup.resources.options = installed.options.clone();
                    self.configuration = Some(installed);
                }
                self.profile().await;
            }
            Command::Close => unreachable!("close handled by actor loop"),
            Command::Action { action, result } => {
                let busy = self.active() || self.questions.running().await;
                let response =
                    crate::workspace::actions::execute(&mut self.setup, action, busy).await;
                if response.is_ok() {
                    self.profile().await;
                }
                let _ = result.send(response);
            }
            Command::SetModel { model, effort } => {
                ensure!(
                    !self.busy(),
                    "wait for the current task before changing its model"
                );
                profile::select(&mut self.setup, &model, effort.as_deref()).await?;
                self.profile().await;
            }
            Command::Steer { message } => {
                ensure!(
                    !message.trim().is_empty() && message.len() <= 1024 * 1024,
                    "invalid session input"
                );
                let run = self
                    .running
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("session is idle; submit a new message"))?;
                run.inbox
                    .persist_and_push(|| async {
                        let id = self
                            .setup
                            .session
                            .store
                            .queue_session_input(self.setup.session.id(), message.clone())
                            .await
                            .map_err(|error| {
                                orbit_agent::tool::ToolError::ExecutionFailed(error.to_string())
                            })?;
                        Ok(InboxMessage {
                            id,
                            content: message,
                        })
                    })
                    .await?;
            }
            Command::Cancel => {
                self.cancel();
                self.pending.clear();
                self.view.lock().await.approvals.clear();
            }
            Command::Approve {
                id,
                allow,
                remember,
            } => {
                let request = self
                    .pending
                    .remove(&id)
                    .ok_or_else(|| anyhow::anyhow!("approval is no longer pending"))?;
                if allow
                    && remember
                    && let Some(key) = approval_key(
                        &request.preview,
                        Path::new(&self.setup.session.snapshot.session.workspace),
                    )
                {
                    self.remembered.insert(key);
                }
                {
                    let mut view = self.view.lock().await;
                    view.approvals.remove(&id);
                    if view.approvals.is_empty()
                        && let Some(clock) = &mut view.clock
                    {
                        clock.resume();
                    }
                }
                request
                    .decision
                    .send(allow)
                    .map_err(|_| anyhow::anyhow!("approval is no longer pending"))?;
            }
            Command::Undo { path } => self.undo(path).await?,
            Command::Start {
                request_id,
                prompt,
                images,
                compact,
                mode,
                model,
            } => {
                ensure!(
                    !request_id.is_empty()
                        && request_id.len() <= 128
                        && (!prompt.trim().is_empty() || !images.is_empty())
                        && prompt.len() <= 1024 * 1024,
                    "invalid workspace request"
                );
                let identity = request_fingerprint(&prompt, &images, compact, mode, &model);
                if self.busy() {
                    ensure!(
                        self.accepted.get(&request_id) == Some(&identity),
                        "session is running; cancel or wait before submitting"
                    );
                    return Ok(());
                }
                match self
                    .setup
                    .session
                    .store
                    .claim_workspace_request(self.setup.session.id(), &request_id, &identity)
                    .await?
                {
                    RequestClaim::New => {}
                    RequestClaim::Accepted => return Ok(()),
                    RequestClaim::Rejected(error) => anyhow::bail!("{error}"),
                }
                let presentation = orbit_protocol::RunPresentation {
                    images: images.iter().map(|image| image.name.clone()).collect(),
                    compact,
                    queued_input: false,
                };
                let started = crate::workspace::run::start(
                    &mut self.setup,
                    prompt.clone(),
                    images,
                    compact,
                    model.as_deref(),
                )
                .await;
                let (events, task, cancellation, inbox) = match started {
                    Ok(started) => started,
                    Err(error) => {
                        let _ = self
                            .setup
                            .session
                            .store
                            .finish_workspace_request(
                                self.setup.session.id(),
                                &request_id,
                                Some(format!("{error:#}")),
                            )
                            .await;
                        return Err(error);
                    }
                };
                self.running = Some(Running {
                    events,
                    task,
                    cancellation,
                    inbox,
                });
                self.mode = mode;
                self.accepted.clear();
                self.accepted.insert(request_id.clone(), identity);
                self.profile().await;
                {
                    let mut view = self.view.lock().await;
                    view.busy = true;
                    view.clock = Some(super::Clock::new());
                    view.push(Update::Started {
                        prompt,
                        presentation,
                    });
                }
                self.setup
                    .session
                    .store
                    .finish_workspace_request(self.setup.session.id(), &request_id, None)
                    .await?;
            }
        }
        Ok(())
    }

    async fn approval(&mut self, request: ApprovalRequest) {
        if !self.busy() {
            let _ = request.decision.send(false);
            return;
        }
        let workspace = Path::new(&self.setup.session.snapshot.session.workspace);
        let key = approval_key(&request.preview, workspace);
        let allowed = self.undo.is_none()
            && (self.mode == Mode::Yolo
                || key
                    .as_ref()
                    .is_some_and(|key| self.remembered.contains(key))
                || (self.mode == Mode::Auto
                    && orbit_tools::approval::auto_allows(&request.preview, workspace)));
        if allowed {
            let _ = request.decision.send(true);
            return;
        }
        self.approval_id += 1;
        self.view
            .lock()
            .await
            .approvals
            .insert(self.approval_id, request.preview.clone());
        if let Some(clock) = &mut self.view.lock().await.clock {
            clock.pause();
        }
        self.pending.insert(self.approval_id, request);
    }

    async fn finish_run(&mut self) {
        let run = self.running.take().unwrap();
        self.pending.clear();
        let (status, event, history) =
            crate::execution::outcome(run.task.await, Some(&run.cancellation));
        let error = match event {
            AgentEvent::Failed { error } => Some(error),
            _ => None,
        };
        let saved = self.setup.session.finish(status, history).await;
        if let Ok(mut workspace) = self.workspace.write() {
            *workspace = self.setup.resources.workspace.clone();
        }
        self.questions.update(&self.setup);
        let status = if saved.is_ok() { status } else { "failed" };
        let mut view = self.view.lock().await;
        if let Some(error) = error.or_else(|| saved.err().map(|error| error.to_string())) {
            view.push(Update::agent(AgentEvent::Failed { error }));
        }
        view.clock = None;
        view.push(Update::Finished {
            elapsed_ms: self
                .setup
                .session
                .snapshot
                .runs
                .last()
                .and_then(|run| run.elapsed_ms),
            status: status.into(),
        });
        view.busy = false;
        view.approvals.clear();
    }

    async fn undo(&mut self, path: Option<String>) -> Result<()> {
        ensure!(
            !self.setup.resources.policy.read_only,
            "read-only sessions cannot undo files"
        );
        ensure!(!self.active(), "请在当前任务结束后撤销文件写入");
        let change = self
            .setup
            .session
            .store
            .workspace_changes(self.setup.session.id())
            .await?
            .into_iter()
            .rev()
            .find(|change| {
                !change.undone && path.as_ref().is_none_or(|path| path == &change.change.path)
            })
            .ok_or_else(|| anyhow::anyhow!("当前会话没有匹配的可撤销写入"))?;
        let writer = self.setup.resources.undo_writer.clone();
        let store = self.setup.session.store.clone();
        let session = self.setup.session.id().to_owned();
        let cancellation = CancellationToken::new();
        let token = cancellation.clone();
        let task = tokio::spawn(async move {
            let output = writer.restore(change.change.clone(), token).await?;
            store.record_workspace_undo(&session, &change).await?;
            Ok(output)
        });
        self.undo = Some(Undo { task, cancellation });
        self.view.lock().await.busy = true;
        Ok(())
    }
}
