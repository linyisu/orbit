use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::Arc,
    time::Duration,
};

use anyhow::{Context, Result, ensure};
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
    completion: Option<(
        String,
        tokio::sync::oneshot::Sender<Result<orbit_protocol::review::Snapshot>>,
    )>,
}

struct Actor {
    configuration: Option<crate::workspace::configuration::Installed>,
    workspace: Arc<std::sync::RwLock<orbit_runtime::filesystem::Workspace>>,
    setup: PreparedWorkspace,
    view: Arc<Mutex<View>>,
    questions: Questions,
    running: Option<Running>,
    continue_after_cancel: bool,
    undo: Option<Undo>,
    import: Option<crate::workspace::resources::Import>,
    mcp_reload: Option<crate::workspace::resources::Reload>,
    starting: Option<Envelope>,
    models_refreshed: bool,
    accepted: HashMap<String, String>,
    remembered: HashSet<String>,
    pending: HashMap<u64, ApprovalRequest>,
    approval_id: u64,
    user_input_enabled: bool,
    planning_enabled: bool,
    compact_after_model_switch: bool,
    progress: super::progress::Progress,
    progress_task: Option<tokio_util::task::AbortOnDropHandle<super::progress::Snapshot>>,
}

pub(super) async fn run(
    setup: PreparedWorkspace,
    view: Arc<Mutex<View>>,
    questions: Questions,
    workspace: Arc<std::sync::RwLock<orbit_runtime::filesystem::Workspace>>,
    mut requests: mpsc::Receiver<Envelope>,
) {
    let remembered = match setup
        .session
        .store
        .tui_file_grants(setup.session.id())
        .await
    {
        Ok(grants) => grants,
        Err(error) => {
            view.lock()
                .await
                .push(Update::Report(orbit_protocol::Report {
                    title: "文件授权".into(),
                    text: format!("无法读取本会话保存的文件授权，请重新确认：{error}"),
                }));
            HashSet::new()
        }
    };
    let mut actor = Actor {
        configuration: None,
        workspace,
        setup,
        view,
        questions,
        running: None,
        continue_after_cancel: false,
        undo: None,
        import: None,
        mcp_reload: None,
        starting: None,
        models_refreshed: false,
        accepted: HashMap::new(),
        remembered,
        pending: HashMap::new(),
        approval_id: 0,
        user_input_enabled: false,
        planning_enabled: false,
        compact_after_model_switch: false,
        progress: Default::default(),
        progress_task: None,
    };
    let mut outputs = tokio::time::interval(Duration::from_millis(50));
    outputs.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut flush = tokio::time::interval(Duration::from_millis(250));
    loop {
        tokio::select! {
            envelope = requests.recv() => {
                let Some(envelope) = envelope else { break };
                // Keep polling and accepting cancellation while optional tools
                // initialize. The first run must see the complete tool catalog.
                if actor
                    .setup
                    .resources
                    .mcp_startup
                    .is_some()
                    && matches!(
                        &envelope.command,
                        Command::Start { .. }
                            | Command::Plan {
                                action: orbit_protocol::plan::Action::Approve { .. }
                                    | orbit_protocol::plan::Action::Feedback { .. },
                            }
                    )
                {
                    if actor.starting.is_some() {
                        let _ = envelope
                            .reply
                            .send(Err(anyhow::anyhow!("已有输入正在等待工具准备完成")));
                    } else {
                        actor.starting = Some(envelope);
                    }
                    continue;
                }
                if let Command::RetireUnobserved { result } = envelope.command {
                    // Drafts have nothing to save. Persisted sessions can release
                    // their runtime only after execution and metadata work finish.
                    let retire = actor.starting.is_none()
                        && actor.import.is_none()
                        && actor.mcp_reload.is_none()
                        && (!actor.setup.session.is_persisted()
                            || (!actor.active()
                                && !actor.questions.running().await
                                && !actor.setup.session.metadata_pending()));
                    let _ = result.send(retire);
                    if retire {
                        drop(actor);
                        let _ = envelope.reply.send(Ok(()));
                        return;
                    }
                    let _ = envelope.reply.send(Ok(()));
                    continue;
                }
                if matches!(&envelope.command, Command::Close) {
                    if actor.active()
                        || actor.questions.running().await
                        || actor.import.is_some()
                        || actor.mcp_reload.is_some()
                    {
                        let _ = envelope
                            .reply
                            .send(Err(anyhow::anyhow!("会话正在运行，无法删除")));
                        continue;
                    }
                    drop(actor);
                    let _ = envelope.reply.send(Ok(()));
                    return;
                }
                let result = actor.command(envelope.command).await;
                let _ = envelope.reply.send(result);
            }
            event = async {
                actor
                    .running
                    .as_mut()
                    .unwrap()
                    .events
                    .recv()
                    .await
            }, if actor.running.is_some() => match event {
                Some(event) => {
                    actor.progress.record(&event);
                    let mut view = actor.view.lock().await;
                    if actor.progress.is_empty() {
                        view.progress.clear();
                    }
                    view.push(Update::agent(event));
                }
                None => actor.finish_run().await,
            },
            result = async { (&mut actor.undo.as_mut().unwrap().task).await },
                if actor.undo.is_some() => {
                let undo = actor.undo.take().unwrap();
                actor.pending.clear();
                let mut view = actor.view.lock().await;
                view.busy = false;
                view.approvals.clear();
                let result = match result {
                    Ok(result) => result,
                    Err(error) => Err(anyhow::anyhow!("撤销任务中断：{error}")),
                };
                if let Some((path, reply)) = undo.completion {
                    let _ = reply.send(
                        result.map(|detail| orbit_protocol::review::Snapshot::Undone {
                            path,
                            detail,
                        }),
                    );
                } else {
                    let text = match result {
                        Ok(text) => text,
                        Err(error) => format!("撤销失败：{error:#}"),
                    };
                    view.push(Update::Report(orbit_protocol::Report {
                        title: "撤销文件写入".into(),
                        text,
                    }));
                }
            }
            output = async { (&mut actor.import.as_mut().unwrap().task).await },
                if actor.import.is_some() => {
                let import = actor.import.take().unwrap();
                import.finish(
                    &mut actor.setup,
                    output
                        .unwrap_or_else(|error| Err(anyhow::anyhow!("MCP 导入任务中断：{error}"))),
                );
            }
            Some(request) = actor.setup.resources.approvals.recv() => actor.approval(request).await,
            output = async { (&mut actor.mcp_reload.as_mut().unwrap().task).await },
                if actor.mcp_reload.is_some() => {
                let reload = actor.mcp_reload.take().unwrap();
                reload.finish(
                    &mut actor.setup,
                    output
                        .unwrap_or_else(|error| Err(anyhow::anyhow!("MCP 重新加载中断：{error}"))),
                );
            }
            output = async {
                (&mut actor
                    .setup
                    .resources
                    .mcp_startup
                    .as_mut()
                    .unwrap()
                    .task)
                    .await
            },
                if actor
                    .setup
                    .resources
                    .mcp_startup
                    .is_some() => {
                let startup = actor
                    .setup
                    .resources
                    .mcp_startup
                    .take()
                    .unwrap();
                startup.finish(&mut actor.setup, output.map_err(anyhow::Error::from));
            }
            output = async {
                actor
                    .setup
                    .resources
                    .model_refresh
                    .as_mut()
                    .unwrap()
                    .await
            },
                if actor
                    .setup
                    .resources
                    .model_refresh
                    .is_some() => {
                actor.setup.resources.model_refresh = None;
                if let Ok(report) = output {
                    for diagnostic in report.diagnostics {
                        eprintln!("{diagnostic}");
                    }
                    actor.models_refreshed = true;
                }
            }
            _ = outputs.tick(),
                if actor.running.is_some()
                    && !actor.progress.is_empty()
                    && actor.progress_task.is_none() => {
                actor.progress_task = Some(actor.progress.start_snapshot(
                    &actor.setup.resources.tools,
                    &actor.setup.session.command_output,
                ));
            }
            result = async {
                actor
                    .progress_task
                    .as_mut()
                    .unwrap()
                    .await
            }, if actor.progress_task.is_some() => {
                actor.progress_task = None;
                if let Ok(snapshot) = result {
                    actor.view.lock().await.progress = actor.progress.accept(snapshot);
                }
            }
            _ = flush.tick() => {
                if actor.running.is_some()
                    && let Err(error) = actor.setup.session.flush_events().await
                {
                    actor
                        .view
                        .lock()
                        .await
                        .push(Update::agent(AgentEvent::Failed {
                            error: error.to_string(),
                        }));
                    actor.cancel();
                }
                actor.sync_user_input().await;
                if actor.models_refreshed && !actor.busy() {
                    actor.models_refreshed = false;
                    if let Err(error) = actor
                        .command(Command::ConfigureModels {
                            configuration: None,
                        })
                        .await
                    {
                        eprintln!("后台模型目录更新未应用：{error:#}");
                    }
                }
                actor.compact_if_needed().await;
                if actor
                    .setup
                    .session
                    .poll_title()
                    .await
                    .unwrap_or(false)
                {
                    actor.profile().await;
                }
            }
        }
        if actor
            .setup
            .resources
            .mcp_startup
            .is_none()
            && actor.mcp_reload.is_none()
            && let Some(envelope) = actor.starting.take()
            && !envelope.reply.is_closed()
        {
            let result = actor.command(envelope.command).await;
            let _ = envelope.reply.send(result);
        }
    }
    actor.cancel();
}

impl Actor {
    fn busy(&self) -> bool {
        self.running.is_some() || self.undo.is_some() || self.starting.is_some()
    }

    fn active(&self) -> bool {
        self.busy()
            || self
                .setup
                .resources
                .tools
                .task_groups()
                .any(|jobs| {
                    jobs.snapshots(&[], false)
                        .map_or(true, |jobs| {
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

    async fn sync_user_input(&self) {
        let inputs = self
            .setup
            .resources
            .user_input
            .pending()
            .await;
        let mut view = self.view.lock().await;
        view.user_inputs = inputs;
        view.planning = self
            .setup
            .resources
            .planning
            .snapshot()
            .await;
        let waiting = view
            .user_inputs
            .iter()
            .any(|request| request.source.is_none())
            || !view.approvals.is_empty()
            || self.setup.resources.planning.waiting();
        if let Some(clock) = &mut view.clock {
            if waiting {
                clock.pause();
            } else {
                clock.resume();
            }
        }
    }

    async fn profile(&self) {
        if self.planning_enabled {
            self.setup.resources.planning.enable();
        }
        if self.user_input_enabled {
            self.setup.resources.user_input.enable();
        }
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
            Command::Plan { action } => {
                use orbit_protocol::plan::Action;
                if matches!(action, Action::Enable | Action::Enter) {
                    self.planning_enabled = true;
                }
                if self
                    .setup
                    .resources
                    .planning
                    .already_applied(&action)
                    .await
                {
                    return Ok(());
                }
                match &action {
                    Action::Enable => {}
                    Action::Enter | Action::Leave => {
                        ensure!(!self.active(), "请先停止或等待当前任务结束，再切换工作模式")
                    }
                    _ => ensure!(
                        self.undo.is_none()
                            && (!self.busy() || self.setup.resources.planning.waiting()),
                        "请等待当前任务结束或提交计划后再操作"
                    ),
                }
                let prompt = self
                    .setup
                    .resources
                    .planning
                    .action(action)
                    .await?;
                self.sync_user_input().await;
                if let Some(prompt) = prompt {
                    ensure!(!self.busy(), "请等待当前任务结束，再发送消息继续计划");
                    Box::pin(self.command(Command::Start {
                        request_id: orbit_protocol::request_id(),
                        prompt,
                        images: vec![],
                        files: Vec::new(),
                        compact: false,
                        model: None,
                    }))
                    .await?;
                }
            }
            Command::EnableUserInput => {
                self.user_input_enabled = true;
                self.setup.resources.user_input.enable();
            }
            Command::AnswerUserInput { id, answers } => {
                self.setup
                    .resources
                    .user_input
                    .answer(&id, answers)
                    .await?
            }
            Command::Activity { result } => {
                let _ = result.send(self.active() || self.questions.running().await);
            }
            Command::UsageContext { result } => {
                let _ = result.send((
                    self.setup
                        .resources
                        .model
                        .config()
                        .clone(),
                    self.setup.session.is_persisted(),
                ));
            }
            Command::SendSubagent { agent, message } => {
                let message = message.trim().to_owned();
                self.setup
                    .resources
                    .tools
                    .execute(
                        orbit_model::types::ToolCall::function(
                            super::super::request_id(),
                            "send_agent_message",
                            serde_json::json!({
                                "agent_id": agent,
                                "message": message
                            }),
                        ),
                        CancellationToken::new(),
                    )
                    .await?;
            }
            Command::SetMode { mode } => {
                if self.setup.session.is_persisted() {
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
                }
                self.setup.resources.mode = mode;
                self.setup.resources.mode_revision += 1;
                self.profile().await;
                let allowed: Vec<_> = self
                    .pending
                    .iter()
                    .filter(|(_, request)| self.allows(&request.preview))
                    .map(|(id, _)| *id)
                    .collect();
                for id in allowed {
                    // A cancelled waiter does not invalidate the committed mode change.
                    let _ = self.resolve_approval(id, true).await;
                }
            }
            Command::ConfigureModels { configuration } => {
                ensure!(!self.busy(), "请在当前任务结束后更新模型配置");
                self.setup.resources.model_refresh = None;
                self.models_refreshed = false;
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
                let _ = orbit_config::models::updater::load_cached(&mut config).await;
                let models = config.model_options()?;
                let previous_limit = self
                    .setup
                    .resources
                    .budget
                    .input_context_tokens;
                let selected = self.setup.resources.model_name.clone();
                let available = models
                    .iter()
                    .any(|model| model.qualified_name() == selected);
                let effort = self
                    .setup
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
                self.setup
                    .resources
                    .budget
                    .model_options = models;
                if available {
                    profile::select(&mut self.setup, &selected, effort.map(|e| e.as_str())).await?;
                    self.compact_after_model_switch |= self.setup.resources.budget.auto_compact
                        && self
                            .setup
                            .resources
                            .budget
                            .input_context_tokens
                            < previous_limit;
                }
                if let Some(installed) = installed {
                    self.setup.resources.options = installed.options.clone();
                    self.configuration = Some(installed);
                }
                self.profile().await;
            }
            Command::Close | Command::RetireUnobserved { .. } => {
                unreachable!("close handled by actor loop")
            }
            Command::Action { action, result } => {
                let busy = self.active() || self.questions.running().await;
                let response =
                    crate::workspace::actions::execute(&mut self.setup, action, busy).await;
                if response.is_ok() {
                    self.profile().await;
                }
                let _ = result.send(response);
            }
            Command::Resources {
                action,
                expected_workspace,
                result,
            } => {
                ensure!(
                    expected_workspace
                        .as_ref()
                        .is_none_or(|expected| {
                            expected
                                == &self
                                    .setup
                                    .session
                                    .snapshot
                                    .session
                                    .workspace
                        }),
                    "工作区已切换，请重新打开当前项目的设置后重试"
                );
                if matches!(
                    action,
                    orbit_protocol::resources::Action::ScanMcp { .. }
                        | orbit_protocol::resources::Action::InstallMcp { .. }
                ) {
                    ensure!(self.import.is_none(), "MCP 导入正在进行，请等待完成");
                    self.import = Some(crate::workspace::resources::Import::start(
                        &mut self.setup,
                        action,
                        result,
                    )?);
                    return Ok(());
                }
                if let orbit_protocol::resources::Action::ReloadMcp {
                    config,
                    allow_start,
                } = action
                {
                    ensure!(self.mcp_reload.is_none(), "MCP 正在重新连接，请等待完成");
                    self.mcp_reload = Some(crate::workspace::resources::Reload::start(
                        &self.setup,
                        config,
                        allow_start,
                        result,
                    )?);
                    // An explicit reload supersedes the initial connection.
                    self.setup.resources.mcp_startup = None;
                    return Ok(());
                }
                let busy = self.active() || self.questions.running().await;
                let response =
                    crate::workspace::resources::execute(&mut self.setup, action, busy).await;
                let _ = result.send(response);
            }
            Command::Files {
                action,
                expected_workspace,
                result,
            } => {
                let busy = self.active() || self.questions.running().await;
                let response = async {
                    ensure!(
                        expected_workspace
                            .as_ref()
                            .is_none_or(|expected| expected
                                == &self
                                    .setup
                                    .session
                                    .snapshot
                                    .session
                                    .workspace),
                        "工作区已切换，请重新打开文件"
                    );
                    crate::workspace::files::execute(&mut self.setup, action, busy).await
                }
                .await;
                let _ = result.send(response);
            }
            Command::Review {
                action,
                expected_workspace,
                result,
            } => {
                let scope = expected_workspace
                    == self
                        .setup
                        .session
                        .snapshot
                        .session
                        .workspace;
                if !scope {
                    let _ = result.send(Err(anyhow::anyhow!("工作区已切换，请重新打开差异审查")));
                } else if matches!(
                    action,
                    orbit_protocol::review::Action::Stage { .. }
                        | orbit_protocol::review::Action::Unstage { .. }
                ) {
                    let response = async {
                        ensure!(
                            !self.active() && !self.questions.running().await,
                            "请先结束正在运行的任务再更改暂存区"
                        );
                        crate::workspace::review::execute(&self.setup, action).await
                    }
                    .await;
                    let _ = result.send(response);
                } else if let orbit_protocol::review::Action::Undo {
                    path,
                    run_id,
                    call_id,
                } = action
                {
                    let response = async {
                        ensure!(
                            !self.questions.running().await,
                            "请先结束旁路提问再撤销文件写入"
                        );
                        let changes = self
                            .setup
                            .session
                            .store
                            .workspace_changes(self.setup.session.id())
                            .await?;
                        let current = changes
                            .iter()
                            .rev()
                            .find(|change| change.change.path == path)
                            .context("该检查点已不存在")?;
                        ensure!(
                            !current.undone
                                && current.run_id == run_id
                                && current.call_id == call_id,
                            "检查点已变更，请重新审查后撤销"
                        );
                        self.undo(Some(path.clone())).await
                    }
                    .await;
                    match response {
                        Ok(()) => self.undo.as_mut().unwrap().completion = Some((path, result)),
                        Err(error) => {
                            let _ = result.send(Err(error));
                        }
                    }
                } else {
                    let _ =
                        result.send(crate::workspace::review::execute(&self.setup, action).await);
                }
            }
            Command::Subagents {
                action,
                expected_workspace,
                result,
            } => {
                let response = async {
                    ensure!(
                        expected_workspace
                            == self
                                .setup
                                .session
                                .snapshot
                                .session
                                .workspace,
                        "工作区已切换，请重新打开子 Agent 列表"
                    );
                    crate::workspace::subagents::execute(&mut self.setup, action).await
                }
                .await;
                let _ = result.send(response);
            }
            Command::InputQueue {
                action,
                expected_workspace,
                result,
            } => {
                let inbox = self
                    .running
                    .as_ref()
                    .map(|run| run.inbox.clone());
                let response = crate::workspace::input_queue::execute(
                    &mut self.setup,
                    inbox,
                    &expected_workspace,
                    action,
                )
                .await;
                let _ = result.send(response);
            }
            Command::Tasks {
                action,
                expected_workspace,
                result,
            } => {
                let response = async {
                    ensure!(
                        expected_workspace
                            .as_ref()
                            .is_none_or(|expected| {
                                expected
                                    == &self
                                        .setup
                                        .session
                                        .snapshot
                                        .session
                                        .workspace
                            }),
                        "工作区已切换，请重新打开当前项目的任务页后重试"
                    );
                    crate::workspace::tasks::snapshot(&mut self.setup, action).await
                }
                .await;
                let _ = result.send(response);
            }
            Command::SetModel { model, effort } => {
                ensure!(
                    !self.busy(),
                    "wait for the current task before changing its model"
                );
                let previous_limit = self
                    .setup
                    .resources
                    .budget
                    .input_context_tokens;
                profile::select(&mut self.setup, &model, effort.as_deref()).await?;
                self.compact_after_model_switch |= self.setup.resources.budget.auto_compact
                    && self
                        .setup
                        .resources
                        .budget
                        .input_context_tokens
                        < previous_limit;
                self.profile().await;
            }
            Command::Steer {
                message,
                images,
                files,
                accepted,
            } => {
                let message = message.trim().to_owned();
                ensure!(
                    (!message.is_empty() || !images.is_empty() || !files.is_empty())
                        && message.len() <= 1_024 * 1_024,
                    "invalid session input"
                );
                let run = self
                    .running
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("session is idle; submit a new message"))?;
                let mut attachments = super::super::run::import_images(
                    self.setup.session.store.path(),
                    self.setup
                        .resources
                        .model
                        .supports_vision(),
                    self.setup
                        .resources
                        .policy
                        .disabled(orbit_protocol::Capability::Image),
                    images,
                )
                .await?;
                attachments.extend(
                    super::super::run::import_files(self.setup.session.store.path(), files).await?,
                );
                super::super::run::grant_files(&self.setup.resources.workspace, &attachments)?;
                run.inbox
                    .persist_and_push(|| async {
                        let id = self
                            .setup
                            .session
                            .store
                            .queue_session_input_with_attachments(
                                self.setup.session.id(),
                                message.clone(),
                                attachments.clone(),
                            )
                            .await
                            .map_err(|error| {
                                orbit_agent::tool::ToolError::ExecutionFailed(error.to_string())
                            })?;
                        let _ = accepted.send(id);
                        Ok(InboxMessage {
                            attachments,
                            id,
                            content: message,
                        })
                    })
                    .await?;
            }
            Command::Cancel => {
                self.continue_after_cancel = self.running.is_some();
                if let Some(envelope) = self.starting.take() {
                    let _ = envelope
                        .reply
                        .send(Err(anyhow::anyhow!("输入已取消")));
                }
                self.cancel();
                self.pending.clear();
                self.view.lock().await.approvals.clear();
            }
            Command::Approve {
                id,
                allow,
                remember,
            } => {
                let preview = &self
                    .pending
                    .get(&id)
                    .ok_or_else(|| anyhow::anyhow!("approval is no longer pending"))?
                    .preview;
                if allow
                    && remember
                    && let Some(key) = approval_key(
                        preview,
                        Path::new(
                            &self
                                .setup
                                .session
                                .snapshot
                                .session
                                .workspace,
                        ),
                    )
                {
                    if matches!(
                        preview,
                        orbit_tools::approval::ApprovalPreview::Write(_)
                            | orbit_tools::approval::ApprovalPreview::Delete(_)
                    ) {
                        self.setup
                            .session
                            .store
                            .grant_tui_file(self.setup.session.id(), &key)
                            .await?;
                    }
                    self.remembered.insert(key);
                }
                self.resolve_approval(id, allow).await?;
            }
            Command::Undo { path } => {
                ensure!(
                    self.setup
                        .resources
                        .planning
                        .snapshot()
                        .await
                        .mode
                        != orbit_protocol::plan::WorkMode::Plan,
                    "Plan 模式不能撤销文件修改，请先切换 Build"
                );
                self.undo(path).await?;
            }
            Command::Start {
                request_id,
                prompt,
                images,
                files,
                compact,
                model,
            } => {
                let prompt = prompt.trim().to_owned();
                ensure!(
                    !request_id.is_empty()
                        && request_id.len() <= 128
                        && (!prompt.is_empty() || !images.is_empty() || !files.is_empty())
                        && prompt.len() <= 1_024 * 1_024,
                    "invalid workspace request"
                );
                let identity = request_fingerprint(&prompt, &images, &files, compact, &model);
                if self.busy() {
                    ensure!(
                        self.accepted.get(&request_id) == Some(&identity),
                        "session is running; cancel or wait before submitting"
                    );
                    return Ok(());
                }
                self.setup.start_session().await?;
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
                    file_assets: Vec::new(),
                    images: images
                        .iter()
                        .map(|image| image.name.clone())
                        .collect(),
                    compact,
                    queued_input: false,
                    replayed: false,
                    image_assets: Vec::new(),
                };
                let started = crate::workspace::run::start(
                    &mut self.setup,
                    Some(prompt.clone()),
                    images,
                    files,
                    compact,
                    model.as_deref(),
                )
                .await;
                let (events, task, cancellation, inbox, image_assets, file_assets) = match started {
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
                self.accepted.clear();
                self.accepted
                    .insert(request_id.clone(), identity);
                self.activate_run(
                    (events, task, cancellation, inbox, image_assets, file_assets),
                    prompt,
                    presentation,
                )
                .await;
                self.setup
                    .session
                    .store
                    .finish_workspace_request(self.setup.session.id(), &request_id, None)
                    .await?;
            }
        }
        Ok(())
    }

    async fn compact_if_needed(&mut self) {
        if self.busy()
            || self
                .setup
                .resources
                .mcp_startup
                .is_some()
            || !self.compact_after_model_switch
        {
            return;
        }
        self.compact_after_model_switch = false;
        let result = async {
            if super::super::run::needs_compaction(&self.setup).await? {
                self.command(Command::Start {
                    request_id: orbit_protocol::request_id(),
                    prompt: "/compact".into(),
                    images: Vec::new(),
                    files: Vec::new(),
                    compact: true,
                    model: None,
                })
                .await?;
            }
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if let Err(error) = result {
            self.view
                .lock()
                .await
                .push(Update::Report(orbit_protocol::Report {
                    title: "模型上下文整理".into(),
                    text: format!("模型已切换，但整理未能启动：{error:#}"),
                }));
        }
    }

    fn allows(&self, preview: &orbit_tools::approval::ApprovalPreview) -> bool {
        let workspace = Path::new(
            &self
                .setup
                .session
                .snapshot
                .session
                .workspace,
        );
        let key = approval_key(preview, workspace);
        self.undo.is_none()
            && (self.setup.resources.mode == Mode::Yolo
                || key
                    .as_ref()
                    .is_some_and(|key| self.remembered.contains(key))
                || (self.setup.resources.mode == Mode::Auto
                    && orbit_tools::approval::auto_allows(preview, workspace)))
    }

    async fn resolve_approval(&mut self, id: u64, allow: bool) -> Result<()> {
        let request = self
            .pending
            .remove(&id)
            .ok_or_else(|| anyhow::anyhow!("approval is no longer pending"))?;
        self.view
            .lock()
            .await
            .approvals
            .remove(&id);
        self.sync_user_input().await;
        request
            .decision
            .send(allow)
            .map_err(|_| anyhow::anyhow!("approval is no longer pending"))
    }

    async fn approval(&mut self, request: ApprovalRequest) {
        if !self.busy() {
            let _ = request.decision.send(false);
            return;
        }
        if self.allows(&request.preview) {
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
        self.pending
            .insert(self.approval_id, request);
    }

    /// Publish a fresh execution boundary before its queued input events are delivered.
    async fn activate_run(
        &mut self,
        started: super::super::run::Started,
        prompt: String,
        mut presentation: orbit_protocol::RunPresentation,
    ) {
        let (events, task, cancellation, inbox, image_assets, file_assets) = started;
        presentation.image_assets = image_assets;
        presentation.file_assets = file_assets;
        self.running = Some(Running {
            events,
            task,
            cancellation,
            inbox,
        });
        self.profile().await;
        let mut view = self.view.lock().await;
        view.busy = true;
        view.clock = Some(super::Clock::new());
        view.push(Update::Started {
            prompt,
            presentation,
        });
    }

    /// Continue only durable, unclaimed input after an explicit stop, with a fresh token.
    async fn continue_queued(&mut self) -> Result<()> {
        if self
            .setup
            .session
            .store
            .pending_agent_messages(self.setup.session.id())
            .await?
            .is_empty()
        {
            return Ok(());
        }
        let started =
            super::super::run::start(&mut self.setup, None, Vec::new(), Vec::new(), false, None)
                .await?;
        self.accepted.clear();
        self.activate_run(
            started,
            String::new(),
            orbit_protocol::RunPresentation {
                queued_input: true,
                ..Default::default()
            },
        )
        .await;
        Ok(())
    }

    async fn finish_run(&mut self) {
        let run = self.running.take().unwrap();
        let continue_queued = std::mem::take(&mut self.continue_after_cancel);
        self.pending.clear();
        let (status, event, history) =
            crate::execution::outcome(run.task.await, Some(&run.cancellation));
        let error = match event {
            AgentEvent::Failed { error } => Some(error),
            _ => None,
        };
        let saved = self
            .setup
            .session
            .finish(status, history)
            .await;
        if let Ok(mut workspace) = self.workspace.write() {
            *workspace = self.setup.resources.workspace.clone();
        }
        self.questions.update(&self.setup);
        let continue_queued = continue_queued && saved.is_ok();
        let status = if saved.is_ok() { status } else { "failed" };
        let mut view = self.view.lock().await;
        if let Some(error) = error.or_else(|| {
            saved
                .err()
                .map(|error| error.to_string())
        }) {
            view.push(Update::agent(AgentEvent::Failed { error }));
        }
        self.progress.clear();
        self.progress_task = None;
        view.progress.clear();
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
        drop(view);
        if continue_queued && let Err(error) = self.continue_queued().await {
            self.view
                .lock()
                .await
                .push(Update::Report(orbit_protocol::Report {
                    title: "暂存消息".into(),
                    text: format!("未能启动暂存消息，消息已保留：{error:#}"),
                }));
        }
    }

    async fn undo(&mut self, path: Option<String>) -> Result<()> {
        ensure!(
            !self
                .setup
                .resources
                .policy
                .disabled(orbit_protocol::Capability::Files),
            "文件功能已禁用"
        );
        ensure!(
            !self
                .setup
                .resources
                .policy
                .disabled(orbit_protocol::Capability::Write),
            "文件写入已禁用"
        );
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
                !change.undone
                    && path
                        .as_ref()
                        .is_none_or(|path| path == &change.change.path)
            })
            .ok_or_else(|| anyhow::anyhow!("当前会话没有匹配的可撤销写入"))?;
        let writer = self.setup.resources.undo_writer.clone();
        let store = self.setup.session.store.clone();
        let session = self.setup.session.id().to_owned();
        let cancellation = CancellationToken::new();
        let token = cancellation.clone();
        let task = tokio::spawn(async move {
            let output = writer
                .restore(change.change.clone(), token)
                .await?;
            store
                .record_workspace_undo(&session, &change)
                .await?;
            Ok(output)
        });
        self.undo = Some(Undo {
            task,
            cancellation,
            completion: None,
        });
        self.view.lock().await.busy = true;
        Ok(())
    }
}
