mod actions;
mod connection;
mod feedback;
mod inspector;
mod lifecycle;
pub(crate) use lifecycle::{WorkspaceEvent, WorkspaceSummary};
mod models;
mod navigation;
mod panel;
mod questions;
mod recovery;
mod settings;
mod slash;
mod view;

use crate::{
    appearance::Appearance,
    backend::{Backend, Command, Event, Options},
    conversation::Conversation,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, Sizable, WindowExt,
    attachment::{Attachment, AttachmentMedia},
    button::{Button, ButtonVariants},
    h_flex,
    input::{InputEvent, Textarea, TextareaState},
    menu::{DropdownMenu, PopupMenuItem},
    resizable::{h_resizable, resizable_panel},
    text::TextView,
    tree::{Tree, TreeItem, TreeState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use orbit_client::protocol::{Approval, Request, Response, SessionInfo, Update};
use std::{
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};

actions!(
    orbit_gui,
    [
        SendMessage,
        NewConversation,
        AppearanceSettings,
        FocusComposer,
        ToggleSidebar,
        WorkspaceCommands,
        ShowChanges,
        SearchSessions,
        OpenProject,
        NextTask,
        PreviousTask,
        ToggleInspector,
        CompletionNext,
        CompletionPrevious,
        CompletionSubmit,
        CompletionConfirm,
        CompletionDismiss
    ]
);

pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new(
            "down",
            CompletionNext,
            Some("Orbit > ComposerCompletions > Input"),
        ),
        KeyBinding::new(
            "up",
            CompletionPrevious,
            Some("Orbit > ComposerCompletions > Input"),
        ),
        KeyBinding::new(
            "enter",
            CompletionSubmit,
            Some("Orbit > ComposerCompletions > Input"),
        ),
        KeyBinding::new(
            "tab",
            CompletionConfirm,
            Some("Orbit > ComposerCompletions > Input"),
        ),
        KeyBinding::new(
            "escape",
            CompletionDismiss,
            Some("Orbit > ComposerCompletions > Input"),
        ),
        KeyBinding::new("ctrl-enter", SendMessage, Some("Orbit")),
        KeyBinding::new("ctrl-n", NewConversation, Some("Orbit")),
        KeyBinding::new("ctrl-comma", AppearanceSettings, Some("Orbit")),
        KeyBinding::new("ctrl-l", FocusComposer, Some("Orbit")),
        KeyBinding::new("ctrl-b", ToggleSidebar, Some("Orbit")),
        KeyBinding::new("ctrl-shift-p", WorkspaceCommands, Some("Orbit")),
        KeyBinding::new("ctrl-shift-d", ShowChanges, Some("Orbit")),
        KeyBinding::new("ctrl-p", SearchSessions, Some("Orbit")),
        KeyBinding::new("ctrl-o", OpenProject, Some("Orbit")),
        KeyBinding::new("ctrl-tab", NextTask, Some("Orbit")),
        KeyBinding::new("ctrl-shift-tab", PreviousTask, Some("Orbit")),
        KeyBinding::new("ctrl-shift-e", ToggleInspector, Some("Orbit")),
        // Input has its own Enter and editing actions. Bind at that context
        // so it cannot consume the composer's commands first.
        KeyBinding::new("ctrl-enter", SendMessage, Some("Orbit > Input")),
        KeyBinding::new("ctrl-n", NewConversation, Some("Orbit > Input")),
        KeyBinding::new("ctrl-comma", AppearanceSettings, Some("Orbit > Input")),
        KeyBinding::new("ctrl-l", FocusComposer, Some("Orbit > Input")),
        KeyBinding::new("ctrl-b", ToggleSidebar, Some("Orbit > Input")),
        KeyBinding::new("ctrl-shift-p", WorkspaceCommands, Some("Orbit > Input")),
        KeyBinding::new("ctrl-shift-d", ShowChanges, Some("Orbit > Input")),
        KeyBinding::new("ctrl-p", SearchSessions, Some("Orbit > Input")),
        KeyBinding::new("ctrl-o", OpenProject, Some("Orbit > Input")),
        KeyBinding::new("ctrl-tab", NextTask, Some("Orbit > Input")),
        KeyBinding::new("ctrl-shift-tab", PreviousTask, Some("Orbit > Input")),
        KeyBinding::new("ctrl-shift-e", ToggleInspector, Some("Orbit > Input")),
    ]);
}

struct PendingSend {
    id: String,
    session: String,
    text: SharedString,
    paths: Vec<PathBuf>,
    steer: bool,
}

pub(crate) struct Workspace {
    key: String,
    visible: bool,
    open_tasks: Vec<WorkspaceSummary>,
    navigation_tasks: Vec<WorkspaceSummary>,
    hovered_title: Option<(SharedString, Instant)>,
    execution: crate::execution::Execution,
    execution_scroll: UniformListScrollHandle,
    task_scroll: ScrollHandle,
    task_reveal: bool,
    task_width: Option<Pixels>,
    inspector: Option<inspector::Section>,
    options: Options,
    runtime: Arc<tokio::runtime::Runtime>,
    backend: Option<Backend>,
    receive: Task<()>,
    reconnect: Task<()>,
    recovery: recovery::Recovery,
    timer: Task<()>,
    picker: Task<()>,
    _subscriptions: Vec<Subscription>,
    focus: FocusHandle,
    input: Entity<TextareaState>,
    conversation: Entity<Conversation>,
    info: Option<SessionInfo>,
    title: SharedString,
    model: SharedString,
    location: SharedString,
    sessions: Vec<SessionEntry>,
    groups: Vec<TreeItem>,
    navigation: Entity<TreeState>,
    project_picker: Task<()>,
    choosing_project: bool,
    project: SharedString,
    sidebar_visible: bool,
    paths: Vec<PathBuf>,
    pending: Option<PendingSend>,
    clipboard_files: Vec<Arc<tempfile::NamedTempFile>>,
    retry: Option<PendingSend>,
    error: Option<SharedString>,
    connected: bool,
    opening: bool,
    busy: bool,
    approvals: Vec<Approval>,
    usage: Entity<crate::usage::Usage>,
    quota_request: Instant,
    appearance: Option<Appearance>,
    settings_page: Option<Entity<settings::SettingsPage>>,
    question_form: Option<Entity<questions::QuestionForm>>,
    command_search: Entity<gpui_kit::component::command::CommandState>,
    command_open: bool,
    arguments: slash::Arguments,
    completion_task: Task<()>,
    planning: orbit_protocol::plan::State,
    user_inputs: Vec<orbit_protocol::user_input::Record>,
    context_used: usize,
    replaying: bool,
    panel: Option<Entity<panel::Panel>>,
    subagent: Option<(String, Entity<Conversation>)>,
}

impl Workspace {
    pub(crate) fn new(
        options: Options,
        runtime: Arc<tokio::runtime::Runtime>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("描述任务，输入 / 查看命令…")
                .submit_on_enter(true)
                .auto_grow(3, 8)
        });
        let subscription = cx.subscribe_in(&input, window, |view, input, event, window, cx| {
            if matches!(event, InputEvent::Change) {
                let draft = input.read(cx).value();
                view.update_completions(draft.as_str(), window, cx);
                cx.emit(WorkspaceEvent::DraftChanged);
                cx.notify();
            } else if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
                view.send(&SendMessage, window, cx);
            }
        });
        let input_focus = input.read(cx).focus_handle(cx);
        let focused = cx.on_focus(&input_focus, window, |_, _, cx| cx.notify());
        let blurred = cx.on_blur(&input_focus, window, |_, _, cx| cx.notify());
        let conversation = cx.new(Conversation::new);
        let location = options
            .workspace
            .clone()
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
            .into();
        let mut view = Self {
            key: orbit_protocol::request_id(),
            visible: false,
            open_tasks: Vec::new(),
            navigation_tasks: Vec::new(),
            hovered_title: None,
            execution: Default::default(),
            execution_scroll: UniformListScrollHandle::new(),
            task_scroll: ScrollHandle::new(),
            task_reveal: true,
            task_width: None,
            inspector: None,
            options,
            runtime,
            backend: None,
            receive: Task::ready(()),
            reconnect: Task::ready(()),
            recovery: Default::default(),
            timer: Task::ready(()),
            picker: Task::ready(()),
            _subscriptions: vec![subscription, focused, blurred],
            focus: cx.focus_handle(),
            input,
            conversation,
            info: None,
            title: "新会话".into(),
            model: "模型".into(),
            location,
            sessions: Vec::new(),
            groups: Vec::new(),
            navigation: cx.new(|cx| TreeState::new(cx)),
            project_picker: Task::ready(()),
            choosing_project: false,
            project: "工作区".into(),
            sidebar_visible: true,
            paths: Vec::new(),
            pending: None,
            clipboard_files: Vec::new(),
            retry: None,
            error: None,
            connected: false,
            opening: true,
            busy: false,
            approvals: Vec::new(),
            usage: cx.new(crate::usage::Usage::new),
            quota_request: Instant::now(),
            appearance: None,
            settings_page: None,
            question_form: None,
            command_search: cx
                .new(|cx| gpui_kit::component::command::CommandState::new(window, cx)),
            command_open: false,
            arguments: Default::default(),
            completion_task: Task::ready(()),
            planning: Default::default(),
            user_inputs: Vec::new(),
            context_used: 0,
            replaying: false,
            panel: None,
            subagent: None,
        };
        view.input.update(cx, |input, cx| input.focus(window, cx));
        view.connect(window, cx);
        view.timer = cx.spawn(async move |weak, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if weak
                    .update(cx, |view, cx| {
                        if view.connected
                            && !view.opening
                            && view.quota_request.elapsed() >= Duration::from_secs(60)
                        {
                            view.refresh_usage(cx);
                        }
                        // The footer borrows the quota view's summary; its reset
                        // countdown also needs a render while the dialog is closed.
                        if view.info.is_some() {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        });
        view
    }

    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reconnect = Task::ready(());
        self.recovery = Default::default();
        self.start_connection(window, cx);
    }

    fn start_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.error.is_some() {
            window.remove_notification::<Workspace>(cx);
        }
        self.error = None;
        self.opening = true;
        self.connected = false;
        let mut options = self.options.clone();
        if let Some(info) = &self.info {
            options.resume = (info.persisted || self.retry.is_some() || self.pending.is_some())
                .then(|| info.id.clone());
            options.recover_draft = !info.persisted && options.resume.is_some();
            options.workspace = Some(PathBuf::from(&info.workspace));
            options.model = Some(info.model.clone());
        }
        let (backend, mut receiver) = Backend::start(options, &self.runtime);
        self.backend = Some(backend);
        self.receive = cx.spawn(async move |weak, cx| {
            while let Some(event) = receiver.recv().await {
                if weak
                    .update_in(cx, |view, window, cx| view.event(event, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        cx.notify();
    }

    fn command(&mut self, command: Command, cx: &mut Context<Self>) -> bool {
        let result = self
            .backend
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("请先连接工作区。"))
            .and_then(|backend| backend.send(command));
        if let Err(error) = result {
            self.report_error(error.to_string(), cx);
            cx.notify();
            false
        } else {
            true
        }
    }

    fn event(&mut self, event: Event, window: &mut Window, cx: &mut Context<Self>) {
        let previous = self.summary();
        match event {
            Event::Connected => self.connected = true,
            Event::Subagent { agent, page, reset } => {
                if reset {
                    let conversation = cx.new(Conversation::new);
                    self.subagent = Some((agent.clone(), conversation.clone()));
                    let owner = cx.weak_entity();
                    let title = format!("子 Agent · {agent}");
                    if self.visible {
                        window.open_sheet(cx, move |sheet, _, _| {
                            let owner = owner.clone();
                            sheet
                                .title(title.clone())
                                .size(rems(48.))
                                .child(div().h_full().child(conversation.clone()))
                                .on_close(move |_, _, cx| {
                                    let _ = owner.update(cx, |view, cx| {
                                        view.subagent = None;
                                        view.command(Command::CloseSubagent, cx);
                                    });
                                })
                        });
                    }
                }
                if let Some((current, conversation)) = &self.subagent
                    && *current == agent
                {
                    conversation.update(cx, |view, cx| {
                        for update in page.updates {
                            view.apply(update, cx);
                        }
                    });
                }
            }
            Event::Image {
                session,
                path,
                result,
            } => {
                if self.info.as_ref().is_some_and(|info| info.id == session) {
                    self.conversation
                        .update(cx, |view, cx| view.image(&path, result, cx));
                }
            }
            Event::Completions {
                session,
                input,
                result,
            } => self.argument_completions(&session, &input, result, window, cx),
            Event::Challenge(challenge) => self.connection_challenge(challenge, window, cx),
            Event::File { path, bytes } => match String::from_utf8(bytes) {
                Ok(text) => self.show_report(path, format!("````\n{text}\n````"), window, cx),
                Err(_) => self.report_error("这个文件是二进制文件，请选择文本文件。", cx),
            },
            Event::Usage { session, result } => {
                if self.info.as_ref().is_some_and(|info| {
                    info.id == session
                        && result.as_ref().map_or(true, |snapshot| {
                            snapshot.model.rsplit("::").next() == info.model.rsplit("::").next()
                        })
                }) {
                    self.usage.update(cx, |usage, cx| match result {
                        Ok(snapshot) => usage.set(snapshot, cx),
                        Err(error) => usage.error(error, cx),
                    });
                }
            }
            Event::Disconnected(error) => {
                self.connected = false;
                self.opening = false;
                self.retry = self.pending.take().or(self.retry.take());
                if let Some(delay) = self
                    .info
                    .as_ref()
                    .and_then(|_| self.recovery.disconnected(Instant::now()))
                {
                    self.error = Some(error.into());
                    self.reconnect = cx.spawn(async move |owner, cx| {
                        cx.background_executor().timer(delay).await;
                        let _ = owner
                            .update_in(cx, |view, window, cx| view.start_connection(window, cx));
                    });
                } else {
                    self.report_error(error, cx);
                }
            }
            Event::Error(error) => {
                self.report_error(error, cx);
                self.opening = false;
            }
            Event::SendFailed { id, error } => {
                if self
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.id == id)
                {
                    self.retry = self.pending.take();
                }
                self.report_error(error, cx);
            }
            Event::Sent { id } => {
                if self
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.id == id)
                {
                    let pending = self.pending.take().unwrap();
                    self.retry = None;
                    // Start acknowledges only after the service persists it.
                    // Closing before the next Profile poll must still resume
                    // this session, rather than restore an empty draft.
                    if let Some(info) = &mut self.info {
                        info.persisted = true;
                    }
                    if self.input.read(cx).value() == pending.text {
                        self.input
                            .update(cx, |input, cx| input.set_value("", window, cx));
                    }
                    self.paths.retain(|path| !pending.paths.contains(path));
                    self.busy = true;
                }
            }
            Event::Response(response) => self.response(response, window, cx),
        }
        if previous != self.summary() {
            cx.emit(WorkspaceEvent::Changed);
        }
        cx.emit(WorkspaceEvent::DraftChanged);
        cx.notify();
    }

    fn profile(&mut self, info: SessionInfo, cx: &mut Context<Self>) {
        let changed = self
            .info
            .as_ref()
            .is_none_or(|previous| previous.id != info.id || previous.workspace != info.workspace);
        if let Some(entry) = self
            .sessions
            .iter_mut()
            .find(|entry| entry.id.as_ref() == info.id)
        {
            if !info.title.trim().is_empty() {
                entry.title = info.title.clone().into();
            }
            entry.workspace = info.workspace.clone().into();
        }
        self.model = info.model.clone().into();
        self.location = info.workspace.clone().into();
        self.project = PathBuf::from(&info.workspace)
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
            .into();
        self.info = Some(info);
        self.refresh_title(cx);
        self.regroup(cx);
        if changed {
            let item = TreeItem::new(
                format!("session:{}", self.info.as_ref().unwrap().id),
                self.title.clone(),
            );
            self.navigation
                .update(cx, |tree, cx| tree.set_selected_item(Some(&item), cx));
        }
    }

    fn response(&mut self, response: Response, window: &mut Window, cx: &mut Context<Self>) {
        match response {
            Response::Opened(info) => {
                self.options.relink_on_resume = false;
                self.options.recover_draft = false;
                // This entity owns one task's composer. Restoring transport
                // history must never overwrite text edited while offline.
                self.profile(info, cx);
                self.opening = true;
                self.busy = false;
                self.error = None;
                self.approvals.clear();
                self.user_inputs.clear();
                self.question_form = None;
                self.planning = Default::default();
                self.context_used = 0;
                self.panel = None;
                self.subagent = None;
                self.usage = cx.new(crate::usage::Usage::new);
                self.conversation = cx.new(Conversation::new);
                self.execution = Default::default();
                self.refresh_usage(cx);
                self.command(Command::Request(Request::AllSessions), cx);
                self.command(
                    Command::Request(Request::EnableUserInput {
                        session: self.info.as_ref().unwrap().id.clone(),
                    }),
                    cx,
                );
                self.plan_action(orbit_protocol::plan::Action::Enable, cx);
            }
            Response::Profile(info) => {
                self.usage = cx.new(crate::usage::Usage::new);
                self.profile(*info, cx);
                self.refresh_usage(cx);
            }
            Response::Sessions(sessions) => {
                self.sessions = sessions
                    .into_iter()
                    .map(|session| SessionEntry {
                        id: session.id.into(),
                        title: session.title.into(),
                        workspace: session.workspace.into(),
                    })
                    .collect();
                self.refresh_title(cx);
                self.regroup(cx);
                cx.emit(WorkspaceEvent::Catalog(self.sessions.clone()));
            }
            Response::Page(page) => {
                let restoring = self.opening;
                // Live output can also span pages. Only an initial restore
                // owns the loading screen; live catch-up keeps the chat visible.
                self.opening = restoring && page.more;
                if !page.more {
                    self.recovery.restored(Instant::now());
                }
                self.busy = page.busy;
                self.approvals = page.approvals;
                self.planning = page.planning;
                self.user_inputs = page.user_inputs;
                self.sync_questions(window, cx);
                let finished = page
                    .updates
                    .iter()
                    .any(|update| matches!(update, Update::Finished { .. }));
                for update in page.updates {
                    if let Update::Started { presentation, .. } = &update {
                        for image in &presentation.image_assets {
                            if let Some(info) = &self.info {
                                self.command(
                                    Command::ReadImage {
                                        session: info.id.clone(),
                                        path: image.path.clone(),
                                    },
                                    cx,
                                );
                            }
                        }
                    }
                    if let Update::Profile(info) = update {
                        let changed = self.info.as_ref().is_none_or(|current| {
                            current.model != info.model || current.workspace != info.workspace
                        });
                        self.profile(*info, cx);
                        if changed {
                            self.usage = cx.new(crate::usage::Usage::new);
                            self.refresh_usage(cx);
                        }
                    } else {
                        self.execution.apply(&update);
                        self.track_feedback(&update, cx);
                        self.conversation
                            .update(cx, |conversation, cx| conversation.apply(update, cx));
                    }
                }
                self.refresh_title(cx);
                if finished && !restoring {
                    self.command(Command::Request(Request::AllSessions), cx);
                }
            }
            Response::Usage(snapshot) => self.usage.update(cx, |usage, cx| usage.set(snapshot, cx)),
            Response::Report(report) => self.show_report(report.title, report.text, window, cx),
            Response::File(content) => self.show_report(
                "文件".into(),
                format!("````text\n{content}\n````"),
                window,
                cx,
            ),
            Response::Questions(questions) => self.show_questions(questions, window, cx),
            Response::Plugins(catalog) => self.show_plugins(catalog, window, cx),
            Response::UserInputs(history) => {
                let text = history
                    .records
                    .iter()
                    .map(|record| {
                        let questions = record
                            .form
                            .questions
                            .iter()
                            .map(|question| question.prompt.as_str())
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        let answers = record
                            .answers
                            .iter()
                            .map(|answer| format!("{} {}", answer.selected.join(", "), answer.text))
                            .collect::<Vec<_>>()
                            .join("\n\n");
                        format!(
                            "## {} · {:?}\n\n{questions}\n\n{answers}",
                            record.id, record.status
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n\n---\n\n");
                self.show_report(
                    "Agent 问答历史".into(),
                    if text.is_empty() {
                        "暂无问答记录".into()
                    } else {
                        text
                    },
                    window,
                    cx,
                );
            }
            Response::Subagent(page) => {
                let conversation = cx.new(Conversation::new);
                conversation.update(cx, |view, cx| {
                    for update in page.updates {
                        view.apply(update, cx);
                    }
                });
                window.open_sheet(cx, move |sheet, _, _| {
                    sheet
                        .title("子 Agent")
                        .size(rems(48.))
                        .child(div().h_full().child(conversation.clone()))
                });
            }
            Response::Error(error) => {
                self.report_error(error, cx);
                self.opening = false;
            }
            _ => {}
        }
    }

    fn open(&mut self, resume: Option<String>, cx: &mut Context<Self>) {
        if self.opening || !self.connected {
            return;
        }
        let mut options = self.options.clone();
        options.workspace = Some(PathBuf::from(self.location.as_ref()));
        options.model = self
            .info
            .as_ref()
            .map(|info| info.model.clone())
            .or(options.model);
        options.resume = resume;
        options.recover_draft = false;
        cx.emit(WorkspaceEvent::Open(Box::new(options)));
    }

    fn send(&mut self, _: &SendMessage, window: &mut Window, cx: &mut Context<Self>) {
        if self.command_open {
            self.submit_completion(window, cx);
            return;
        }
        if self.input.read(cx).value().starts_with('/') && self.dispatch_slash(window, cx) {
            return;
        }
        if !self.can_send(cx) {
            return;
        }
        let text = self.input.read(cx).value();
        let session = self.info.as_ref().unwrap().id.clone();
        let retry = self
            .retry
            .as_ref()
            .filter(|retry| retry.matches(&session, &text, &self.paths));
        let steer = retry.map_or(self.busy, |retry| retry.steer);
        let id = retry
            .map(|retry| retry.id.clone())
            .unwrap_or_else(orbit_protocol::request_id);
        let pending = PendingSend {
            session: session.clone(),
            id: id.clone(),
            text: text.clone(),
            paths: self.paths.clone(),
            steer,
        };
        if self.command(
            Command::Send {
                session,
                id,
                prompt: text.to_string(),
                paths: self.paths.clone(),
                steer,
            },
            cx,
        ) {
            self.pending = Some(pending);
            self.error = None;
            cx.emit(WorkspaceEvent::DraftChanged);
            cx.notify();
        }
    }

    fn can_send(&self, cx: &App) -> bool {
        self.connected
            && !self.opening
            && self.pending.is_none()
            && self.info.is_some()
            && (!self.input.read(cx).value().trim().is_empty() || !self.paths.is_empty())
            && (!self.busy || self.paths.is_empty())
    }

    fn refresh_usage(&mut self, cx: &mut Context<Self>) {
        self.quota_request = Instant::now();
        if let Some(info) = &self.info {
            self.command(
                Command::Request(Request::Usage {
                    session: info.id.clone(),
                }),
                cx,
            );
        }
    }

    fn appearance(&mut self, _: &AppearanceSettings, window: &mut Window, cx: &mut Context<Self>) {
        self.settings(false, window, cx);
    }

    fn attach(&mut self, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("添加图片".into()),
        });
        let session = self.info.as_ref().map(|info| info.id.clone());
        self.picker = cx.spawn(async move |weak, cx| {
            let result = prompt.await;
            let _ = weak.update(cx, |view, cx| {
                if view.info.as_ref().map(|info| &info.id) != session.as_ref() {
                    return;
                }
                match result {
                    Ok(Ok(Some(paths))) => {
                        for path in paths {
                            let supported =
                                path.extension().and_then(|v| v.to_str()).is_some_and(|v| {
                                    matches!(
                                        v.to_ascii_lowercase().as_str(),
                                        "png" | "jpg" | "jpeg" | "webp" | "gif"
                                    )
                                });
                            if supported && !view.paths.contains(&path) {
                                view.paths.push(path);
                            } else if !supported {
                                view.report_error("请选择 PNG、JPEG、WebP 或 GIF 图片。", cx);
                            }
                        }
                    }
                    Ok(Err(error)) => view.report_error(format!("无法选择图片：{error}"), cx),
                    _ => {}
                }
                cx.emit(WorkspaceEvent::DraftChanged);
                cx.notify();
            });
        });
    }

    fn paste_images(&mut self, item: &ClipboardItem, cx: &mut Context<Self>) -> bool {
        if self.pending.is_some() || self.busy {
            return false;
        }
        let images = item
            .entries()
            .iter()
            .filter_map(|entry| match entry {
                ClipboardEntry::Image(image) => Some(image.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        if images.is_empty() {
            let mut handled = false;
            for entry in item.entries() {
                if let ClipboardEntry::ExternalPaths(paths) = entry {
                    self.add_images(&paths.0, cx);
                    handled = true;
                }
            }
            return handled;
        }
        let session = self.info.as_ref().map(|info| info.id.clone());
        let job =
            self.runtime
                .spawn_blocking(move || -> anyhow::Result<Vec<tempfile::NamedTempFile>> {
                    use std::io::Write;
                    images
                        .into_iter()
                        .map(|image| {
                            let mut file = tempfile::Builder::new()
                                .prefix("orbit-image-")
                                .suffix(&format!(".{}", image.format().extension()))
                                .tempfile()?;
                            file.write_all(image.bytes())?;
                            Ok(file)
                        })
                        .collect()
                });
        self.picker = cx.spawn(async move |weak, cx| {
            let result = job.await;
            let _ = weak.update(cx, |view, cx| {
                if view.info.as_ref().map(|info| &info.id) != session.as_ref() {
                    return;
                }
                match result {
                    Ok(Ok(files)) => {
                        view.paths
                            .extend(files.iter().map(|file| file.path().to_path_buf()));
                        view.clipboard_files.extend(files.into_iter().map(Arc::new));
                    }
                    Ok(Err(error)) => view.report_error(format!("粘贴图片失败：{error}"), cx),
                    Err(error) => view.report_error(format!("粘贴图片失败：{error}"), cx),
                }
                cx.emit(WorkspaceEvent::DraftChanged);
                cx.notify();
            });
        });
        true
    }

    fn add_images(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        if self.pending.is_some() || self.busy {
            return;
        }
        for path in paths {
            let supported = path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| {
                    matches!(
                        extension.to_ascii_lowercase().as_str(),
                        "png" | "jpg" | "jpeg" | "webp" | "gif"
                    )
                });
            if supported && !self.paths.contains(path) {
                self.paths.push(path.clone());
            } else if !supported {
                self.report_error("请选择 PNG、JPEG、WebP 或 GIF 图片。", cx);
            }
        }
        cx.emit(WorkspaceEvent::DraftChanged);
        cx.notify();
    }

    fn approval(&self, approval: &Approval, window: &mut Window, cx: &mut Context<Self>) {
        let Some(info) = &self.info else {
            return;
        };
        let session = info.id.clone();
        let id = approval.id;
        let text: SharedString = serde_json::to_string_pretty(&approval.preview)
            .unwrap_or_default()
            .into();
        let weak = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let buttons = [
                (false, false, "拒绝"),
                (true, false, "允许一次"),
                (true, true, "记住并允许"),
            ]
            .into_iter()
            .map(|(allow, remember, label)| {
                let weak = weak.clone();
                let session = session.clone();
                Button::new(if remember {
                    "remember"
                } else if allow {
                    "allow"
                } else {
                    "deny"
                })
                .label(label)
                .on_click(move |_, window, cx| {
                    let accepted = weak
                        .update(cx, |view, cx| {
                            view.command(
                                Command::Request(Request::Approve {
                                    session: session.clone(),
                                    approval: id,
                                    allow,
                                    remember,
                                }),
                                cx,
                            )
                        })
                        .unwrap_or(false);
                    if accepted {
                        window.close_dialog(cx);
                    }
                })
            });
            dialog
                .title("审批操作")
                .child(TextView::markdown(
                    "approval-preview",
                    format!("```json\n{text}\n```"),
                ))
                .footer(h_flex().gap_2().justify_end().children(buttons))
        });
    }

    fn show_usage(&self, window: &mut Window, cx: &mut Context<Self>) {
        let usage = self.usage.clone();
        let weak = cx.weak_entity();
        window.open_dialog(cx, move |dialog, window, _| {
            let weak = weak.clone();
            dialog
                .title("用量")
                .width(window.rem_size() * 38.)
                .child(usage.clone())
                .footer(
                    h_flex().justify_end().child(
                        Button::new("refresh-usage")
                            .label("刷新")
                            .on_click(move |_, _, cx| {
                                let _ = weak.update(cx, |view, cx| view.refresh_usage(cx));
                            }),
                    ),
                )
        });
    }
}

#[derive(Clone)]
pub(crate) struct SessionEntry {
    id: SharedString,
    title: SharedString,
    workspace: SharedString,
}

impl PendingSend {
    fn matches(&self, session: &str, text: &str, paths: &[PathBuf]) -> bool {
        self.session == session && self.text.as_ref() == text && self.paths == paths
    }
}
#[cfg(test)]
mod tests {
    use super::PendingSend;
    #[test]
    fn ambiguous_send_reuses_receipt_only_for_the_same_session_and_payload() {
        let pending = PendingSend {
            id: "receipt".into(),
            session: "one".into(),
            text: "draft".into(),
            paths: vec!["a.png".into()],
            steer: false,
        };
        assert!(pending.matches("one", "draft", &["a.png".into()]));
        assert!(!pending.matches("two", "draft", &["a.png".into()]));
        assert!(!pending.matches("one", "edited", &["a.png".into()]));
        assert!(!pending.matches("one", "draft", &["b.png".into()]));
    }
}
