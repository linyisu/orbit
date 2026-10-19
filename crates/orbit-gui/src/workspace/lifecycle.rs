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
    CloseTask,
    Settings(Entity<SettingsPage>),
    CloseSettings,
    Changed,
    DraftChanged,
    Catalog(Vec<SessionEntry>),
    SessionDeleted { id: String, options: Box<Options> },
}

impl EventEmitter<WorkspaceEvent> for Workspace {}

impl Workspace {
    pub(super) fn show_files(
        &mut self,
        path: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(info) = &self.info else { return };
        let session = info.id.clone();
        let workspace = info.workspace.clone();
        if !self
            .files
            .as_ref()
            .is_some_and(|files| {
                files
                    .read(cx)
                    .belongs_to(&session, &workspace)
            })
        {
            if let Some(previous) = self.files.take()
                && previous.read(cx).has_unsaved_changes()
            {
                self.file_drafts.push(previous);
            }
            if let Some(index) = self
                .file_drafts
                .iter()
                .position(|files| {
                    files
                        .read(cx)
                        .belongs_to_workspace(&workspace)
                })
            {
                let files = self.file_drafts.remove(index);
                files.update(cx, |files, cx| files.bind_session(session, cx));
                self.files = Some(files);
            } else {
                let owner = cx.weak_entity();
                self.files =
                    Some(cx.new(|cx| files::Files::new(owner, session, workspace, window, cx)));
            }
        }
        self.dictation
            .update(cx, |dictation, cx| dictation.cancel(window, cx));
        self.page = MainPage::Files;
        if let Some(files) = &self.files {
            files.update(cx, |files, cx| {
                if let Some(path) = path {
                    files.open(path, window, cx);
                }
                files.focus(window, cx);
            });
        }
        cx.notify();
    }

    pub(super) fn close_files(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_composer(window, cx);
    }

    pub(super) fn show_review(
        &mut self,
        path: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(info) = &self.info else { return };
        let session = info.id.clone();
        let workspace = info.workspace.clone();
        if !self
            .review
            .as_ref()
            .is_some_and(|review| {
                review
                    .read(cx)
                    .belongs_to(&session, &workspace)
            })
        {
            let owner = cx.weak_entity();
            self.review =
                Some(cx.new(|cx| review::Review::new(owner, session, workspace, window, cx)));
        }
        self.dictation
            .update(cx, |dictation, cx| dictation.cancel(window, cx));
        self.page = MainPage::Review;
        if let Some(review) = &self.review {
            review.update(cx, |review, cx| {
                review.activate(path, cx);
                review.focus(window, cx);
            });
        }
        cx.notify();
    }

    pub(super) fn show_checkpoints(
        &mut self,
        path: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_review(None, window, cx);
        if let Some(review) = &self.review {
            review.update(cx, |review, cx| review.activate_checkpoint(path, cx));
        }
    }

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
            attachment_names: self
                .attachment_names
                .iter()
                .filter(|(path, _)| self.paths.contains(path))
                .map(|(p, n)| (p.clone(), n.clone()))
                .collect(),
            retry: self
                .pending
                .as_ref()
                .or(self.retry.as_ref())
                .map(|send| crate::workbench::storage::SavedSend {
                    id: send.id.clone(),
                    session: send.session.clone(),
                    text: send.text.to_string(),
                    paths: send.paths.clone(),
                    steer: send.steer,
                }),
            clipboard_files: self
                .clipboard_files
                .iter()
                .filter(|file| {
                    self.paths
                        .iter()
                        .any(|path| path == file.path())
                })
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
        self.attachment_names = saved.attachment_names;
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
        let Some(ix) = self
            .open_tasks
            .iter()
            .position(|task| task.key == self.key)
        else {
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

    pub(crate) fn files_need_attention(&self, cx: &App) -> bool {
        self.files
            .iter()
            .chain(self.file_drafts.iter())
            .any(|files| files.read(cx).has_unsaved_changes())
    }

    pub(crate) fn files_saving(&self, cx: &App) -> bool {
        self.files
            .iter()
            .chain(self.file_drafts.iter())
            .any(|files| files.read(cx).is_saving())
    }

    pub(crate) fn discard_file_edits(&mut self, cx: &mut Context<Self>) {
        self.files = None;
        self.file_drafts.clear();
        if self.page == MainPage::Files {
            self.page = MainPage::Conversation;
        }
        cx.notify();
    }

    pub(crate) fn focus_file_edits(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(files) = self.files.clone() {
            self.dictation
                .update(cx, |dictation, cx| dictation.cancel(window, cx));
            self.page = MainPage::Files;
            files.update(cx, |files, cx| files.focus(window, cx));
            cx.notify();
        }
    }

    pub(crate) fn can_resume(&self, id: &str, options: &Options) -> bool {
        (self
            .info
            .as_ref()
            .is_some_and(|info| info.id == id)
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
                || !self.connection_challenges.is_empty()
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
        if !self.connection_challenges.is_empty() {
            return "等待 SSH 验证";
        }
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

    pub(super) fn is_new_conversation(&self, cx: &App) -> bool {
        self.options.resume.is_none()
            && self
                .info
                .as_ref()
                .is_none_or(|info| !info.persisted)
            && self.conversation.read(cx).is_empty()
            && !self.busy
    }

    pub(crate) fn is_empty_draft(&self, cx: &App) -> bool {
        self.options.resume.is_none()
            && self.info.as_ref().is_none_or(|info| !info.persisted)
            && self.conversation.read(cx).is_empty()
            && !self.has_draft(cx)
            && !self.busy
            && !self.files_need_attention(cx)
            && !self.files_saving(cx)
            && !self
                .settings_page
                .as_ref()
                .is_some_and(|page| page.read(cx).has_pending_work(cx))
            // Accessory editors can own unsent drafts or independent requests
            // even while the main conversation remains empty.
            && self.side_questions.is_none()
            && self.child_agents.is_none()
            && !self.input_queue.as_ref().is_some_and(|queue| queue.read(cx).has_drafts(cx))
            && self.question_form.is_none()
            && self.tasks.is_none()
    }

    pub(crate) fn set_visible(
        &mut self,
        visible: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let changed = self.visible != visible;
        self.visible = visible;
        if changed {
            self.watch_agents(cx);
        }
        if !visible && changed {
            self.dictation
                .update(cx, |dictation, cx| dictation.cancel(window, cx));
            if self.attention.suspend() {
                window.close_all_dialogs(cx);
            }
            self.suspend_connection_challenge(window, cx);
            window.remove_notification1::<Workspace>(SharedString::from(self.key.clone()), cx);
        }
        if visible && changed {
            self.focus_page(window, cx);
            if self.connected {
                self.command(Command::Request(Request::AllSessions), cx);
            }
        }
        if visible {
            self.show_connection_challenge(window, cx);
            self.show_pending_decision(window, cx);
        }
        cx.notify();
    }

    pub(crate) fn set_catalog(&mut self, catalog: Vec<SessionEntry>, cx: &mut Context<Self>) {
        self.sessions = catalog;
        self.refresh_title(cx);
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
            self.info
                .as_ref()
                .is_some_and(|info| info.persisted)
                || self.options.resume.is_some(),
            title,
            self.conversation
                .read(cx)
                .first_prompt(),
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
        self.open_tasks = tasks;
        self.navigation_tasks = navigation_tasks;
        cx.notify();
    }

    pub(crate) fn new_task(&mut self, cx: &mut Context<Self>) {
        self.open(None, cx);
    }

    /// An explicit new-task action can reuse the tab, but its old draft connection must not retain
    /// obsolete model or MCP settings.
    pub(crate) fn reopen_empty_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.opening && self.is_empty_draft(cx) {
            self.connect(window, cx);
        }
    }

    pub(crate) fn reconnect_if_needed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.connected && !self.opening && !self.recovery.is_active() {
            self.connect(window, cx);
        }
    }

    pub(crate) fn connection_unestablished(&self) -> bool {
        self.info.is_none() && !self.connected
    }

    pub(crate) fn configuration_required(&self) -> bool {
        !self.opening
            && self.info.as_ref().is_some_and(|info| {
                info.phase == orbit_protocol::SessionPhase::Browsing && info.models.is_empty()
            })
    }

    pub(crate) fn startup_diagnostic(&self) -> String {
        format!("{:?}; error={:?}", self.summary(), self.error)
    }

    pub(crate) fn has_draft(&self, cx: &App) -> bool {
        self.pending.is_some()
            || self.picker_pending
            || self.attachment_imports != 0
            || self.retry.is_some()
            || !self.paths.is_empty()
            || !self.input.read(cx).value().is_empty()
    }

    pub(crate) fn window_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.settings(false, window, cx);
    }

    pub(crate) fn window_commands(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.commands(window, cx);
    }

    pub(crate) fn window_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_sessions(window, cx);
    }

    pub(crate) fn focus_composer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.page = MainPage::Conversation;
        if window.viewport_size().width < window.rem_size() * 72. {
            // At this width the accessory replaces the conversation, including
            // its composer. Returning to the conversation must reveal it first.
            self.panel = None;
            self.inspector = None;
            self.watch_agents(cx);
        }
        self.focus_page(window, cx);
        cx.notify();
    }

    pub(super) fn focus_page(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.page {
            MainPage::Conversation
                if self.info.is_none()
                    || (self.opening && !self.recovery.is_active())
                    || (window.viewport_size().width < window.rem_size() * 72.
                        && (self.panel.is_some() || self.inspector.is_some()))
                    || self
                        .question_form
                        .as_ref()
                        .is_some_and(|form| !form.read(cx).hidden()) =>
            {
                // Loading, an accessory, or a question form hides the ordinary input.
                self.focus.focus(window, cx);
            }
            MainPage::Conversation => self
                .input
                .update(cx, |input, cx| input.focus(window, cx)),
            MainPage::Files => {
                if let Some(files) = &self.files {
                    files.update(cx, |files, cx| files.focus(window, cx));
                } else {
                    self.focus.focus(window, cx);
                }
            }
            MainPage::Review => {
                if let Some(review) = &self.review {
                    review.update(cx, |review, cx| review.focus(window, cx));
                } else {
                    self.focus.focus(window, cx);
                }
            }
            MainPage::Tasks => self.focus.focus(window, cx),
        }
    }

    pub(super) fn repair_hidden_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.visible || window.has_active_sheet(cx) || window.has_active_dialog(cx) {
            return;
        }
        let hidden_composer = self.page != MainPage::Conversation
            || self.info.is_none()
            || (self.opening && !self.recovery.is_active())
            || (window.viewport_size().width < window.rem_size() * 72.
                && (self.panel.is_some() || self.inspector.is_some()))
            || self
                .question_form
                .as_ref()
                .is_some_and(|form| !form.read(cx).hidden());
        let stale_input = hidden_composer
            && self
                .input
                .focus_handle(cx)
                .is_focused(window);
        // A retained handle can outlive its rendered focus node. Bounds changes
        // catch the imminent compact layout; focus-lost catches font changes
        // and a Sheet restoring a handle that disappeared while it was open.
        if stale_input || !window.is_action_available(&WorkspaceCommands, cx) {
            self.focus_page(window, cx);
        }
    }

    pub(crate) fn window_command(
        &mut self,
        command: actions::WorkspaceCommand,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.execute(command, window, cx);
    }

    pub(crate) fn toggle_inspector(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let section = if self.inspector.is_some() {
            None
        } else {
            Some(inspector::Section::Execution)
        };
        self.set_inspector(section, window, cx);
    }

    pub(super) fn set_inspector(
        &mut self,
        section: Option<inspector::Section>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.inspector = section;
        self.watch_agents(cx);
        self.panel = None;
        if self.page == MainPage::Conversation {
            if self.inspector == Some(inspector::Section::Execution) {
                self.inspector_state
                    .focus
                    .focus(window, cx);
            } else if self.inspector.is_some()
                && window.viewport_size().width < window.rem_size() * 72.
            {
                // The compact inspector replaces the composer. Sheets opened
                // from it must restore a focus target that remains visible.
                self.focus.focus(window, cx);
            } else if self.inspector.is_none() {
                self.focus_page(window, cx);
            }
        }
        cx.notify();
    }

    pub(super) fn watch_agents(&mut self, cx: &mut Context<Self>) {
        let scope = (self.visible && self.inspector == Some(inspector::Section::Agents))
            .then(|| {
                self.info
                    .as_ref()
                    .map(|info| (info.id.clone(), info.workspace.clone()))
            })
            .flatten();
        if self.backend.is_some() {
            self.command(Command::WatchAgents(scope), cx);
        }
    }

    pub(crate) fn environment(&self) -> String {
        self.options
            .ssh
            .as_ref()
            .map(|host| format!("SSH · {host}"))
            .or_else(|| {
                self.options
                    .wsl
                    .as_ref()
                    .map(|host| format!("WSL · {host}"))
            })
            .unwrap_or_else(|| "本地".into())
    }

    pub(super) fn show_project_tasks(
        &mut self,
        create: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(info) = &self.info else {
            return;
        };
        if let Some(tasks) = &self.tasks {
            tasks.update(cx, |tasks, cx| tasks.activate(cx));
        } else {
            let owner = cx.weak_entity();
            let session = info.id.clone();
            let workspace = info.workspace.clone();
            self.tasks =
                Some(cx.new(|cx| tasks::Tasks::new(owner, session, workspace, window, cx)));
        }
        let changed = self.page != MainPage::Tasks;
        self.dictation
            .update(cx, |dictation, cx| dictation.cancel(window, cx));
        self.page = MainPage::Tasks;
        self.panel = None;
        self.inspector = None;
        self.watch_agents(cx);
        if create {
            self.tasks
                .as_ref()
                .unwrap()
                .update(cx, |tasks, cx| tasks.start_create(window, cx));
        } else if changed {
            self.focus_page(window, cx);
        }
        cx.notify();
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
