//! The workspace publishes intent and summaries. Its window host retains the
//! entities, so navigation never tears down a running conversation.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct WorkspaceSummary {
    pub(crate) key: String,
    pub(crate) session: Option<String>,
    pub(crate) title: SharedString,
    pub(crate) project: SharedString,
    pub(crate) status: &'static str,
    pub(crate) busy: bool,
    pub(crate) attention: bool,
    pub(crate) unread: bool,
}

pub(crate) enum WorkspaceEvent {
    Open(Box<Options>),
    Activate(String),
    Close(String),
    Changed,
    Catalog(Vec<SessionEntry>),
}
impl EventEmitter<WorkspaceEvent> for Workspace {}

impl Workspace {
    pub(super) fn adjacent_task(&self, forward: bool, cx: &mut Context<Self>) {
        let Some(ix) = self.open_tasks.iter().position(|task| task.key == self.key) else {
            return;
        };
        let len = self.open_tasks.len();
        let next = if forward {
            (ix + 1) % len
        } else {
            (ix + len - 1) % len
        };
        cx.emit(WorkspaceEvent::Activate(self.open_tasks[next].key.clone()));
    }
    pub(crate) fn options(&self) -> &Options {
        &self.options
    }
    pub(crate) fn catalog(&self) -> &[SessionEntry] {
        &self.sessions
    }
    pub(crate) fn key(&self) -> &str {
        &self.key
    }
    pub(crate) fn can_resume(&self, id: &str, options: &Options) -> bool {
        self.connected
            && self.info.as_ref().is_some_and(|info| info.id == id)
            && options
                .workspace
                .as_ref()
                .is_none_or(|path| path == &PathBuf::from(self.location.as_ref()))
    }
    pub(crate) fn summary(&self) -> WorkspaceSummary {
        WorkspaceSummary {
            key: self.key.clone(),
            session: self
                .info
                .as_ref()
                .map(|info| info.id.clone())
                .or_else(|| self.options.resume.clone()),
            title: self.title.clone(),
            project: self.project.clone(),
            status: self.status(),
            busy: self.busy || self.pending.is_some(),
            attention: self.error.is_some()
                || !self.approvals.is_empty()
                || self
                    .user_inputs
                    .iter()
                    .any(|record| record.status == orbit_protocol::user_input::Status::Pending)
                || self
                    .planning
                    .plan
                    .as_ref()
                    .is_some_and(|plan| plan.status == orbit_protocol::plan::Status::Review),
            unread: false,
        }
    }
    pub(crate) fn status(&self) -> &'static str {
        if self.opening {
            return "正在打开";
        }
        if !self.connected {
            return "已断开";
        }
        if self.error.is_some() {
            return "需要处理";
        }
        if !self.approvals.is_empty() {
            return "等待审批";
        }
        if self
            .user_inputs
            .iter()
            .any(|record| record.status == orbit_protocol::user_input::Status::Pending)
        {
            return "等待回答";
        }
        if self
            .planning
            .plan
            .as_ref()
            .is_some_and(|plan| plan.status == orbit_protocol::plan::Status::Review)
        {
            return "等待计划确认";
        }
        if self.pending.is_some() {
            return "正在发送";
        }
        self.execution.phase().label()
    }
    pub(crate) fn is_empty_draft(&self, cx: &App) -> bool {
        self.info.as_ref().is_some_and(|info| !info.persisted)
            && self.conversation.read(cx).is_empty()
            && self.input.read(cx).value().is_empty()
            && self.paths.is_empty()
            && !self.busy
            && self.pending.is_none()
    }
    pub(crate) fn set_visible(
        &mut self,
        visible: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let changed = self.visible != visible;
        self.visible = visible;
        if !visible && changed {
            window.remove_notification::<Workspace>(cx);
        }
        if visible && changed {
            self.input.update(cx, |input, cx| input.focus(window, cx));
            if self.connected {
                self.command(Command::Request(Request::AllSessions), cx);
            }
        }
        cx.notify();
    }
    pub(crate) fn set_catalog(&mut self, catalog: Vec<SessionEntry>, cx: &mut Context<Self>) {
        self.sessions = catalog;
        self.regroup(cx);
        cx.notify();
    }
    pub(crate) fn set_open_tasks(&mut self, tasks: Vec<WorkspaceSummary>, cx: &mut Context<Self>) {
        self.open_tasks = tasks;
        cx.notify();
    }
}
