//! Session observation, background reads and receipt routing shared by frontends.
use super::{catalogue, queue_attachments};
use crate::attachments::send;
use crate::{
    protocol::{Request, Response},
    transport::Connection,
};
use anyhow::{Context, Result, bail};
use base64::Engine;
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::sync::mpsc;

#[cfg(all(test, unix))]
mod tests;

/// Session opening intent; paths and connection preparation have already been resolved.
pub struct SessionOptions {
    pub connection: Arc<Connection>,
    pub resume: Option<String>,
    pub model: Option<String>,
    pub relink_on_resume: bool,
    pub recover_draft: bool,
    pub attachment_cache: PathBuf,
}

pub enum Command {
    WatchAgents(Option<(String, String)>),
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
        paths: Vec<(PathBuf, String)>,
        steer: bool,
    },
}

pub enum Event {
    AgentStatuses {
        session: String,
        workspace: String,
        agents: Vec<orbit_protocol::subagents::Agent>,
    },
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
        page: crate::protocol::Page,
        reset: bool,
    },
    Image {
        session: String,
        path: String,
        result: Result<Vec<u8>, String>,
    },
    Challenge(crate::interaction::Challenge),
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

/// Observe a session off the UI thread; dropping the caller disconnects without cancelling it.
pub async fn observe(
    options: SessionOptions,
    mut commands: mpsc::Receiver<Command>,
    events: &mpsc::Sender<Event>,
    progress: crate::interaction::Progress,
) -> Result<()> {
    let queue_cache = options.attachment_cache.clone();
    let connection = options.connection.clone();
    progress.report("启动工作区服务并等待握手");
    let mut client = connection
        .connect_progress(Some(&progress))
        .await?;
    events.send(Event::Connected).await?;
    progress.report("读取会话并打开工作区");
    let mut history = client
        .request(Request::AllSessions)
        .await?;
    if options.relink_on_resume {
        anyhow::ensure!(
            matches!(connection.target, crate::transport::Target::Local),
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
            history = client
                .request(Request::AllSessions)
                .await?;
        }
    }
    let resume = recovered_session(options.resume, options.recover_draft, &history)?;
    events
        .send(Event::Response(history))
        .await?;
    let info =
        crate::connect::open_session(&mut client, &connection, resume, options.model).await?;
    let mut session = String::new();
    let mut cursor = 0;
    let opened = Response::Opened(info);
    deliver(opened, &mut session, &mut cursor, events).await?;
    progress.report("已连接");
    let mut interval = tokio::time::interval(Duration::from_millis(80));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut quota_tasks = tokio::task::JoinSet::new();
    let mut review_tasks = tokio::task::JoinSet::new();
    let mut model_tasks = tokio::task::JoinSet::new();
    let mut queue_images = std::collections::HashMap::new();
    let mut questions_revision = None;
    let mut subagent: Option<(String, usize)> = None;
    let mut agent_watch = catalogue::Watch::default();
    let mut page_state = super::PollState::default();
    // A paged transcript is already available: drain it without the live
    // polling delay, while keeping commands ahead of each page request.
    let mut backlog = true;
    loop {
        tokio::select! {
            biased;
            command = commands.recv() => {
                let Some(command) = command else {
                    return Ok(());
                };
                match command {
                    Command::WatchAgents(scope) => agent_watch.set(scope),
                    Command::Complete { session, input } => {
                        let result = match client
                            .request(Request::Complete {
                                session: session.clone(),
                                input: input.clone(),
                            })
                            .await
                        {
                            Ok(Response::Completions { entries, .. }) => Ok(entries),
                            Ok(_) => Err("无法读取参数候选".into()),
                            Err(error) => Err(format!("{error:#}")),
                        };
                        events
                            .send(Event::Completions {
                                session,
                                input,
                                result,
                            })
                            .await?;
                    }
                    Command::CloseSubagent => {
                        subagent = None;
                    }
                    Command::RefreshModels => {
                        if model_tasks.is_empty() {
                            let connection = connection.clone();
                            let session = session.clone();
                            model_tasks.spawn(async move {
                                let result = async {
                                    let local = connection
                                        .local_configuration
                                        .as_ref()
                                        .context("当前连接没有本地模型配置")?;
                                    local.snapshot_with_refresh(true).await
                                }
                                .await;
                                (session, result)
                            });
                        }
                    }
                    Command::ReadImage { session, path } => {
                        let result = match client
                            .request(Request::ReadFile {
                                session: session.clone(),
                                path: path.clone(),
                                allow_missing: false,
                            })
                            .await
                        {
                            Ok(Response::File(content)) => {
                                base64::engine::general_purpose::STANDARD
                                    .decode(content)
                                    .map_err(|error| error.to_string())
                            }

                            Err(error) => Err(format!("{error:#}")),
                            _ => Err("无法读取图片".into()),
                        };
                        events
                            .send(Event::Image {
                                session,
                                path,
                                result,
                            })
                            .await?;
                    }
                    Command::Request(Request::Usage { session }) => {
                        if quota_tasks.is_empty() {
                            let connection = connection.clone();
                            let events = events.clone();
                            quota_tasks.spawn(async move {
                                let result = async {
                                    crate::usage::fetch(&connection, session.clone()).await
                                }
                                .await
                                .map_err(|error: anyhow::Error| format!("{error:#}"));
                                let _ = events
                                    .send(Event::Usage { session, result })
                                    .await;
                            });
                        }
                    }
                    Command::Tasks {
                        session,
                        workspace,
                        action,
                    } => {
                        let result = match client
                            .request(Request::Tasks {
                                session: session.clone(),
                                expected_workspace: Some(workspace),
                                action,
                            })
                            .await
                        {
                            Ok(Response::Tasks(tasks)) => Ok(tasks),
                            Ok(_) => Err("服务没有返回任务数据，请更新工作区服务后重试。".into()),
                            Err(error) => Err(format!("{error:#}")),
                        };
                        events
                            .send(Event::Tasks { session, result })
                            .await?;
                    }
                    Command::Settings { request, result } => {
                        if super::independent(&request) || crate::library::routed(&request) {
                            // Long settings requests use a separate connection so
                            // chat keeps polling events and accepting decisions.
                            let connection = connection.clone();
                            review_tasks.spawn(async move {
                                let response = async {
                                    let mut client = connection
                                        .connect()
                                        .await
                                        .map_err(|error| format!("{error:#}"))?;
                                    match client.request(request).await {
                                        Ok(response) => Ok(response),
                                        Err(error) => Err(format!("{error:#}")),
                                    }
                                }
                                .await;
                                let _ = result.send(response);
                            });
                            continue;
                        }
                        let queue_session = match &request {
                            Request::InputQueue { session, .. } => Some(session.clone()),
                            _ => None,
                        };
                        let response = match client.request(request).await {
                            Ok(mut response) => {
                                if let Some(session) = queue_session {
                                    queue_attachments::localize(
                                        &mut client,
                                        &session,
                                        &mut response,
                                        &mut queue_images,
                                        &queue_cache,
                                    )
                                    .await
                                    .map(|()| response)
                                    .map_err(|error| format!("{error:#}"))
                                } else {
                                    Ok(response)
                                }
                            }
                            Err(error) => Err(format!("{error:#}")),
                        };
                        let _ = result.send(response);
                    }
                    Command::Request(request) => {
                        if crate::library::routed(&request) {
                            let connection = connection.clone();
                            let events = events.clone();
                            review_tasks.spawn(async move {
                                let response = async {
                                    let mut client = connection.connect().await?;
                                    Ok::<_, anyhow::Error>(client.request(request).await?)
                                }.await;
                                let event = match response {
                                    Ok(response) => Event::Response(response),
                                    Err(error) => Event::Error(format!("{error:#}")),
                                };
                                let _ = events.send(event).await;
                            });
                            continue;
                        }
                        if matches!(request, Request::CloseQuestions { .. }) {
                            questions_revision = None;
                        }
                        let remember = matches!(request, Request::SetModel { .. });
                        let agent = if let Request::ReadSubagent { agent, .. } = &request {
                            Some(agent.clone())
                        } else {
                            None
                        };
                        let file_path = if let Request::ReadFile { path, .. } = &request {
                            Some(path.clone())
                        } else {
                            None
                        };
                        if matches!(request, Request::SetModel { .. }) {
                            quota_tasks.shutdown().await;
                        }
                        match client.request(request).await {
                            Ok(response) => {
                                if let Some(agent) = agent
                                    && matches!(response, Response::Subagent(_))
                                {
                                    let Response::Subagent(page) = response else {
                                        unreachable!()
                                    };
                                    subagent = Some((agent.clone(), page.next));
                                    events
                                        .send(Event::Subagent {
                                            agent,
                                            page,
                                            reset: true,
                                        })
                                        .await?;
                                    continue;
                                }
                                if remember
                                    && let Response::Profile(info) = &response
                                    && let Some(local) = &connection.local_configuration
                                    && let Err(error) = local.remember_model(&info.model)
                                {
                                    events
                                        .send(Event::Error(format!(
                                            "模型已切换，但无法保存偏好：{error}"
                                        )))
                                        .await?;
                                }
                                if let Response::Questions(questions) = &response {
                                    questions_revision = Some(questions.revision);
                                }
                                if let (Some(path), Response::File(content)) =
                                    (file_path, &response)
                                {
                                    match base64::engine::general_purpose::STANDARD.decode(content)
                                    {
                                        Ok(bytes) => {
                                            events
                                                .send(Event::File { path, bytes })
                                                .await?;
                                        }
                                        Err(error) => {
                                            events
                                                .send(Event::Error(format!(
                                                    "无法解码文件：{error}"
                                                )))
                                                .await?;
                                        }
                                    }
                                } else {
                                    deliver(response, &mut session, &mut cursor, events).await?;
                                }
                            }
                            Err(error) => {
                                events
                                    .send(Event::Error(format!("{error:#}")))
                                    .await?;
                            }
                        }
                    }
                    Command::Send {
                        session,
                        id,
                        prompt,
                        paths,
                        steer,
                    } => {
                        let result =
                            send(&mut client, session, id.clone(), prompt, paths, steer).await;
                        let event = match result {
                            Ok(()) => Event::Sent { id },
                            Err(error) => Event::SendFailed {
                                id,
                                error: format!("{error:#}"),
                            },
                        };
                        events.send(event).await?;
                    }
                }
                if client.failed() {
                    bail!("工作区连接已断开。")
                }
            }
            _ = quota_tasks.join_next(), if !quota_tasks.is_empty() => {}
            _ = review_tasks.join_next(), if !review_tasks.is_empty() => {}
            Some(result) = model_tasks.join_next(), if !model_tasks.is_empty() => {
                let configuration = match result {
                    Ok((scope, result)) if scope == session => result,
                    Ok(_) => continue,
                    Err(error) => Err(error.into()),
                };
                let result = match configuration {
                    Ok(configuration) => client
                        .request(Request::ConfigureModels {
                            session: session.clone(),
                            configuration: Some(configuration),
                        })
                        .await
                        .map_err(anyhow::Error::new),
                    Err(error) => Err(error),
                };
                match result {
                    Ok(response) => {
                        deliver(response, &mut session, &mut cursor, events).await?;
                    }
                    Err(error) => {
                        events.send(Event::Error(format!("{error:#}"))).await?;
                    }
                }
                if client.failed() {
                    bail!("工作区连接已断开。")
                }
            }
            _ = async {
                if !backlog {
                    interval.tick().await;
                }
            }, if !session.is_empty() => {
                let response = client
                    .request(Request::Poll {
                        session: session.clone(),
                        after: cursor,
                    })
                    .await?;
                backlog = matches!(&response, Response::Page(page) if page.more);
                if let Response::Page(page) = &response {
                    if page_state.changed(page)? {
                        deliver(response, &mut session, &mut cursor, events).await?;
                    }
                } else {
                    deliver(response, &mut session, &mut cursor, events).await?;
                }
                if let Some(revision) = questions_revision
                    && let Response::Questions(questions) = client
                        .request(Request::ReadQuestions {
                            session: session.clone(),
                            after: Some(revision),
                        })
                        .await?
                {
                    questions_revision = Some(questions.revision);
                    if questions.records.is_some() {
                        events
                            .send(Event::Response(Response::Questions(questions)))
                            .await?;
                    }
                }
                if let Some((agent, after)) = subagent.clone() {
                    match client
                        .request(Request::ReadSubagent {
                            session: session.clone(),
                            agent: agent.clone(),
                            after,
                        })
                        .await
                    {
                        Ok(Response::Subagent(page)) => {
                            subagent = Some((agent.clone(), page.next));
                            if !page.updates.is_empty() {
                                events
                                    .send(Event::Subagent {
                                        agent,
                                        page,
                                        reset: false,
                                    })
                                    .await?;
                            }
                        }
                        Err(crate::transport::RequestError::Remote(error)) => {
                            subagent = None;
                            events.send(Event::Error(error)).await?;
                        }
                        Err(error) => return Err(error.into()),
                        _ => {}
                    }
                }
                if !backlog {
                    agent_watch
                        .poll(&mut client, &session, events)
                        .await?;
                }
            }
        }
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
    Ok(resume.filter(|id| {
        sessions
            .iter()
            .any(|session| &session.id == id)
    }))
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
    events
        .send(Event::Response(response))
        .await?;
    Ok(())
}
