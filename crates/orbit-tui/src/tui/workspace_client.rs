use super::connection_spec::Spec;
pub(crate) mod auth;
mod configuration;
mod controls;
mod input;
mod project;
mod reconnection;
mod submissions;
use crate::workspace::pump;
mod standalone;
mod subagents;
#[cfg(test)]
pub(super) mod tests;
pub(crate) mod wizard;

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
    connect::Opened,
    protocol::{Mode, Request, Response, SessionInfo, Update},
    transport::{Client, Connection},
};

const CHANNEL_CAPACITY: usize = 16;

pub(super) enum Action {
    Restore,
    Connect,
    Editor,
    RefreshModels,
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
    session_title: String,
    model_catalog: Vec<orbit_protocol::ModelSummary>,
    pub id: Option<String>,
    pub connection: Option<Arc<Connection>>,
    spec: Option<Spec>,
    connecting: Option<AbortOnDropHandle<Result<Opened>>>,
    updates: Option<workspace::interaction::Updates>,
    progress: Option<workspace::interaction::Progress>,
    preparing: Option<AbortOnDropHandle<Result<Arc<Connection>>>>,
    browsing: Option<AbortOnDropHandle<Result<workspace::directories::Listing>>>,
    connected_at: std::time::Instant,
    worker: Option<AbortOnDropHandle<()>>,
    commands: Option<Arc<mpsc::Sender<Call>>>,
    incoming: Option<mpsc::Receiver<Result<Response>>>,
    busy: bool,
    starting: Option<String>,
    starting_attachments: Vec<orbit_model::types::Attachment>,
    approval_id: Option<u64>,
    mode: Mode,
    offline: bool,
    reconnect_at: Option<std::time::Instant>,
    reconnect_attempts: u32,
    subagent: Option<subagents::Remote>,
    question_revision: Option<u64>,
    question_poll: Option<tokio::sync::oneshot::Receiver<Result<Response>>>,
    submissions: Vec<submissions::Submission>,
    completion_input: String,
    next_completion: std::time::Instant,
    next_questions: std::time::Instant,
    model_refresh: Option<AbortOnDropHandle<Result<orbit_protocol::Configuration>>>,
    model_watcher: Option<workspace::configuration::Watcher>,
    model_configuration: Option<orbit_protocol::Configuration>,
    attachment_task: Option<AbortOnDropHandle<Result<Vec<orbit_model::types::Attachment>>>>,
}

impl Tab {
    fn local_directory(&self) -> Result<std::path::PathBuf> {
        match self.target_identity() {
            Some(
                workspace::transport::Target::Ssh { .. } | workspace::transport::Target::Wsl { .. },
            ) => std::env::current_dir().map_err(Into::into),
            _ if !self.state.workspace.is_empty() => Ok(self.state.workspace.clone().into()),
            _ => std::env::current_dir().map_err(Into::into),
        }
    }

    pub(super) fn target_identity(&self) -> Option<workspace::transport::Target> {
        self.connection
            .as_ref()
            .map(|connection| connection.target.clone())
            .or_else(|| self.spec.as_ref().map(Spec::target_identity))
    }

    pub(super) fn display_title(&self, show_target: bool) -> String {
        if show_target {
            return self.title.clone();
        }
        let Some(label) = &self.state.connection_label else {
            return self.title.clone();
        };
        if self.title == *label {
            return if self.id.is_some() {
                "新会话"
            } else {
                "连接中"
            }
            .into();
        }
        self.title
            .strip_prefix(&format!("{label} · "))
            .unwrap_or(&self.title)
            .to_owned()
    }

    pub(super) fn ready_for_restore(&self) -> bool {
        self.id.is_some() && self.connection.is_some() && self.connecting.is_none() && !self.offline
    }

    pub fn connecting(spec: Spec, media: media::Context) -> Self {
        let mut tab = Self::empty(spec.label(), spec.workspace().into(), media);
        tab.spec = Some(spec.clone());
        if spec.remote() && spec.configure_required() {
            tab.state.model = "配置连接".into();
            tab.state.transcript.clear();
            tab.state.workspace_wizard = Some(wizard::Wizard::new(Some(spec)));
        } else {
            tab.start(spec);
        }
        tab
    }

    fn start(&mut self, spec: Spec) {
        self.connecting = None;
        self.preparing = None;
        self.browsing = None;
        self.connection = None;
        self.state.ssh_prompt = None;
        self.spec = Some(spec.clone());
        self.title = spec.label();
        self.state.connection_label = Some(spec.label());
        self.state.model = "连接中".into();
        self.offline = false;
        self.connected_at = std::time::Instant::now();
        let (progress, updates) = workspace::interaction::channel();
        self.progress = Some(progress.clone());
        self.updates = Some(updates);
        self.state.workspace_wizard = spec.remote().then(|| {
            let mut wizard = wizard::Wizard::new(None);
            wizard.busy = true;
            wizard
        });
        if spec.remote() && spec.configure_required() {
            self.preparing = Some(AbortOnDropHandle::new(tokio::spawn(async move {
                spec.prepare(progress).await
            })));
        } else {
            self.connecting = Some(AbortOnDropHandle::new(tokio::spawn(spec.open(progress))));
        }
    }

    fn browse(&mut self, path: String) {
        let (Some(connection), Some(progress)) = (self.connection.clone(), self.progress.clone())
        else {
            return;
        };
        if let Some(wizard) = &mut self.state.workspace_wizard {
            wizard.busy = true;
            wizard.failed = false;
        }
        self.browsing = Some(AbortOnDropHandle::new(tokio::spawn(async move {
            workspace::directories::list(&connection, &path, &progress).await
        })));
    }

    fn open_directory(&mut self, path: String) {
        let (Some(connection), Some(progress)) = (self.connection.clone(), self.progress.clone())
        else {
            return;
        };
        let mut connection = (*connection).clone();
        connection.workspace = path;
        if let Some(wizard) = &mut self.state.workspace_wizard {
            wizard.listing = None;
            wizard.busy = true;
        }
        self.connecting = Some(AbortOnDropHandle::new(tokio::spawn(
            Opened::connect_progress(Arc::new(connection), None, progress),
        )));
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
            session_title: String::new(),
            model_catalog: Vec::new(),
            id: None,
            connection: None,
            spec: None,
            connecting: None,
            updates: None,
            progress: None,
            preparing: None,
            browsing: None,
            connected_at: std::time::Instant::now(),
            worker: None,
            commands: None,
            incoming: None,
            busy: false,
            starting: None,
            starting_attachments: Vec::new(),
            approval_id: None,
            mode: Mode::Auto,
            offline: false,
            reconnect_at: None,
            reconnect_attempts: 0,
            subagent: None,
            question_revision: None,
            question_poll: None,
            submissions: Vec::new(),
            completion_input: String::new(),
            next_completion: std::time::Instant::now(),
            next_questions: std::time::Instant::now(),
            attachment_task: None,
            model_refresh: None,
            model_watcher: None,
            model_configuration: None,
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

    pub fn reload_models(&mut self) {
        self.refresh_models(true);
    }

    fn refresh_models(&mut self, force: bool) {
        if !force && self.model_refresh.is_some() {
            return;
        }
        if let Some(local) = self
            .connection
            .as_ref()
            .and_then(|connection| connection.local_configuration.clone())
        {
            if force {
                self.state
                    .report_local(TranscriptItem::Notice("正在刷新模型目录…".into()));
            }
            self.model_configuration = None;
            if let Some(watcher) = &mut self.model_watcher {
                watcher.resynchronize();
            }
            self.model_refresh = Some(AbortOnDropHandle::new(tokio::spawn(async move {
                local.snapshot_with_refresh(force).await
            })));
        }
    }

    pub(super) fn can_replace_with_resumed(&self) -> bool {
        self.connection.is_some()
            && !self.offline
            && !self.busy
            && self.starting.is_none()
            && self.connecting.is_none()
            && self.attachment_task.is_none()
            && self.submissions.is_empty()
            && self.state.is_home()
            && self.session_title.is_empty()
            && !self.state.btw.has_records()
            && self.state.input.text.is_empty()
            && self.state.attachments.is_empty()
    }

    pub fn sibling(&self, resume: Option<String>) -> Self {
        if let Some(connection) = &self.connection {
            let mut tab = Self::empty(
                connection.target.label(),
                self.state.workspace.clone(),
                self.state.media.clone(),
            );
            let mut connection = (**connection).clone();
            if let Some(configuration) = &mut connection.local_configuration {
                configuration.model = None;
            }
            let connection = Arc::new(connection);
            tab.connection = Some(connection.clone());
            tab.state.workspace_wizard =
                (!matches!(connection.target, workspace::transport::Target::Local))
                    .then(|| wizard::Wizard::new(None));
            let connection = connection.clone();
            let (progress, updates) = workspace::interaction::channel();
            tab.progress = Some(progress.clone());
            tab.updates = Some(updates);
            tab.connecting = Some(AbortOnDropHandle::new(tokio::spawn(Opened::reopen(
                connection, resume, progress,
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
        let attachments = std::mem::take(&mut self.state.attachments);
        let attachment_cursor = self.state.attachment_cursor;
        let user_input = if self.id.as_deref() == Some(&opened.info.id) {
            std::mem::take(&mut self.state.user_input)
        } else {
            Default::default()
        };
        let planning = if self.id.as_deref() == Some(&opened.info.id) {
            std::mem::take(&mut self.state.planning)
        } else {
            Default::default()
        };
        let media = self.state.media.clone();
        let cursor_trail = self.state.cursor_motion.enabled;
        self.state = UiState::new(&opened.info.model);
        self.state.media = media;
        self.state.user_input = user_input;
        self.state.planning = planning;
        self.state.cursor_motion.enabled = cursor_trail;
        self.state.input.set(draft);
        self.state.attachments = attachments;
        self.state.attachment_cursor = attachment_cursor;
        self.state.connection_label = Some(opened.connection.target.label());
        self.state.workspace = opened.info.workspace.clone();
        self.profile(&opened.info);
        self.id = Some(opened.info.id.clone());
        let mut connection = opened.connection;
        Arc::make_mut(&mut connection).unattended = true;
        self.model_watcher = connection
            .local_configuration
            .clone()
            .map(workspace::configuration::Watcher::new);
        self.connection = Some(connection);
        let (commands, requests) = mpsc::channel(CHANNEL_CAPACITY);
        let commands = Arc::new(commands);
        self.state.media.files = Some(Files {
            requests: commands.clone(),
            session: opened.info.id.clone(),
        });
        self.commands = Some(commands);
        self.send(Request::Plan {
            session: opened.info.id.clone(),
            action: orbit_protocol::plan::Action::Enable,
        });
        self.send(Request::EnableUserInput {
            session: opened.info.id.clone(),
        });
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
        self.reconnect_at = None;
        self.reconnect_attempts = 0;
    }

    fn profile(&mut self, info: &SessionInfo) {
        if self.state.workspace != info.workspace {
            self.state.workspace = info.workspace.clone();
            self.state.stored_completions.clear();
            self.state.completion_query = None;
            self.completion_input.clear();
        }
        self.state.media.workspace = info.workspace.clone().into();
        self.state.completion_home = info.home.as_ref().map(Into::into);
        self.state.branch = info.branch.clone();
        self.state.branch_workspace = info.workspace.clone();
        self.state.cursor_motion.enabled = info.cursor_trail;
        self.state.auto_compact = info.auto_compact;
        self.state.skill_completions = info.skills.clone();
        self.mode = info.mode;
        self.state.permission_mode = match info.mode {
            Mode::Ask => super::permissions::Mode::Ask,
            Mode::Auto => super::permissions::Mode::Auto,
            Mode::Yolo => super::permissions::Mode::Yolo,
        };
        if let Some(connection) = &mut self.connection {
            Arc::make_mut(connection).workspace = info.workspace.clone();
        }
        self.session_title = info.title.clone();
        let title = if info.title.is_empty() {
            std::path::Path::new(&info.workspace)
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&info.workspace)
        } else {
            &info.title
        };
        self.title = format!(
            "{} · {title}",
            self.state.connection_label.as_deref().unwrap_or_default(),
        );
        self.state.model = info.model.clone();
        self.state.model_completions = info.models.clone();
        self.model_catalog = info.model_catalog.clone();
        self.state.supports_vision = info.supports_vision;
        self.state.reasoning_effort = info
            .reasoning_effort
            .as_deref()
            .and_then(|effort| effort.parse().ok());
        self.state.reasoning_efforts = info
            .reasoning_efforts
            .iter()
            .filter_map(|effort| effort.parse().ok())
            .collect();
        self.state.context_tokens = info.context_tokens;
        self.state.input_context_tokens = info.input_context_tokens;
        self.state.max_output_tokens = info.max_output_tokens;
    }

    pub async fn poll(&mut self) -> bool {
        let mut changed = self.poll_reconnection();
        if self.commands.is_some()
            && self.model_refresh.is_none()
            && self.model_configuration.is_none()
            && self
                .model_watcher
                .as_mut()
                .is_some_and(|watcher| watcher.due())
        {
            self.refresh_models(false);
        }
        let input = self.state.input.text.clone();
        if self.completion_input != input
            && self.commands.is_some()
            && std::time::Instant::now() >= self.next_completion
        {
            self.completion_input = input.clone();
            self.next_completion = std::time::Instant::now() + Duration::from_millis(150);
            if [
                "/cd ", "/diff ", "/undo ", "/resume ", "/tasks ", "/memory ",
            ]
            .iter()
            .any(|prefix| input.starts_with(prefix))
            {
                self.send(Request::Complete {
                    session: self.id.clone().unwrap_or_default(),
                    input,
                });
            }
        }
        if self
            .model_refresh
            .as_ref()
            .is_some_and(|task| task.is_finished())
        {
            changed = true;
            match self.model_refresh.take().unwrap().await {
                Ok(Ok(configuration)) => {
                    if self
                        .model_watcher
                        .as_ref()
                        .is_none_or(|watcher| watcher.unchanged())
                    {
                        self.model_configuration = Some(configuration);
                    }
                }
                Ok(Err(error)) => self
                    .state
                    .report_local(TranscriptItem::CommandError(error.to_string())),
                Err(error) => self
                    .state
                    .report_local(TranscriptItem::CommandError(error.to_string())),
            }
        }
        if !self.busy
            && self.starting.is_none()
            && let Some(configuration) = self.model_configuration.take()
            && self
                .model_watcher
                .as_ref()
                .is_none_or(|watcher| watcher.unchanged())
        {
            changed |= self.send(Request::ConfigureModels {
                session: self.id.clone().unwrap_or_default(),
                configuration: if self.connection.as_ref().is_some_and(|connection| {
                    matches!(connection.target, workspace::transport::Target::Local)
                }) {
                    None
                } else {
                    Some(configuration)
                },
            });
        }
        changed |= self.state.btw.tick() | self.poll_submissions() | self.poll_questions();
        if self
            .attachment_task
            .as_ref()
            .is_some_and(|task| task.is_finished())
        {
            match self.attachment_task.take().unwrap().await {
                Ok(Ok(images)) => {
                    for image in images {
                        super::attachments::queue_attachment(&mut self.state, image);
                    }
                }
                Ok(Err(error)) => self
                    .state
                    .report_local(TranscriptItem::Error(error.to_string())),
                Err(error) => self
                    .state
                    .report_local(TranscriptItem::Error(error.to_string())),
            }
            changed = true;
        }
        if let Some(updates) = &mut self.updates {
            if let Some(wizard) = &mut self.state.workspace_wizard {
                let logs = updates
                    .logs
                    .lock()
                    .map(|logs| logs.iter().cloned().collect::<Vec<_>>())
                    .unwrap_or_default();
                if wizard.logs != logs {
                    wizard.logs = logs;
                    changed = true;
                }
                let status = format!(
                    "{} · {}s",
                    updates.stage.borrow().as_str(),
                    self.connected_at.elapsed().as_secs()
                );
                if wizard.status != status && !wizard.failed {
                    wizard.status = status;
                    changed = true;
                }
            }
            if self
                .state
                .ssh_prompt
                .as_ref()
                .is_some_and(|prompt| prompt.closed())
            {
                self.state.ssh_prompt = None;
                changed = true;
            }
            if self.state.ssh_prompt.is_none()
                && let Ok(prompt) = updates.prompts.try_recv()
            {
                self.state.ssh_prompt = Some(auth::Prompt::new(prompt));
                self.state.cursor_motion.clear();
                changed = true;
            }
            if self.id.is_none()
                && (self.connecting.is_some()
                    || self.preparing.is_some()
                    || self.browsing.is_some())
            {
                let stage = updates.stage.borrow_and_update().clone();
                let seconds = self.connected_at.elapsed().as_secs();
                let status = format!("{stage} · {seconds}s · Esc / Ctrl+C 取消");
                if let Some(TranscriptItem::Notice(current)) = self.state.transcript.first_mut()
                    && *current != status
                {
                    *current = status;
                    self.state.transcript_layout.invalidate_from(0);
                    changed = true;
                }
            }
        }
        if self
            .preparing
            .as_ref()
            .is_some_and(|task| task.is_finished())
        {
            match self.preparing.take().unwrap().await {
                Ok(Ok(connection)) => {
                    self.connection = Some(connection);
                    self.browse("~".into());
                }
                Ok(Err(error)) => self.disconnected(format!("{error:#}")),
                Err(error) => self.disconnected(error.to_string()),
            }
            changed = true;
        }
        if self
            .browsing
            .as_ref()
            .is_some_and(|task| task.is_finished())
        {
            match self.browsing.take().unwrap().await {
                Ok(Ok(listing)) => {
                    if let Some(wizard) = &mut self.state.workspace_wizard {
                        wizard.set_listing(listing);
                        self.state.model = "选择目录".into();
                        if let Some(TranscriptItem::Notice(notice)) =
                            self.state.transcript.first_mut()
                        {
                            *notice = "连接已就绪，请选择工作目录".into();
                            self.state.transcript_layout.invalidate_from(0);
                        }
                    }
                }
                Ok(Err(error)) => {
                    if let Some(wizard) = &mut self.state.workspace_wizard {
                        wizard.busy = false;
                        wizard.failed = true;
                        wizard.status = format!("{error:#}");
                        if let Some(progress) = &self.progress {
                            progress.log(&wizard.status);
                        }
                    }
                }
                Err(error) => self.disconnected(error.to_string()),
            }
            changed = true;
        }
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
        changed |= self.state.navigation.advance_preview();
        if let Some(picker) = &mut self.state.picker {
            changed |= picker.advance_title_scroll();
        }
        if let Some(picker) = &mut self.state.restore_picker {
            changed |= picker.advance_title_scroll();
        }
        changed |= self.poll_subagent();
        changed || self.state.running()
    }

    fn apply(&mut self, response: Response) {
        match response {
            Response::Completions { input, entries } => {
                if input == self.state.input.text {
                    self.state.completion_query = Some(input);
                    self.state.stored_completions = entries;
                }
            }
            Response::Page(page) => {
                self.state.planning.sync(page.planning);
                if self.state.user_input.sync(page.user_inputs) {
                    self.send(Request::UserInputHistory {
                        session: self.id.clone().unwrap_or_default(),
                        before: None,
                    });
                }
                self.busy = page.busy;
                self.state.model = page.model;
                for update in page.updates {
                    match update {
                        Update::Profile(info) => self.profile(&info),
                        Update::Report(report) => {
                            self.state.show_output(&report.title, report.text)
                        }
                        Update::Started {
                            prompt,
                            presentation,
                        } => {
                            self.starting.take();
                            self.starting_attachments.clear();
                            if self.session_title.is_empty() {
                                self.title = format!(
                                    "{} · {}",
                                    self.state.connection_label.as_deref().unwrap_or_default(),
                                    prompt.lines().next().unwrap_or_default()
                                );
                            }
                            self.state.start_remote(prompt, presentation);
                        }
                        Update::Agent(event) => self.state.apply(*event),
                        Update::Finished { status, elapsed_ms } => {
                            self.state.finish_remote(&status, elapsed_ms);
                        }
                    }
                }
                let pending = page.approvals.into_iter().next();
                if pending.as_ref().map(|approval| approval.id) != self.approval_id {
                    self.approval_id = pending.as_ref().map(|approval| approval.id);
                    self.state.approval = pending.map(|approval| approval.preview);
                    self.state.approval_scroll = 0;
                    self.state.approval_shown = false;
                    self.state.approval_choice = 0;
                    if self.state.approval.is_some() {
                        self.state.pause_clock();
                    } else if self.busy {
                        self.state.resume_clock();
                    }
                }
                if !page.more {
                    self.state.apply_tool_progress(page.tool_progress);
                }
                if !page.more
                    && let Some(timing) = page.timing
                {
                    self.state.sync_clock(timing);
                }
            }
            Response::UserInputs(history) => self.state.user_input.receive(history),
            Response::Plugins(catalog) => {
                self.state.marketplace_dialog =
                    Some(super::dialogs::MarketplaceDialog::from_catalog(catalog));
            }
            Response::Questions(questions) => {
                self.question_revision = Some(questions.revision);
                self.state
                    .btw
                    .receive(questions.records, questions.running, questions.selected);
            }
            Response::Report(report) => self.state.show_output(&report.title, report.text),
            Response::Sessions(sessions) => {
                self.state.picker = Some(super::dialogs::SessionPicker::new(sessions));
            }
            Response::Error(error) => {
                self.restore_submission();
                self.state.report_local(TranscriptItem::Error(error));
            }
            _ => {}
        }
    }

    fn restore_submission(&mut self) {
        if let Some(prompt) = self.starting.take() {
            let mut attachments = std::mem::take(&mut self.starting_attachments);
            attachments.append(&mut self.state.attachments);
            self.state.attachments = attachments;
            if self.state.input.text.is_empty() {
                self.state.input.set(prompt);
            } else {
                self.state
                    .report_local(TranscriptItem::Error(format!("尚未确认的输入：{prompt}")));
            }
        }
    }

    fn disconnected(&mut self, error: String) {
        self.disconnect(error, true);
    }

    fn disconnect(&mut self, error: String, retry: bool) {
        self.restore_submission();
        if let Some(wizard) = &mut self.state.workspace_wizard {
            wizard.failed = true;
            wizard.busy = false;
            wizard.status = error.clone();
            if let Some(updates) = &self.updates
                && let Ok(logs) = updates.logs.lock()
            {
                wizard.logs = logs.iter().cloned().collect();
            }
            self.state.model = "连接失败".into();
        }
        self.updates = None;
        self.state.ssh_prompt = None;
        self.worker = None;
        self.incoming = None;
        self.commands = None;
        self.offline = true;
        self.busy = false;
        self.state.approval = None;
        self.approval_id = None;
        self.state.finish_clock();
        self.state.status = RunStatus::Failed;
        let retrying = self.reconnect_attempts > 0;
        let automatic = retry && self.schedule_reconnection();
        if self.state.workspace_wizard.is_none() && !retrying {
            let recovery = if automatic {
                "正在自动重连原会话；Esc / Ctrl+C 停止重连，/close 关闭标签。"
            } else {
                "/reconnect 重试，/close 关闭标签。"
            };
            self.state.report_local(TranscriptItem::Error(format!(
                "{error}\n连接已断开，目标任务可能仍在运行。{recovery}"
            )));
        }
    }

    fn reconnect(&mut self) {
        self.reconnect_attempts = 0;
        self.begin_reconnection(false);
    }

    fn begin_reconnection(&mut self, automatic: bool) {
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
        self.reconnect_at = None;
        if !automatic {
            self.state
                .report_local(TranscriptItem::Notice("正在重新连接…".into()));
        }
        let (progress, updates) = workspace::interaction::channel();
        self.progress = Some(progress.clone());
        self.updates = Some(updates);
        let remote = self.connection.as_ref().map_or_else(
            || self.spec.as_ref().is_some_and(Spec::remote),
            |connection| !matches!(connection.target, workspace::transport::Target::Local),
        );
        self.state.workspace_wizard =
            (remote && self.id.is_none()).then(|| wizard::Wizard::new(None));
        self.connected_at = std::time::Instant::now();
        let task = if let Some(connection) = &self.connection {
            tokio::spawn(Opened::reopen(
                connection.clone(),
                self.id.clone(),
                progress,
            ))
        } else if let Some(spec) = &self.spec {
            tokio::spawn(spec.clone().open(progress))
        } else {
            return;
        };
        self.connecting = Some(AbortOnDropHandle::new(task));
    }

    fn cancel_connect(&mut self) {
        self.connecting = None;
        self.preparing = None;
        self.browsing = None;
        self.disconnect("连接已取消".into(), false);
        self.reconnect_at = None;
        self.reconnect_attempts = 0;
        self.state.report_local(TranscriptItem::Notice(
            "已停止重连；/reconnect 可接回原会话。".into(),
        ));
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
