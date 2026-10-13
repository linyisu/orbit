//! The Tokio transport lives off the GPUI thread. Dropping a window disconnects
//! its client; it never sends Cancel to a running agent.
#[path = "backend/queue_images.rs"]
mod queue_images;
use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result, bail};
use base64::Engine;
use orbit_client::{
    ConnectionOptions,
    protocol::{Request, Response},
    transport::Client,
};
use tokio::sync::mpsc;

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, clap::Parser)]
#[command(name = "orbit-gui", about = "Orbit desktop client")]
pub(crate) struct Options {
    #[arg(long, default_value = "config.toml", env = "orbit_config")]
    pub(crate) config: PathBuf,
    #[arg(long)]
    pub(crate) workspace: Option<PathBuf>,
    #[arg(long)]
    pub(crate) server: Option<String>,
    #[arg(long)]
    pub(crate) database: Option<PathBuf>,
    #[arg(long)]
    pub(crate) auth_dir: Option<PathBuf>,
    #[arg(long)]
    pub(crate) model: Option<String>,
    /// Allow enabled MCP programs to start for this invocation.
    #[arg(long)]
    #[serde(skip)]
    pub(crate) allow_mcp_start: bool,
    #[arg(long)]
    pub(crate) resume: Option<String>,
    /// One-shot intent from selecting a replacement directory for a saved session.
    #[arg(skip)]
    #[serde(skip)]
    pub(crate) relink_on_resume: bool,
    /// A first send may have lost its acknowledgement before persistence was
    /// confirmed. Only this case may recover as a fresh draft if absent.
    #[arg(skip)]
    #[serde(default)]
    pub(crate) recover_draft: bool,
    /// Directory containing the Shell appearance panel; watched in debug builds.
    #[arg(long)]
    pub(crate) appearance: Option<PathBuf>,
    /// GUI preferences file; separate from agent configuration and credentials.
    #[arg(long)]
    pub(crate) preferences: Option<PathBuf>,
    #[arg(long, conflicts_with = "wsl")]
    pub(crate) ssh: Option<String>,
    #[arg(long, requires = "ssh")]
    #[serde(default)]
    pub(crate) ssh_user: Option<String>,
    #[arg(long, requires = "ssh", value_parser = clap::value_parser!(u16).range(1..))]
    #[serde(default)]
    pub(crate) ssh_port: Option<u16>,
    #[arg(long, requires = "ssh")]
    #[serde(default)]
    pub(crate) identity: Option<String>,
    #[arg(long, conflicts_with = "ssh")]
    pub(crate) wsl: Option<String>,
    #[arg(long, requires = "wsl")]
    #[serde(default)]
    pub(crate) wsl_user: Option<String>,
}

impl Options {
    pub(crate) fn resolve_paths(&mut self) -> Result<()> {
        let directory = std::env::current_dir()?;
        for path in std::iter::once(&mut self.config)
            .chain(self.auth_dir.iter_mut())
            .chain(self.preferences.iter_mut())
            .chain(self.appearance.iter_mut())
        {
            if path.is_relative() {
                *path = directory.join(&*path);
            }
        }
        // Workspace and database paths are interpreted by the target service.
        // Prefixing ~/project with the desktop's cwd corrupts a remote target.
        if self.ssh.is_none() && self.wsl.is_none() {
            for path in self.workspace.iter_mut().chain(self.database.iter_mut()) {
                if path.is_relative() {
                    *path = directory.join(&*path);
                }
            }
        }
        if let Some(identity) = &mut self.identity {
            let path = PathBuf::from(&*identity);
            if path.is_relative() && !identity.starts_with('~') {
                *identity = directory.join(path).to_string_lossy().into_owned();
            }
        }
        Ok(())
    }

    /// Session IDs and paths are meaningful only within the same service/data
    /// source. Never activate a local tab for an identically named remote one.
    pub(crate) fn same_host(&self, other: &Self) -> bool {
        // The service resolves an ordinary relative database against its
        // workspace. Home-relative paths instead identify one host-wide store.
        let workspace_database = (self.ssh.is_some() || self.wsl.is_some())
            && self.database.as_ref().is_some_and(|path| {
                path.is_relative()
                    && !path
                        .to_str()
                        .is_some_and(|path| path == "~" || path.starts_with("~/"))
            });
        self.ssh == other.ssh
            && self.ssh_user == other.ssh_user
            && self.ssh_port == other.ssh_port
            && self.identity == other.identity
            && self.wsl == other.wsl
            && self.wsl_user == other.wsl_user
            && self.server == other.server
            && self.database == other.database
            && (!workspace_database || self.workspace == other.workspace)
            && self.auth_dir == other.auth_dir
            && self.config == other.config
    }

    pub(crate) fn connection_options(&self) -> Result<ConnectionOptions> {
        anyhow::ensure!(
            self.ssh.is_none() || self.wsl.is_none(),
            "请选择一种远程连接方式"
        );
        let workspace = match &self.workspace {
            Some(path) => path.clone(),
            None if self.ssh.is_some() || self.wsl.is_some() => PathBuf::from("~"),
            None => std::env::current_dir()?,
        };
        let mut connection = ConnectionOptions::local(workspace.to_string_lossy().into_owned());
        connection.server = self.server.clone();
        connection.ssh = self.ssh.clone();
        connection.ssh_user = self.ssh_user.clone();
        connection.ssh_port = self.ssh_port;
        connection.identity = self.identity.clone();
        connection.wsl = self.wsl.clone();
        connection.wsl_user = self.wsl_user.clone();
        connection.allow_mcp_start = self.allow_mcp_start;
        Ok(connection)
    }
}

pub(crate) enum Command {
    Tasks {
        session: String,
        workspace: String,
        action: orbit_protocol::project::TaskAction,
    },
    Complete {
        session: String,
        input: String,
    },
    RefreshModels,
    CloseSubagent,
    ReadImage {
        session: String,
        path: String,
    },
    Request(Request),
    Settings {
        request: Request,
        result: tokio::sync::oneshot::Sender<Result<Response, String>>,
    },
    Send {
        session: String,
        id: String,
        prompt: String,
        paths: Vec<PathBuf>,
        steer: bool,
    },
}

pub(crate) enum Event {
    Tasks {
        session: String,
        result: Result<orbit_protocol::project::TaskSnapshot, String>,
    },
    ConnectionStage(String),
    ConnectionLogs(Vec<String>),
    Completions {
        session: String,
        input: String,
        result: Result<Vec<orbit_protocol::Completion>, String>,
    },
    Connected,
    Subagent {
        agent: String,
        page: orbit_client::protocol::Page,
        reset: bool,
    },
    Image {
        session: String,
        path: String,
        result: Result<Vec<u8>, String>,
    },
    Challenge(orbit_client::interaction::Challenge),
    File {
        path: String,
        bytes: Vec<u8>,
    },
    Usage {
        session: String,
        result: Result<orbit_protocol::usage::Snapshot, String>,
    },
    Response(Response),
    Sent {
        id: String,
    },
    SendFailed {
        id: String,
        error: String,
    },
    Error(String),
    Disconnected(String),
}

pub(crate) struct Backend {
    sender: mpsc::Sender<Command>,
    task: tokio::task::JoinHandle<()>,
}

impl Backend {
    pub(crate) fn start(
        options: Options,
        runtime: &tokio::runtime::Runtime,
    ) -> (Self, mpsc::Receiver<Event>) {
        let (sender, commands) = mpsc::channel(32);
        let (events, receiver) = mpsc::channel(64);
        let task = runtime.spawn(async move {
            if let Err(error) = Box::pin(run(options, commands, &events)).await {
                let _ = events.send(Event::Disconnected(format!("{error:#}"))).await;
            }
        });
        (Self { sender, task }, receiver)
    }

    pub(crate) fn send(&self, command: Command) -> Result<()> {
        self.sender
            .try_send(command)
            .map_err(|_| anyhow::anyhow!("连接繁忙或已断开，请稍后重试。"))
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn run(
    options: Options,
    commands: mpsc::Receiver<Command>,
    events: &mpsc::Sender<Event>,
) -> Result<()> {
    let (progress, mut updates) = orbit_client::interaction::channel();
    let logs = updates.logs.clone();
    let initial_stage = updates.stage.borrow_and_update().clone();
    events.send(Event::ConnectionStage(initial_stage)).await?;
    let prompts = events.clone();
    let mut forwarding = AbortTask(tokio::spawn(async move {
        let mut last_logs = Vec::new();
        let mut interval = tokio::time::interval(Duration::from_millis(200));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let logs = log_snapshot(&updates.logs);
                    if logs != last_logs {
                        last_logs.clone_from(&logs);
                        if prompts.send(Event::ConnectionLogs(logs)).await.is_err() { break; }
                    }
                }
                changed = updates.stage.changed() => {
                    if changed.is_err() { break; }
                    let stage = updates.stage.borrow_and_update().clone();
                    if prompts.send(Event::ConnectionStage(stage)).await.is_err() { break; }
                }
                challenge = updates.prompts.recv() => {
                    let Some(challenge) = challenge else { break; };
                    if prompts.send(Event::Challenge(challenge)).await.is_err() { break; }
                }
            }
        }
    }));
    // The transport loop's future is large in debug builds. Do not embed its
    // state machine in this wrapper and again in the runtime's spawned future.
    let result = Box::pin(run_connection(options, commands, events, progress)).await;
    forwarding.0.abort();
    let _ = (&mut forwarding.0).await;
    // The diagnostics reader may append immediately before a connect error.
    // Flush after the forwarding task stops, before Disconnected is delivered.
    let _ = events
        .send(Event::ConnectionLogs(log_snapshot(&logs)))
        .await;
    result
}

fn log_snapshot(logs: &std::sync::Mutex<std::collections::VecDeque<String>>) -> Vec<String> {
    logs.lock()
        .map(|logs| logs.iter().cloned().collect())
        .unwrap_or_default()
}

async fn run_connection(
    options: Options,
    mut commands: mpsc::Receiver<Command>,
    events: &mpsc::Sender<Event>,
    progress: orbit_client::interaction::Progress,
) -> Result<()> {
    let queue_cache = queue_images::cache_database(&options)?;
    let connection_options = options.connection_options()?;
    let connection = orbit_client::prepare_connection(
        &connection_options,
        options.config,
        options.database,
        options.auth_dir,
        Some(progress.clone()),
    )
    .await?;
    progress.report("启动工作区服务并等待握手");
    let mut client = connection.connect_progress(Some(&progress)).await?;
    events.send(Event::Connected).await?;
    progress.report("读取会话并打开工作区");
    let mut history = client.request(Request::AllSessions).await?;
    if options.relink_on_resume {
        anyhow::ensure!(
            options.ssh.is_none() && options.wsl.is_none(),
            "只能重新关联本地项目"
        );
        let id = options
            .resume
            .as_deref()
            .context("重新关联项目需要原会话")?;
        let Response::Sessions(sessions) = &history else {
            bail!("无法读取原会话的项目归属");
        };
        let saved = sessions
            .iter()
            .find(|session| session.id == id)
            .context("原会话已不存在，无法重新关联项目")?;
        // Renames retaining filesystem identity are already resolved by AllSessions.
        // Explicit linking also covers copies/moves across filesystems, where that
        // identity changes. Use the store's checks instead of bypassing ownership.
        if saved.workspace != connection.workspace {
            client
                .request(Request::Admin {
                    action: orbit_protocol::admin::Action::LinkProject {
                        id: saved.project_id.clone(),
                    },
                })
                .await
                .context("无法将原项目关联到所选目录")?;
            history = client.request(Request::AllSessions).await?;
        }
    }
    let resume = recovered_session(options.resume, options.recover_draft, &history)?;
    events.send(Event::Response(history)).await?;
    // The service preserves a valid historical model. Send today's default as
    // its fallback instead of blindly recycling a stale tab's launch model.
    let preferred = if resume.is_some() {
        match &connection.local_configuration {
            // A missing/corrupt local preference is not an error in the saved
            // conversation. The service can still choose a configured model.
            Some(local) => local
                .selected_model()
                .await
                .unwrap_or_default()
                .or(options.model),
            None => options.model,
        }
    } else {
        options.model
    };
    let model = match preferred {
        Some(model) => Some(model),
        None => match &connection.local_configuration {
            Some(local) => match local.selected_model().await? {
                Some(model) => Some(model),
                None => local
                    .model_catalog()
                    .await?
                    .first()
                    .map(|model| model.qualified_name()),
            },
            None => None,
        },
    };
    let mut session = String::new();
    let mut cursor = 0;
    let initial = Request::Open {
        model: model.clone(),
        policy: Default::default(),
        resume,
        configuration: connection.configuration().await?,
    };
    let opened = client.request(initial).await?;
    if let Response::Error(error) = opened {
        bail!(error);
    }
    deliver(opened, &mut session, &mut cursor, events).await?;
    progress.report("已连接");
    let mut interval = tokio::time::interval(Duration::from_millis(80));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut quota_tasks = tokio::task::JoinSet::new();
    let mut review_tasks = tokio::task::JoinSet::new();
    let mut queue_images = std::collections::HashMap::new();
    let mut questions_revision = None;
    let mut subagent: Option<(String, usize)> = None;
    let mut last_page = None;
    // A paged transcript is already available: drain it without the live
    // polling delay, while keeping commands ahead of each page request.
    let mut backlog = true;
    loop {
        tokio::select! {
            biased;
            command = commands.recv() => {
                let Some(command) = command else { return Ok(()) };
                match command {
                    Command::Complete { session, input } => {
                        let result = match client.request(Request::Complete { session: session.clone(), input: input.clone() }).await {
                            Ok(Response::Completions { entries, .. }) => Ok(entries),
                            Ok(_) => Err("无法读取参数候选".into()),
                            Err(error) => Err(format!("{error:#}")),
                        };
                        events.send(Event::Completions { session, input, result }).await?;
                    }
                    Command::CloseSubagent => { subagent = None; }
                    Command::RefreshModels => {
                        let result = async {
                            let local = connection.local_configuration.as_ref().context("当前连接没有本地模型配置")?;
                            let configuration = local.snapshot_with_refresh(true).await?;
                            client.request(Request::ConfigureModels { session: session.clone(), configuration: Some(configuration) }).await
                        }.await;
                        match result {
                            Ok(response) => { deliver(response, &mut session, &mut cursor, events).await?; }
                            Err(error) => { events.send(Event::Error(format!("{error:#}"))).await?; }
                        }
                    }
                    Command::ReadImage { session, path } => {
                        let result = match client.request(Request::ReadFile { session: session.clone(), path: path.clone() }).await {
                            Ok(Response::File(content)) => base64::engine::general_purpose::STANDARD.decode(content).map_err(|error| error.to_string()),
                            Ok(Response::Error(error)) => Err(error),
                            Err(error) => Err(format!("{error:#}")),
                            _ => Err("无法读取图片".into()),
                        };
                        events.send(Event::Image { session, path, result }).await?;
                    }
                    Command::Request(Request::Usage { session }) => {
                        if quota_tasks.is_empty() {
                            let connection = connection.clone();
                            let events = events.clone();
                            quota_tasks.spawn(async move {
                                let result = async {
                                    let mut client = connection.connect().await?;
                                    match client.request(Request::Usage { session: session.clone() }).await? {
                                        Response::Usage(snapshot) => Ok(snapshot),
                                        Response::Error(error) => bail!(error),
                                        _ => bail!("无法读取账号额度"),
                                    }
                                }.await.map_err(|error: anyhow::Error| format!("{error:#}"));
                                let _ = events.send(Event::Usage { session, result }).await;
                            });
                        }
                    }
                    Command::Tasks { session, workspace, action } => {
                        let result = match client.request(Request::Tasks { session: session.clone(), expected_workspace: Some(workspace), action }).await {
                            Ok(Response::Tasks(tasks)) => Ok(tasks),
                            Ok(Response::Error(error)) => Err(error),
                            Ok(_) => Err("服务没有返回任务数据，请更新工作区服务后重试。".into()),
                            Err(error) => Err(format!("{error:#}")),
                        };
                        events.send(Event::Tasks { session, result }).await?;
                    }
                    Command::Settings { request, result } => {
                        if matches!(request, Request::Review { action: orbit_protocol::review::Action::Undo { .. } | orbit_protocol::review::Action::Stage { .. } | orbit_protocol::review::Action::Unstage { .. }, .. }) {
                            // Undo may wait for a permission decision. Keep the
                            // main connection polling approvals and accepting Cancel.
                            let connection = connection.clone();
                            review_tasks.spawn(async move {
                                let response = async {
                                    let mut client = connection.connect().await.map_err(|error| format!("{error:#}"))?;
                                    match client.request(request).await {
                                        Ok(Response::Error(error)) => Err(error),
                                        Ok(response) => Ok(response),
                                        Err(error) => Err(format!("{error:#}")),
                                    }
                                }.await;
                                let _ = result.send(response);
                            });
                            continue;
                        }
                        let queue_session = match &request { Request::InputQueue { session, .. } => Some(session.clone()), _ => None };
                        let response = match client.request(request).await {
                            Ok(Response::Error(error)) => Err(error),
                            Ok(mut response) => {
                                if let Some(session) = queue_session {
                                    queue_images::localize(&mut client, &session, &mut response, &mut queue_images, &queue_cache).await.map(|()| response).map_err(|error| format!("{error:#}"))
                                } else { Ok(response) }
                            },
                            Err(error) => Err(format!("{error:#}")),
                        };
                        let _ = result.send(response);
                    }
                    Command::Request(request) => {
                        if matches!(request, Request::CloseQuestions { .. }) { questions_revision = None; }
                        let remember = matches!(request, Request::SetModel { .. });
                        let agent = if let Request::ReadSubagent { agent, .. } = &request { Some(agent.clone()) } else { None };
                        let file_path = if let Request::ReadFile { path, .. } = &request { Some(path.clone()) } else { None };
                        if matches!(request, Request::SetModel { .. }) { quota_tasks.shutdown().await; }
                        match client.request(request).await {
                            Ok(response) => {
                                if let Some(agent) = agent && matches!(response, Response::Subagent(_)) {
                                    let Response::Subagent(page) = response else { unreachable!() };
                                    subagent = Some((agent.clone(), page.next));
                                    events.send(Event::Subagent { agent, page, reset: true }).await?;
                                    continue;
                                }
                                if remember && let Response::Profile(info) = &response && let Some(local) = &connection.local_configuration
                                    && let Err(error) = local.remember_model(&info.model) {
                                    events.send(Event::Error(format!("模型已切换，但无法保存偏好：{error}"))).await?;
                                }
                                if let Response::Questions(questions) = &response { questions_revision = Some(questions.revision); }
                                if let (Some(path), Response::File(content)) = (file_path, &response) {
                                    match base64::engine::general_purpose::STANDARD.decode(content) {
                                        Ok(bytes) => { events.send(Event::File { path, bytes }).await?; }
                                        Err(error) => { events.send(Event::Error(format!("无法解码文件：{error}"))).await?; }
                                    }
                                } else { deliver(response, &mut session, &mut cursor, events).await?; }
                            },
                            Err(error) => { events.send(Event::Error(format!("{error:#}"))).await?; }
                        }
                    }
                    Command::Send { session, id, prompt, paths, steer } => {
                        let result = send(&mut client, session, id.clone(), prompt, paths, steer).await;
                        let event = match result {
                            Ok(()) => Event::Sent { id },
                            Err(error) => Event::SendFailed { id, error: format!("{error:#}") },
                        };
                        events.send(event).await?;
                    }
                }
                if client.failed() { bail!("工作区连接已断开。") }
            }
            _ = quota_tasks.join_next(), if !quota_tasks.is_empty() => {}
            _ = review_tasks.join_next(), if !review_tasks.is_empty() => {}
            _ = async { if !backlog { interval.tick().await; } }, if !session.is_empty() => {
                let response = client.request(Request::Poll { session: session.clone(), after: cursor }).await?;
                backlog = matches!(&response, Response::Page(page) if page.more);
                if let Response::Page(page) = &response {
                    let state = serde_json::to_string(&(page.more, page.busy, &page.model, &page.approvals, &page.planning, &page.user_inputs, &page.tool_progress))?;
                    let unchanged = !page.more && page.updates.is_empty() && last_page.as_ref() == Some(&state);
                    last_page = Some(state);
                    if !unchanged { deliver(response, &mut session, &mut cursor, events).await?; }
                } else { deliver(response, &mut session, &mut cursor, events).await?; }
                if let Some(revision) = questions_revision
                    && let Response::Questions(questions) = client.request(Request::ReadQuestions { session: session.clone(), after: Some(revision) }).await? {
                        questions_revision = Some(questions.revision);
                        if questions.records.is_some() { events.send(Event::Response(Response::Questions(questions))).await?; }
                }
                if let Some((agent, after)) = subagent.clone() {
                    match client.request(Request::ReadSubagent { session: session.clone(), agent: agent.clone(), after }).await? {
                        Response::Subagent(page) => {
                            subagent = Some((agent.clone(), page.next));
                            if !page.updates.is_empty() { events.send(Event::Subagent { agent, page, reset: false }).await?; }
                        }
                        Response::Error(error) => { subagent = None; events.send(Event::Error(error)).await?; }
                        _ => {}
                    }
                }
            }
        }
    }
}

struct AbortTask(tokio::task::JoinHandle<()>);
impl Drop for AbortTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn send(
    client: &mut Client,
    session: String,
    id: String,
    prompt: String,
    paths: Vec<PathBuf>,
    steer: bool,
) -> Result<()> {
    let images = tokio::task::spawn_blocking(move || {
        orbit_client::attachments::uploads(&orbit_client::attachments::paths(paths)?)
    })
    .await
    .context("读取图片失败")??;
    let request = if steer {
        if images.is_empty() {
            Request::Steer {
                session,
                message: prompt,
            }
        } else {
            Request::SteerImages {
                session,
                message: prompt,
                images,
            }
        }
    } else {
        Request::Start {
            session,
            request_id: id,
            prompt,
            images,
            compact: false,
            model: None,
        }
    };
    match client.request(request).await? {
        Response::Ok | Response::InputQueued { .. } => Ok(()),
        Response::Error(error) => bail!(error),
        _ => bail!("服务未确认发送，请检查会话状态。"),
    }
}

fn recovered_session(
    resume: Option<String>,
    recover_draft: bool,
    history: &Response,
) -> Result<Option<String>> {
    if !recover_draft {
        return Ok(resume);
    }
    let Response::Sessions(sessions) = history else {
        bail!("无法确认未发送任务的会话记录");
    };
    Ok(resume.filter(|id| sessions.iter().any(|session| &session.id == id)))
}

async fn deliver(
    response: Response,
    session: &mut String,
    cursor: &mut usize,
    events: &mpsc::Sender<Event>,
) -> Result<()> {
    match &response {
        Response::Opened(info) => {
            *session = info.id.clone();
            *cursor = 0;
        }
        Response::Page(page) => {
            *cursor = page.next;
        }
        _ => {}
    }
    events.send(Event::Response(response)).await?;
    Ok(())
}
