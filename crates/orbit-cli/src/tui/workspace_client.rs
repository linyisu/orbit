mod input;
mod pump;
mod standalone;
#[cfg(test)]
mod tests;

use std::{sync::Arc, time::Duration};

use anyhow::Result;
use tokio::sync::mpsc;
use tokio_util::task::AbortOnDropHandle;

use super::{
    app::{RunStatus, TranscriptItem, UiState},
    media,
};
use crate::workspace::{
    self, Call, Files,
    connect::{Opened, Spec},
    protocol::{Mode, Request, Response, SessionInfo, Update},
    transport::{Client, Connection},
};

const CHANNEL_CAPACITY: usize = 16;
const HELP: &str = "/ssh HOST [PATH]、/wsl DISTRO [PATH] 连接新环境；/new 新建当前环境会话，/local 新建本地标签，/sessions 查看目标历史，/resume ID 在新标签恢复，/reconnect 重连，/close 断开当前标签。/model、/mode 切换模型与权限。";

pub(super) enum Action {
    None,
    Close,
    Quit,
    Local,
    Open(Box<Spec>),
    SameTarget(Option<String>),
    Redraw,
}

pub(super) struct Tab {
    pub state: UiState,
    pub title: String,
    pub id: Option<String>,
    pub connection: Option<Arc<Connection>>,
    spec: Option<Spec>,
    connecting: Option<AbortOnDropHandle<Result<Opened>>>,
    worker: Option<AbortOnDropHandle<()>>,
    commands: Option<Arc<mpsc::Sender<Call>>>,
    incoming: Option<mpsc::Receiver<Result<Response>>>,
    busy: bool,
    starting: Option<String>,
    approval_id: Option<u64>,
    selected_model: Option<String>,
    mode: Mode,
    offline: bool,
}

impl Tab {
    pub fn connecting(spec: Spec, media: media::Context) -> Self {
        let mut tab = Self::empty(spec.label(), spec.workspace().into(), media);
        tab.spec = Some(spec.clone());
        tab.connecting = Some(AbortOnDropHandle::new(tokio::spawn(spec.open())));
        tab
    }

    fn empty(label: String, workspace: String, media: media::Context) -> Self {
        let mut state = UiState::new("连接中");
        state.media = media;
        state.connection_label = Some(label.clone());
        state.workspace = workspace;
        state.report_local(TranscriptItem::Notice(format!("正在连接 {label}…")));
        Self {
            state,
            title: label,
            id: None,
            connection: None,
            spec: None,
            connecting: None,
            worker: None,
            commands: None,
            incoming: None,
            busy: false,
            starting: None,
            approval_id: None,
            selected_model: None,
            mode: Mode::Auto,
            offline: false,
        }
    }

    pub fn ready(opened: Opened, media: media::Context) -> Self {
        let mut tab = Self::empty(
            opened.connection.target.label(),
            opened.info.workspace.clone(),
            media,
        );
        tab.install(opened);
        tab
    }

    pub fn sibling(&self, resume: Option<String>) -> Self {
        if let Some(connection) = &self.connection {
            let mut tab = Self::empty(
                connection.target.label(),
                self.state.workspace.clone(),
                self.state.media.clone(),
            );
            tab.connection = Some(connection.clone());
            let connection = connection.clone();
            tab.connecting = Some(AbortOnDropHandle::new(tokio::spawn(Opened::connect(
                connection, resume,
            ))));
            tab
        } else {
            Self::connecting(
                self.spec
                    .as_ref()
                    .expect("connecting tab has a spec")
                    .with_resume(resume),
                self.state.media.clone(),
            )
        }
    }

    fn install(&mut self, opened: Opened) {
        let draft = self.state.input.text.clone();
        let media = self.state.media.clone();
        let cursor_trail = self.state.cursor_motion.enabled;
        self.state = UiState::new(&opened.info.model);
        self.state.media = media;
        self.state.cursor_motion.enabled = cursor_trail;
        self.state.input.set(draft);
        self.state.connection_label = Some(opened.connection.target.label());
        self.state.workspace = opened.info.workspace.clone();
        self.profile(&opened.info);
        self.id = Some(opened.info.id.clone());
        let mut connection = opened.connection;
        Arc::make_mut(&mut connection).unattended = true;
        self.connection = Some(connection);
        self.title = format!(
            "{} · {}",
            self.state.connection_label.as_deref().unwrap_or_default(),
            std::path::Path::new(&self.state.workspace)
                .file_name()
                .and_then(|p| p.to_str())
                .unwrap_or(&self.state.workspace)
        );
        let (commands, requests) = mpsc::channel(CHANNEL_CAPACITY);
        let commands = Arc::new(commands);
        self.state.media.files = Some(Files {
            requests: commands.clone(),
            session: opened.info.id.clone(),
        });
        self.commands = Some(commands);
        let (responses, incoming) = mpsc::channel(CHANNEL_CAPACITY);
        self.incoming = Some(incoming);
        self.worker = Some(AbortOnDropHandle::new(tokio::spawn(pump::pump(
            opened.client,
            opened.info.id.clone(),
            requests,
            responses,
        ))));
        self.busy = false;
        self.starting = None;
        self.approval_id = None;
        self.state.permission_mode = match self.mode {
            Mode::Auto => super::permissions::Mode::Auto,
            Mode::Ask => super::permissions::Mode::Ask,
            Mode::Yolo => super::permissions::Mode::Yolo,
        };
        self.offline = false;
        self.state.report_local(TranscriptItem::Notice(format!(
            "工作区会话 {} · /help 查看连接命令",
            opened.info.id
        )));
    }

    fn profile(&mut self, info: &SessionInfo) {
        self.state.model = self.selected_model.as_ref().unwrap_or(&info.model).clone();
        self.state.model_completions = info.models.clone();
        self.state.context_tokens = info.context_tokens;
        self.state.input_context_tokens = info.input_context_tokens;
        self.state.max_output_tokens = info.max_output_tokens;
    }

    pub async fn poll(&mut self) -> bool {
        let mut changed = false;
        if self
            .connecting
            .as_ref()
            .is_some_and(|task| task.is_finished())
        {
            let result = self.connecting.take().unwrap().await;
            match result {
                Ok(Ok(opened)) => self.install(opened),
                Ok(Err(error)) => self.disconnected(format!("{error:#}")),
                Err(error) => self.disconnected(error.to_string()),
            }
            changed = true;
        }
        for _ in 0..CHANNEL_CAPACITY {
            let response = match self.incoming.as_mut().map(|rx| rx.try_recv()) {
                Some(Ok(response)) => response,
                Some(Err(mpsc::error::TryRecvError::Disconnected)) if !self.offline => {
                    self.disconnected("工作区连接已结束".into());
                    changed = true;
                    break;
                }
                _ => break,
            };
            changed = true;
            match response {
                Ok(response) => self.apply(response),
                Err(error) => {
                    self.disconnected(format!("{error:#}"));
                    break;
                }
            }
        }
        changed |= self.state.media.changed()
            || self
                .state
                .media_preview
                .as_ref()
                .is_some_and(|preview| preview.tick());
        let direction = self.state.selection.auto_scroll(std::time::Instant::now());
        if direction < 0 {
            self.state.scroll_up(2);
        }
        if direction > 0 {
            self.state.scroll_down(2);
        }
        changed |= direction != 0;
        changed |= self
            .state
            .thinking_hover_started
            .is_some_and(|start| start.elapsed() < Duration::from_millis(160));
        changed |= super::advance_tab_hover(&mut self.state);
        changed |= self.state.expire_footnote_focus();
        changed || self.state.running()
    }

    fn apply(&mut self, response: Response) {
        match response {
            Response::Page(page) => {
                self.busy = page.busy;
                if self.selected_model.is_none() {
                    self.state.model = page.model;
                }
                for update in page.updates {
                    match update {
                        Update::Profile(info) => self.profile(&info),
                        Update::Started { prompt } => {
                            self.starting = None;
                            self.title = format!(
                                "{} · {}",
                                self.state.connection_label.as_deref().unwrap_or_default(),
                                prompt.lines().next().unwrap_or_default()
                            );
                            self.state.submit(prompt);
                        }
                        Update::Agent(event) => self.state.apply(*event),
                        Update::Finished { status } => {
                            self.state.finish_clock();
                            self.state.status = match status.as_str() {
                                "completed" => RunStatus::Completed,
                                "cancelled" => RunStatus::Cancelled,
                                _ => RunStatus::Failed,
                            };
                        }
                    }
                }
                let pending = page.approvals.into_iter().next();
                if pending.as_ref().map(|approval| approval.id) != self.approval_id {
                    self.approval_id = pending.as_ref().map(|approval| approval.id);
                    self.state.approval = pending.map(|approval| approval.preview);
                    self.state.approval_scroll = 0;
                    self.state.approval_expanded = false;
                    self.state.approval_shown = false;
                    self.state.approval_choice = 0;
                    self.state.approval_diff = None;
                    if self.state.approval.is_some() {
                        self.state.pause_clock();
                    } else if self.busy {
                        self.state.resume_clock();
                    }
                }
            }
            Response::Sessions(sessions) => self.state.report_local(TranscriptItem::CommandResult(
                sessions
                    .iter()
                    .map(|session| format!("{}  {}  {}", session.id, session.model, session.title))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )),
            Response::Error(error) => {
                self.restore_submission();
                self.state.report_local(TranscriptItem::Error(error));
            }
            _ => {}
        }
    }

    fn restore_submission(&mut self) {
        if let Some(prompt) = self.starting.take() {
            if self.state.input.text.is_empty() {
                self.state.input.set(prompt);
            } else {
                self.state
                    .report_local(TranscriptItem::Notice(format!("尚未确认的输入：{prompt}")));
            }
        }
    }

    fn disconnected(&mut self, error: String) {
        self.restore_submission();
        self.worker = None;
        self.incoming = None;
        self.commands = None;
        self.offline = true;
        self.busy = false;
        self.state.approval = None;
        self.approval_id = None;
        self.state.finish_clock();
        self.state.status = RunStatus::Failed;
        self.state.report_local(TranscriptItem::Error(format!(
            "{error}\n连接已断开，目标任务可能仍在运行；/reconnect 接回，/close 关闭此标签。"
        )));
    }

    fn reconnect(&mut self) {
        if self.connecting.is_some() {
            return;
        }
        if self.starting.is_some() || self.busy {
            self.state
                .report_local(TranscriptItem::Notice("当前连接仍在执行任务。".into()));
            return;
        }
        self.worker = None;
        self.incoming = None;
        self.commands = None;
        self.offline = false;
        self.state
            .report_local(TranscriptItem::Notice("正在重新连接…".into()));
        let task = if let Some(connection) = &self.connection {
            tokio::spawn(Opened::connect(connection.clone(), self.id.clone()))
        } else if let Some(spec) = &self.spec {
            tokio::spawn(spec.clone().open())
        } else {
            return;
        };
        self.connecting = Some(AbortOnDropHandle::new(task));
    }

    fn send(&mut self, request: Request) -> bool {
        let Some(commands) = &self.commands else {
            self.state.report_local(TranscriptItem::Error(
                "尚未连接；输入已保留，可用 /reconnect 重试。".into(),
            ));
            return false;
        };
        if let Err(error) = commands.try_send(Call {
            request,
            reply: None,
        }) {
            self.state
                .report_local(TranscriptItem::Error(format!("连接请求未发送：{error}")));
            return false;
        }
        true
    }
}

pub async fn run(client: Client, connection: Arc<Connection>, info: SessionInfo) -> Result<()> {
    standalone::run(Opened {
        client,
        connection,
        info,
    })
    .await
}
