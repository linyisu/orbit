mod actions;
mod approval;
mod attention;
pub(crate) use actions::WorkspaceCommand;
mod composer;
mod connection;
mod feedback;
mod files;
mod input_queue;
mod inspector;
mod lifecycle;
pub(crate) use lifecycle::{WorkspaceEvent, WorkspaceSummary};
mod models;
mod navigation;
mod sidebar;
pub(crate) use sidebar::Sidebar;
mod panel;
mod questions;
mod recovery;
mod review;
mod settings;
pub(crate) use settings::SettingsPage;
mod slash;
mod subagents;
mod tasks;
mod tool_output;
mod view;

use crate::{
    appearance::Appearance,
    backend::{Backend, Command, Event, Options},
    conversation::Conversation,
};
use gpui_kit::assets::IconName;
use gpui_kit::component::{
    ActiveTheme, Disableable, Icon, Sizable, WindowExt,
    attachment::{Attachment, AttachmentContent, AttachmentMedia, AttachmentTitle},
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
        CompletionDismiss,
        DismissRemoteChoices,
        InputHistoryPrevious,
        InputHistoryNext,
        CloseTask,
        StopTask
    ]
);

pub(crate) fn bind_keys(cx: &mut App) {
    const REMOTE_CHOICE_INPUT: &str = "RemoteChoiceInput > Input";
    const REMOTE_CHOICE_LIST: &str = "RemoteChoices > Combobox > List";

    files::bind_keys(cx);
    inspector::bind_keys(cx);
    cx.bind_keys([
        KeyBinding::new("tab", DismissRemoteChoices, Some(REMOTE_CHOICE_INPUT)),
        KeyBinding::new("shift-tab", DismissRemoteChoices, Some(REMOTE_CHOICE_INPUT)),
        KeyBinding::new("tab", DismissRemoteChoices, Some(REMOTE_CHOICE_LIST)),
        KeyBinding::new("shift-tab", DismissRemoteChoices, Some(REMOTE_CHOICE_LIST)),
        KeyBinding::new(
            "down",
            gpui_kit::base::actions::SelectDown,
            Some(REMOTE_CHOICE_INPUT),
        ),
        KeyBinding::new(
            "up",
            gpui_kit::base::actions::SelectUp,
            Some(REMOTE_CHOICE_INPUT),
        ),
        KeyBinding::new(
            "escape",
            gpui_kit::base::actions::Cancel,
            Some(REMOTE_CHOICE_INPUT),
        ),
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
    ]);
    crate::keymap::init(cx);
}

struct PendingSend {
    id: String,
    session: String,
    text: SharedString,
    paths: Vec<PathBuf>,
    steer: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MainPage {
    Conversation,
    Tasks,
    Files,
    Review,
}

pub(crate) struct Workspace {
    key: String,
    visible: bool,
    open_tasks: Vec<WorkspaceSummary>,
    navigation_tasks: Vec<WorkspaceSummary>,
    execution: crate::execution::Execution,
    execution_scroll: UniformListScrollHandle,
    inspector: Option<inspector::Section>,
    inspector_state: inspector::State,
    options: Options,
    runtime: Arc<tokio::runtime::Runtime>,
    backend: Option<Backend>,
    receive: Task<()>,
    reconnect: Task<()>,
    recovery: recovery::Recovery,
    timer: Task<()>,
    picker: Task<()>,
    picker_pending: bool,
    _subscriptions: Vec<Subscription>,
    focus: FocusHandle,
    input: Entity<TextareaState>,
    dictation: Entity<crate::speech::Dictation>,
    history: composer::History,
    conversation: Entity<Conversation>,
    info: Option<SessionInfo>,
    title: SharedString,
    model: SharedString,
    location: SharedString,
    sessions: Vec<SessionEntry>,
    sidebar: Entity<Sidebar>,
    project_picker: Task<()>,
    choosing_project: bool,
    project: SharedString,
    paths: Vec<PathBuf>,
    pending: Option<PendingSend>,
    clipboard_files: Vec<Arc<tempfile::NamedTempFile>>,
    attachment_names: std::collections::BTreeMap<PathBuf, String>,
    attachment_imports: usize,
    retry: Option<PendingSend>,
    error: Option<SharedString>,
    connected: bool,
    opening: bool,
    connection_stage: SharedString,
    connection_logs: Entity<connection::ConnectionLogs>,
    connection_challenges: std::collections::VecDeque<connection::ConnectionChallenge>,
    auth_dialog_open: bool,
    busy: bool,
    approvals: Vec<Approval>,
    attention: attention::Attention,
    usage: Entity<crate::usage::Usage>,
    quota_request: Instant,
    appearance: Option<Appearance>,
    settings_page: Option<Entity<settings::SettingsPage>>,
    question_form: Option<Entity<questions::QuestionForm>>,
    side_questions: Option<Entity<questions::SideQuestions>>,
    question_history: Option<Entity<questions::QuestionHistory>>,
    child_agents: Option<Entity<subagents::ChildAgents>>,
    input_queue: Option<Entity<input_queue::InputQueue>>,
    input_queue_updates: Option<Subscription>,
    tool_output: Option<Entity<tool_output::ToolOutput>>,
    command_search: Entity<gpui_kit::component::command::CommandState>,
    command_open: bool,
    arguments: slash::Arguments,
    completion_task: Task<()>,
    planning: orbit_protocol::plan::State,
    user_inputs: Vec<orbit_protocol::user_input::Record>,
    context_used: usize,
    context_usage: orbit_client::usage::ContextUsage,
    replaying: bool,
    panel: Option<Entity<panel::Panel>>,
    subagent: Option<(String, Entity<Conversation>)>,
    tasks: Option<Entity<tasks::Tasks>>,
    page: MainPage,
    files: Option<Entity<files::Files>>,
    file_drafts: Vec<Entity<files::Files>>,
    review: Option<Entity<review::Review>>,
}

impl Workspace {
    fn new_conversation(
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<Conversation>, Subscription, Subscription) {
        let conversation = cx.new(Conversation::new);
        let subscription = cx.subscribe(
            &conversation,
            |view, source, request: &crate::file_preview::FileRequest, cx| {
                if let Some(response) = request.take_response() {
                    let current = source.entity_id() == view.conversation.entity_id()
                        || view
                            .subagent
                            .as_ref()
                            .is_some_and(|(_, conversation)| {
                                conversation.entity_id() == source.entity_id()
                            });
                    let session = view
                        .info
                        .as_ref()
                        .filter(|_| current)
                        .map(|info| info.id.clone());
                    if let Some(session) = session {
                        view.command(
                            Command::Settings {
                                request: Request::ReadFile {
                                    allow_missing: false,
                                    session,
                                    path: request.path().to_owned(),
                                },
                                result: response,
                            },
                            cx,
                        );
                    } else {
                        let _ = response.send(Err("会话尚未连接".into()));
                    }
                }
            },
        );
        let tools = cx.subscribe_in(
            &conversation,
            window,
            |view, source, request: &crate::conversation::ToolRequest, window, cx| {
                view.open_tool_record(request.0.clone(), *source != view.conversation, window, cx);
            },
        );
        (conversation, subscription, tools)
    }

    pub(crate) fn new(
        options: Options,
        runtime: Arc<tokio::runtime::Runtime>,
        sidebar: Entity<Sidebar>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("描述任务，输入 / 查看命令…")
                .submit_on_enter(false)
                .auto_grow(3, 8)
        });
        let subscription = cx.subscribe_in(&input, window, |view, input, event, window, cx| {
            if matches!(event, InputEvent::Change) {
                let draft = input.read(cx).value();
                view.history.changed(&draft);
                view.update_completions(draft.as_str(), window, cx);
                cx.emit(WorkspaceEvent::DraftChanged);
                cx.notify();
            }
        });
        let input_focus = input.read(cx).focus_handle(cx);
        let dictation =
            cx.new(|cx| crate::speech::Dictation::new(input.clone(), runtime.clone(), window, cx));
        let focused = cx.on_focus(&input_focus, window, |_, _, cx| cx.notify());
        let blurred = cx.on_blur(&input_focus, window, |_, _, cx| cx.notify());
        let bounds = cx.observe_window_bounds(window, |view, window, cx| {
            view.repair_hidden_focus(window, cx);
        });
        let focus_lost = cx.on_focus_lost(window, |view, window, cx| {
            view.repair_hidden_focus(window, cx);
        });
        let (conversation, image_subscription, tool_subscription) =
            Self::new_conversation(window, cx);
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
            execution: Default::default(),
            execution_scroll: UniformListScrollHandle::new(),
            inspector: None,
            inspector_state: inspector::State::new(cx),
            options,
            runtime,
            backend: None,
            receive: Task::ready(()),
            reconnect: Task::ready(()),
            recovery: Default::default(),
            timer: Task::ready(()),
            picker: Task::ready(()),
            picker_pending: false,
            _subscriptions: vec![
                subscription,
                focused,
                blurred,
                bounds,
                focus_lost,
                image_subscription,
                tool_subscription,
            ],
            focus: cx.focus_handle(),
            input,
            dictation,
            history: Default::default(),
            conversation,
            info: None,
            title: "新会话".into(),
            model: "模型".into(),
            location,
            sessions: Vec::new(),
            sidebar,
            project_picker: Task::ready(()),
            choosing_project: false,
            project: "工作区".into(),
            paths: Vec::new(),
            pending: None,
            clipboard_files: Vec::new(),
            attachment_names: Default::default(),
            attachment_imports: 0,
            retry: None,
            error: None,
            connected: false,
            opening: true,
            connection_stage: "准备连接".into(),
            connection_logs: cx.new(|cx| connection::ConnectionLogs::new(window, cx)),
            connection_challenges: Default::default(),
            auth_dialog_open: false,
            busy: false,
            approvals: Vec::new(),
            attention: Default::default(),
            usage: cx.new(crate::usage::Usage::new),
            quota_request: Instant::now(),
            appearance: None,
            settings_page: None,
            question_form: None,
            side_questions: None,
            question_history: None,
            child_agents: None,
            input_queue: None,
            input_queue_updates: None,
            tool_output: None,
            command_search: cx
                .new(|cx| gpui_kit::component::command::CommandState::new(window, cx)),
            command_open: false,
            arguments: Default::default(),
            completion_task: Task::ready(()),
            planning: Default::default(),
            user_inputs: Vec::new(),
            context_used: 0,
            context_usage: Default::default(),
            replaying: false,
            panel: None,
            subagent: None,
            tasks: None,
            page: MainPage::Conversation,
            files: None,
            file_drafts: Vec::new(),
            review: None,
        };
        view.input
            .update(cx, |input, cx| input.focus(window, cx));
        view.connect(window, cx);
        view.timer = cx.spawn(async move |weak, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(1))
                    .await;
                if weak
                    .update_in(cx, |view, window, cx| {
                        if view.connected
                            && !view.opening
                            && view.quota_request.elapsed() >= orbit_client::usage::REFRESH_INTERVAL
                        {
                            view.refresh_usage(cx);
                        }
                        view.show_pending_decision(window, cx);
                        // The footer borrows the quota view's summary; its reset
                        // countdown also needs a render while the dialog is closed.
                        if view.info.is_some() {
                            if !window.has_active_sheet(cx) && !window.has_active_dialog(cx) {
                                view.refresh_input_queue(false, cx);
                            }
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
        self.disconnect_tool_output(cx);
        if let Some(panel) = &self.input_queue {
            panel.update(cx, |panel, cx| panel.disconnected(cx));
        }
        if let Some(panel) = &self.child_agents {
            panel.update(cx, |panel, cx| panel.disconnected(cx));
        }
        if let Some(form) = &self.question_form {
            form.update(cx, |form, cx| form.disconnected(cx));
        }
        if let Some(files) = &self.files {
            files.update(cx, |files, cx| files.disconnected(cx));
        }
        if let Some(review) = &self.review {
            review.update(cx, |review, cx| review.disconnected(cx));
        }
        self.clear_connection_challenges(window, cx);
        if let Some(tasks) = &self.tasks {
            tasks.update(cx, |tasks, cx| tasks.disconnected(window, cx));
        }
        if self.error.is_some() {
            window.remove_notification1::<Workspace>(SharedString::from(self.key.clone()), cx);
        }
        self.error = None;
        self.opening = true;
        self.connected = false;
        self.connection_stage = "准备连接".into();
        let environment = self.environment();
        self.connection_logs
            .update(cx, |logs, cx| logs.reset(environment, window, cx));
        let mut options = self.options.clone();
        options.allow_mcp_start |=
            crate::preferences::Preferences::allows_mcp_start(&options.config, cx);
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
            Event::AgentStatuses {
                session,
                workspace,
                agents,
            } => {
                if self
                    .info
                    .as_ref()
                    .is_some_and(|info| info.id == session && info.workspace == workspace)
                {
                    self.execution.sync_agents(&agents);
                }
            }
            Event::Tasks { session, result } => {
                if let Some(tasks) = &self.tasks {
                    tasks.update(cx, |tasks, cx| tasks.receive(&session, result, window, cx));
                }
            }
            Event::Connected => {
                self.connected = true;
                self.connection_stage = "正在恢复会话…".into();
            }
            Event::ConnectionStage(stage) => self.connection_stage = stage.into(),
            Event::ConnectionLogs(logs) => {
                self.connection_logs
                    .update(cx, |view, cx| view.sync(logs, window, cx));
            }
            Event::Subagent { agent, page, reset } => {
                if reset {
                    let (conversation, image_subscription, tool_subscription) =
                        Self::new_conversation(window, cx);
                    self._subscriptions
                        .extend([image_subscription, tool_subscription]);
                    self.subagent = Some((agent.clone(), conversation.clone()));
                    let owner = cx.weak_entity();
                    let title = format!("子 Agent · {agent}");
                    if self.visible {
                        let width = (window.rem_size() * 48.).min(
                            (window.viewport_size().width - window.rem_size() * 2.).max(px(0.)),
                        );
                        window.open_sheet(cx, move |sheet, _, _| {
                            let owner = owner.clone();
                            sheet
                                .title(title.clone())
                                .size(width)
                                .child(
                                    div()
                                        .h_full()
                                        .child(conversation.clone()),
                                )
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
                if self
                    .info
                    .as_ref()
                    .is_some_and(|info| info.id == session)
                {
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
            Event::File { path, bytes } => {
                self.show_files(None, window, cx);
                if let Some(files) = &self.files {
                    files.update(cx, |files, cx| files.receive_file(path, bytes, window, cx));
                }
            }
            Event::Usage { session, result } => {
                if self.info.as_ref().is_some_and(|info| {
                    info.id == session
                        && result
                            .as_ref()
                            .map_or(true, |snapshot| {
                                snapshot.model.rsplit("::").next() == info.model.rsplit("::").next()
                            })
                }) {
                    self.usage
                        .update(cx, |usage, cx| match result {
                            Ok(snapshot) => usage.set(snapshot, cx),
                            Err(error) => usage.error(error, cx),
                        });
                }
            }
            Event::Disconnected(error) => {
                self.disconnect_tool_output(cx);
                if let Some(panel) = &self.input_queue {
                    panel.update(cx, |panel, cx| panel.disconnected(cx));
                }
                if let Some(panel) = &self.child_agents {
                    panel.update(cx, |panel, cx| panel.disconnected(cx));
                }
                if let Some(form) = &self.question_form {
                    form.update(cx, |form, cx| form.disconnected(cx));
                }
                if let Some(files) = &self.files {
                    files.update(cx, |files, cx| files.disconnected(cx));
                }
                if let Some(review) = &self.review {
                    review.update(cx, |review, cx| review.disconnected(cx));
                }
                self.clear_connection_challenges(window, cx);
                if let Some(tasks) = &self.tasks {
                    tasks.update(cx, |tasks, cx| tasks.disconnected(window, cx));
                }
                self.connected = false;
                self.opening = false;
                self.retry = self
                    .pending
                    .take()
                    .or(self.retry.take());
                if let Some(delay) = self.info.as_ref().and_then(|_| {
                    self.recovery
                        .disconnected(Instant::now())
                }) {
                    self.error = Some(error.into());
                    self.reconnect = cx.spawn(async move |owner, cx| {
                        cx.background_executor()
                            .timer(delay)
                            .await;
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
                    self.history.accepted(&pending.text);
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
                    self.paths
                        .retain(|path| !pending.paths.contains(path));
                    self.prune_draft_attachments();
                    self.busy = true;
                    self.refresh_input_queue(true, cx);
                }
            }
            Event::Response(response) => self.response(response, window, cx),
        }
        self.show_pending_decision(window, cx);
        if previous != self.summary() {
            cx.emit(WorkspaceEvent::Changed);
        }
        cx.emit(WorkspaceEvent::DraftChanged);
        cx.notify();
    }

    fn profile(&mut self, info: SessionInfo, window: &mut Window, cx: &mut Context<Self>) {
        if self.context_usage.select_profile(&info) {
            self.error = None;
            self.execution.clear_failure();
            window.remove_notification1::<Workspace>(SharedString::from(self.key.clone()), cx);
        }
        if let Some(used) = self.context_usage.used_tokens() {
            self.context_used = used;
        }
        if self
            .info
            .as_ref()
            .is_some_and(|previous| previous.workspace != info.workspace || previous.id != info.id)
        {
            if self.attention.clear() {
                window.close_all_dialogs(cx);
            }
            self.question_form = None;
            self.disconnect_tool_output(cx);
            if let Some(panel) = &self.input_queue {
                panel.update(cx, |panel, cx| panel.disconnected(cx));
            }
        }
        if self.visible
            && (self.side_questions.is_some()
                || self.question_history.is_some()
                || self.child_agents.is_some()
                || self.input_queue.is_some())
            && self
                .info
                .as_ref()
                .is_some_and(|previous| {
                    previous.workspace != info.workspace || previous.id != info.id
                })
        {
            window.close_sheet(cx);
        }
        if let Some(previous) = &self.info {
            if previous.workspace != info.workspace {
                self.tasks = None;
                self.page = MainPage::Conversation;
                if let Some(files) = self.files.take()
                    && files.read(cx).has_unsaved_changes()
                {
                    self.file_drafts.push(files);
                }
                self.review = None;
                self.side_questions = None;
                self.question_history = None;
                self.child_agents = None;
                self.input_queue = None;
            } else if previous.id != info.id {
                self.side_questions = None;
                self.question_history = None;
                self.child_agents = None;
                self.input_queue = None;
                if let Some(tasks) = &self.tasks {
                    tasks.update(cx, |tasks, cx| {
                        tasks.bind_session(info.id.clone(), window, cx)
                    });
                }
                if let Some(files) = &self.files {
                    files.update(cx, |files, cx| files.bind_session(info.id.clone(), cx));
                }
                self.review = None;
                if self.page == MainPage::Review {
                    self.page = MainPage::Conversation;
                }
            }
        }
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
        if let Some(settings) = &self.settings_page {
            settings.update(cx, |settings, cx| {
                settings.bind_context(Some(info.id.clone()), info.workspace.clone(), window, cx)
            });
        }
        self.info = Some(info);
        self.watch_agents(cx);
        self.refresh_title(cx);
    }

    fn response(&mut self, response: Response, window: &mut Window, cx: &mut Context<Self>) {
        match response {
            Response::Opened(info) => {
                self.disconnect_tool_output(cx);
                self.options.relink_on_resume = false;
                self.options.recover_draft = false;
                // This entity owns one task's composer. Restoring transport
                // history must never overwrite text edited while offline.
                self.context_usage = Default::default();
                self.context_used = 0;
                self.profile(info, window, cx);
                self.opening = true;
                self.busy = false;
                self.error = None;
                self.approvals.clear();
                self.user_inputs.clear();
                if self.visible
                    && (self.side_questions.is_some() || self.question_history.is_some())
                {
                    window.close_sheet(cx);
                }
                self.side_questions = None;
                self.question_history = None;
                if let (Some(panel), Some(info)) = (&self.input_queue, &self.info) {
                    let retained = panel.update(cx, |panel, cx| {
                        let retained = panel.rebind(&info.id, &info.workspace, &self.options, cx);
                        if retained {
                            panel.reconnected(cx);
                        }
                        retained
                    });
                    if !retained {
                        self.input_queue = None;
                    }
                }
                if let (Some(panel), Some(info)) = (&self.child_agents, &self.info) {
                    let retained = panel.update(cx, |panel, cx| {
                        let retained = panel.rebind(&info.id, &info.workspace, &self.options, cx);
                        if retained {
                            panel.reconnected(cx);
                        }
                        retained
                    });
                    if !retained {
                        self.child_agents = None;
                    }
                }
                self.planning = Default::default();
                self.panel = None;
                self.subagent = None;
                self.usage = cx.new(crate::usage::Usage::new);
                let (conversation, image_subscription, tool_subscription) =
                    Self::new_conversation(window, cx);
                self.conversation = conversation;
                self._subscriptions
                    .extend([image_subscription, tool_subscription]);
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
                self.ensure_input_queue(window, cx);
                self.refresh_input_queue(true, cx);
            }
            Response::Profile(info) => {
                self.usage = cx.new(crate::usage::Usage::new);
                self.profile(*info, window, cx);
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
                    let images = match &update {
                        Update::Started { presentation, .. } => presentation
                            .image_assets
                            .iter()
                            .map(|image| image.path.clone())
                            .collect::<Vec<_>>(),
                        Update::Agent(event) => match event.as_ref() {
                            orbit_client::protocol::events::AgentEvent::InputReceived {
                                attachments,
                                ..
                            } => attachments
                                .iter()
                                .filter(|a| a.is_image())
                                .map(|image| {
                                    image
                                        .path()
                                        .to_string_lossy()
                                        .into_owned()
                                })
                                .collect(),
                            _ => vec![],
                        },
                        _ => vec![],
                    };
                    for path in images {
                        if let Some(info) = &self.info {
                            self.command(
                                Command::ReadImage {
                                    session: info.id.clone(),
                                    path,
                                },
                                cx,
                            );
                        }
                    }
                    if let Update::Profile(info) = update {
                        let changed = self
                            .info
                            .as_ref()
                            .is_none_or(|current| {
                                current.model != info.model || current.workspace != info.workspace
                            });
                        self.profile(*info, window, cx);
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
                if restoring && !self.opening {
                    self.history
                        .seed(self.conversation.read(cx).prompts());
                }
                self.execution
                    .apply_progress(&page.tool_progress);
                if self.input_queue.is_none() {
                    self.ensure_input_queue(window, cx);
                }
                if (restoring && !self.opening) || finished {
                    self.refresh_input_queue(true, cx);
                }
                self.sync_tool_output(restoring, window, cx);
                self.refresh_title(cx);
                if finished && !restoring {
                    self.command(Command::Request(Request::AllSessions), cx);
                }
            }
            Response::Usage(snapshot) => self
                .usage
                .update(cx, |usage, cx| usage.set(snapshot, cx)),
            Response::Report(report) => self.show_report(report.title, report.text, window, cx),
            Response::File(_) => self.report_error("文件响应缺少路径，请从文件页重新打开。", cx),
            Response::Questions(questions) => self.show_questions(questions, window, cx),
            Response::Plugins(catalog) => self.show_plugins(catalog, window, cx),
            Response::UserInputs(_) => self.open_question_history(window, cx),
            Response::Subagent(page) => {
                let (conversation, image_subscription, tool_subscription) =
                    Self::new_conversation(window, cx);
                self._subscriptions
                    .extend([image_subscription, tool_subscription]);
                conversation.update(cx, |view, cx| {
                    for update in page.updates {
                        view.apply(update, cx);
                    }
                });
                let width = (window.rem_size() * 48.)
                    .min((window.viewport_size().width - window.rem_size() * 2.).max(px(0.)));
                window.open_sheet(cx, move |sheet, _, _| {
                    sheet
                        .title("子 Agent")
                        .size(width)
                        .child(
                            div()
                                .h_full()
                                .child(conversation.clone()),
                        )
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
        if !self.visible || self.page != MainPage::Conversation {
            return;
        }
        if self.command_open {
            self.submit_completion(window, cx);
            return;
        }
        if slash::is_command_input(&self.input.read(cx).value()) && self.dispatch_slash(window, cx)
        {
            return;
        }
        if !self.can_send(cx) {
            return;
        }
        self.dictation
            .update(cx, |dictation, cx| dictation.commit(cx));
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
                paths: self
                    .paths
                    .iter()
                    .map(|path| {
                        (
                            path.clone(),
                            self.attachment_names
                                .get(path)
                                .cloned()
                                .unwrap_or_else(|| {
                                    path.file_name()
                                        .unwrap_or_default()
                                        .to_string_lossy()
                                        .into_owned()
                                }),
                        )
                    })
                    .collect(),
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
            && !self.picker_pending
            && self.attachment_imports == 0
            && self.info.is_some()
            && draft_ready(&self.input.read(cx).value(), &self.paths, self.busy)
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
        if self.pending.is_some() || self.picker_pending {
            return;
        }
        self.picker_pending = true;
        cx.notify();
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("添加附件".into()),
        });
        let session = self
            .info
            .as_ref()
            .map(|info| info.id.clone());
        self.picker = cx.spawn(async move |weak, cx| {
            let result = prompt.await;
            let _ = weak.update_in(cx, |view, window, cx| {
                view.picker_pending = false;
                if view.info.as_ref().map(|info| &info.id) == session.as_ref() {
                    match result {
                        Ok(Ok(Some(paths))) => view.add_paths(&paths, window, cx),
                        Ok(Err(error)) => view.report_error(format!("无法选择附件：{error}"), cx),
                        _ => {}
                    }
                }
                cx.notify();
            });
        });
    }

    fn paste_attachments(
        &mut self,
        item: &ClipboardItem,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.pending.is_some() || self.picker_pending {
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
        let mut handled = false;
        for entry in item.entries() {
            if let ClipboardEntry::ExternalPaths(paths) = entry {
                self.add_paths(&paths.0, window, cx);
                handled = true;
            }
        }
        if images.is_empty()
            && !handled
            && let Some(text) = item.text()
            && let Some(paths) = self.pasted_paths(&text)
        {
            self.add_paths(&paths, window, cx);
            return true;
        }
        let text = item
            .text()
            .filter(|text| images.is_empty() && !handled && composer::large_paste(text));
        if images.is_empty() && text.is_none() {
            return handled;
        }
        let session = self
            .info
            .as_ref()
            .map(|info| info.id.clone());
        self.attachment_imports += 1;
        cx.notify();
        let job =
            self.runtime
                .spawn_blocking(move || -> anyhow::Result<Vec<tempfile::NamedTempFile>> {
                    use std::io::Write;
                    let mut files: Vec<_> = images
                        .into_iter()
                        .map(|image| {
                            let mut file = tempfile::Builder::new()
                                .prefix("orbit-image-")
                                .suffix(&format!(".{}", image.format().extension()))
                                .tempfile()?;
                            file.write_all(image.bytes())?;
                            Ok(file)
                        })
                        .collect::<anyhow::Result<_>>()?;
                    if let Some(text) = text {
                        let mut file = tempfile::Builder::new()
                            .prefix("已粘贴的文本-")
                            .suffix(".txt")
                            .tempfile()?;
                        file.write_all(text.as_bytes())?;
                        files.push(file);
                    }
                    Ok(files)
                });
        let previous = std::mem::replace(&mut self.picker, Task::ready(()));
        self.picker = cx.spawn(async move |weak, cx| {
            previous.await;
            let result = job.await;
            let _ = weak.update(cx, |view, cx| {
                view.attachment_imports = view
                    .attachment_imports
                    .saturating_sub(1);
                cx.notify();
                if view.info.as_ref().map(|info| &info.id) != session.as_ref() {
                    return;
                }
                match result {
                    Ok(Ok(files)) => {
                        for file in &files {
                            if file
                                .path()
                                .file_name()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .starts_with("已粘贴的文本-")
                            {
                                view.attachment_names
                                    .insert(file.path().to_owned(), "已粘贴的文本.txt".into());
                            }
                        }
                        view.paths.extend(
                            files
                                .iter()
                                .map(|file| file.path().to_path_buf()),
                        );
                        view.clipboard_files
                            .extend(files.into_iter().map(Arc::new));
                    }
                    Ok(Err(error)) => view.report_error(format!("粘贴附件失败：{error}"), cx),
                    Err(error) => view.report_error(format!("粘贴附件失败：{error}"), cx),
                }
                cx.emit(WorkspaceEvent::DraftChanged);
                cx.notify();
            });
        });
        true
    }

    fn show_usage(&self, window: &mut Window, cx: &mut Context<Self>) {
        let usage = self.usage.clone();
        let weak = cx.weak_entity();
        crate::dialog::open(window, cx, move |dialog, _, _| {
            let weak = weak.clone();
            dialog
                .title("用量")
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

fn draft_ready(text: &str, paths: &[PathBuf], _busy: bool) -> bool {
    !text.trim().is_empty() || !paths.is_empty()
}

#[derive(Clone, PartialEq, Eq)]
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
    use super::{PendingSend, draft_ready};

    #[test]
    fn image_drafts_can_be_queued_while_busy() {
        let paths = ["draft.png".into()];
        assert!(draft_ready("", &paths, true));
        assert!(draft_ready("look at this image", &paths, true));
        assert!(draft_ready("", &paths, false));
        assert!(draft_ready("look at this image", &paths, false));
        assert!(draft_ready("a text supplement", &[], true));
        assert!(!draft_ready("  ", &[], true));
    }

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
