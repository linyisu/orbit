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
    pub(crate) loading: bool,
    pub(crate) attention: bool,
    pub(crate) unread: bool,
}

pub(crate) enum WorkspaceEvent {
    Open(Box<Options>),
    Activate(String),
    Close(String),
    Changed,
    DraftChanged,
    Catalog(Vec<SessionEntry>),
}
impl EventEmitter<WorkspaceEvent> for Workspace {}

impl Workspace {
    pub(crate) fn saved(&self, cx: &App) -> crate::workbench::storage::SavedTask {
        let mut options = self.options.clone();
        if let Some(info) = &self.info {
            options.workspace = Some(PathBuf::from(&info.workspace));
            options.model = Some(info.model.clone());
            options.resume = (info.persisted || self.pending.is_some() || self.retry.is_some())
                .then(|| info.id.clone());
            options.recover_draft = !info.persisted && options.resume.is_some();
        }
        options.relink_on_resume = false;
        crate::workbench::storage::SavedTask {
            key: self.key.clone(),
            options,
            title: self.title.to_string(),
            text: self.input.read(cx).value().to_string(),
            paths: self.paths.clone(),
            retry: self.pending.as_ref().or(self.retry.as_ref()).map(|send| {
                crate::workbench::storage::SavedSend {
                    id: send.id.clone(),
                    session: send.session.clone(),
                    text: send.text.to_string(),
                    paths: send.paths.clone(),
                    steer: send.steer,
                }
            }),
            clipboard_files: self
                .clipboard_files
                .iter()
                .filter(|file| self.paths.iter().any(|path| path == file.path()))
                .cloned()
                .collect(),
        }
    }
    pub(crate) fn restore(
        &mut self,
        saved: crate::workbench::storage::SavedTask,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.key = saved.key;
        self.title = saved.title.into();
        self.paths = saved.paths;
        self.input
            .update(cx, |input, cx| input.set_value(saved.text, window, cx));
        // A restored send is only an explicit retry candidate, never a queued
        // command. Start retains its idempotency key across a desktop restart.
        self.retry = saved.retry.map(|send| PendingSend {
            id: send.id,
            session: send.session,
            text: send.text.into(),
            paths: send.paths,
            steer: send.steer,
        });
        cx.notify();
    }
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
        (self.info.as_ref().is_some_and(|info| info.id == id)
            || self.options.resume.as_deref() == Some(id))
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
            loading: self.opening
                || self.recovery.is_active()
                || self.busy
                || self.pending.is_some(),
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
        if self.recovery.is_active() {
            return "正在重新连接";
        }
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
        self.info
            .as_ref()
            .map_or(self.options.resume.is_none(), |info| !info.persisted)
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
        if visible {
            // A retained workspace can return with different tab widths and a
            // different sidebar split. Reveal against its newly painted bounds.
            let owner = cx.weak_entity();
            window.on_next_frame(move |_, cx| {
                let _ = owner.update(cx, |view, cx| {
                    if view.visible {
                        view.reveal_task();
                        cx.notify();
                    }
                });
            });
        }
        if visible && changed {
            self.reveal_task();
            self.input.update(cx, |input, cx| input.focus(window, cx));
            if self.connected {
                self.command(Command::Request(Request::AllSessions), cx);
            }
        }
        cx.notify();
    }
    pub(crate) fn set_catalog(&mut self, catalog: Vec<SessionEntry>, cx: &mut Context<Self>) {
        self.sessions = catalog;
        self.refresh_title(cx);
        self.regroup(cx);
        cx.notify();
    }

    pub(super) fn refresh_title(&mut self, cx: &App) {
        let id = self
            .info
            .as_ref()
            .map(|info| info.id.as_str())
            .or(self.options.resume.as_deref());
        let title = self
            .info
            .as_ref()
            .filter(|info| !info.title.trim().is_empty())
            .map(|info| info.title.as_str())
            .or_else(|| {
                self.sessions
                    .iter()
                    .find(|entry| Some(entry.id.as_ref()) == id)
                    .map(|entry| entry.title.as_ref())
            });
        self.title = session_title(
            self.info.as_ref().is_some_and(|info| info.persisted) || self.options.resume.is_some(),
            title,
            self.conversation.read(cx).first_prompt(),
            &self.title,
        )
        .into();
    }
    pub(crate) fn set_open_tasks(
        &mut self,
        tasks: Vec<WorkspaceSummary>,
        navigation_tasks: Vec<WorkspaceSummary>,
        cx: &mut Context<Self>,
    ) {
        let layout_changed = self.open_tasks.len() != tasks.len()
            || self
                .open_tasks
                .iter()
                .zip(&tasks)
                .any(|(old, new)| old.key != new.key || old.title != new.title);
        self.open_tasks = tasks;
        self.navigation_tasks = navigation_tasks;
        if layout_changed {
            self.reveal_task();
        }
        cx.notify();
    }

    fn reveal_task(&self) {
        if let Some(index) = self.open_tasks.iter().position(|task| task.key == self.key) {
            self.task_scroll.scroll_to_item(index);
        }
    }
}

fn session_title(
    persisted: bool,
    title: Option<&str>,
    prompt: Option<&str>,
    previous: &str,
) -> String {
    title
        .filter(|title| !title.trim().is_empty())
        .or_else(|| {
            (!matches!(previous, "" | "新会话" | "历史会话" | "未命名会话")).then_some(previous)
        })
        .or(prompt)
        .unwrap_or(if persisted {
            "未命名会话"
        } else {
            "新会话"
        })
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::session_title;

    #[test]
    fn draft_ids_do_not_turn_new_tasks_into_history() {
        assert_eq!(session_title(false, None, None, "历史会话"), "新会话");
    }

    #[test]
    fn untitled_history_keeps_its_saved_title_or_uses_the_first_prompt() {
        assert_eq!(session_title(true, Some(""), None, "创建目录"), "创建目录");
        assert_eq!(
            session_title(true, None, Some("查看\n项目结构"), "历史会话"),
            "查看 项目结构"
        );
        assert_eq!(
            session_title(true, Some("新的标题"), Some("旧的问题"), "旧的标题"),
            "新的标题"
        );
        assert_eq!(session_title(true, None, None, "历史会话"), "未命名会话");
    }
}
