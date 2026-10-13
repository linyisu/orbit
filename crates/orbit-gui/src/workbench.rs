//! Window-level ownership of independent task workspaces. Changing selection
//! changes only the visible entity, never a task's transport or input state.
pub(crate) mod storage;
mod toolbar;
use crate::{
    backend::Options,
    workspace::{Sidebar, Workspace, WorkspaceEvent},
};
use gpui_kit::component::{
    WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use std::{collections::HashSet, sync::Arc};

struct OpenTask {
    view: Entity<Workspace>,
    _subscription: Subscription,
    was_busy: bool,
    _observer: Subscription,
    closed: bool,
}
pub(crate) struct Workbench {
    sidebar: Entity<Sidebar>,
    panes: Entity<gpui_kit::component::resizable::ResizableState>,
    _sidebar_observer: Subscription,
    _appearance_subscription: Subscription,
    runtime: Arc<tokio::runtime::Runtime>,
    tasks: Vec<OpenTask>,
    active: String,
    unread: HashSet<String>,
    task_scroll: ScrollHandle,
    task_reveal: bool,
    task_width: Option<Pixels>,
    settings: Option<Entity<crate::workspace::SettingsPage>>,
    origin: Options,
    storage: storage::Storage,
    restoring: bool,
    _storage_errors: Task<()>,
}
impl Workbench {
    pub(crate) fn new(
        options: Options,
        runtime: Arc<tokio::runtime::Runtime>,
        loaded: storage::Loaded,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let storage::Loaded {
            storage,
            mut saved,
            mut errors,
        } = loaded;
        if let Some(snapshot) = &mut saved {
            snapshot.deduplicate_tabs();
        }
        let sidebar = cx.new(Sidebar::new);
        let sidebar_observer = cx.observe(&sidebar, |_, _, cx| cx.notify());
        let mut workbench = Self {
            sidebar,
            panes: cx.new(|_| Default::default()),
            _sidebar_observer: sidebar_observer,
            _appearance_subscription: crate::preferences::Preferences::watch_appearance(window, cx),
            runtime,
            tasks: Vec::new(),
            active: String::new(),
            unread: HashSet::new(),
            task_scroll: ScrollHandle::new(),
            task_reveal: true,
            task_width: None,
            settings: None,
            origin: options.clone(),
            storage,
            restoring: true,
            _storage_errors: cx.spawn(async move |owner, cx| {
                while errors.changed().await.is_ok() {
                    let error = errors.borrow_and_update().clone();
                    if let Some(error) = error {
                        let _ = owner.update_in(cx, |_, window, cx| {
                            use gpui_kit::component::{WindowExt, notification::Notification};
                            window.push_notification(
                                Notification::error(error).title("草稿尚未保存"),
                                cx,
                            );
                        });
                    }
                }
            }),
        };
        let active = saved.as_ref().map(|saved| saved.active.clone());
        if let Some(saved) = saved {
            for task in saved.tasks {
                workbench.create(task.options.clone(), Some(task), window, cx);
            }
        }
        if workbench.tasks.is_empty()
            || options.workspace.is_some()
            || options.resume.is_some()
            || options.ssh.is_some()
            || options.wsl.is_some()
        {
            workbench.open(options, window, cx);
        } else if let Some(active) = active {
            workbench.activate(&active, window, cx);
        }
        workbench.restoring = false;
        // Component overlays are installed after the root view constructor.
        // Activate only once, after that installation, instead of exposing each
        // restored tab and attempting to dismiss its notifications in bootstrap.
        cx.defer_in(window, |workbench, window, cx| {
            let active = workbench.active.clone();
            workbench.activate(&active, window, cx);
        });
        workbench
    }
    fn open(&mut self, options: Options, window: &mut Window, cx: &mut Context<Self>) {
        let existing = self
            .tasks
            .iter()
            .find(|task| {
                let view = task.view.read(cx);
                if !view.options().same_host(&options) {
                    return false;
                }
                match options.resume.as_deref() {
                    Some(id) => view.can_resume(id, &options),
                    None => {
                        view.options().workspace == options.workspace && view.is_empty_draft(cx)
                    }
                }
            })
            .map(|task| task.view.read(cx).key().to_owned());
        if let Some(key) = existing {
            if let Some(index) = self.empty_task_to_replace(&options, cx)
                && self.tasks[index].view.read(cx).key() != key
            {
                self.discard_empty_task(index, window, cx);
            }
            if let Some(task) = self
                .tasks
                .iter()
                .find(|task| task.view.read(cx).key() == key)
            {
                task.view.update(cx, |view, cx| {
                    if options.resume.is_none() {
                        view.reopen_empty_draft(window, cx);
                    } else {
                        view.reconnect_if_needed(window, cx);
                    }
                });
            }
            self.activate(&key, window, cx);
            return;
        }
        self.create(options, None, window, cx);
    }

    fn empty_task_to_replace(&self, options: &Options, cx: &App) -> Option<usize> {
        // Opening a saved conversation fills the current placeholder. Ordinary
        // tab navigation and restoring a window must retain every saved tab.
        if self.restoring || options.resume.is_none() {
            return None;
        }
        self.tasks.iter().position(|task| {
            !task.closed
                && task.view.read(cx).key() == self.active
                && task.view.read(cx).is_empty_draft(cx)
        })
    }

    fn discard_empty_task(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        // Dismiss the placeholder's authentication before activating the target
        // host. Dropping its transport never cancels a server-side task.
        self.tasks[index]
            .view
            .update(cx, |view, cx| view.set_visible(false, window, cx));
        let task = self.tasks.remove(index);
        self.unread.remove(task.view.read(cx).key());
    }

    fn create(
        &mut self,
        options: Options,
        saved: Option<storage::SavedTask>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let replacement = saved
            .is_none()
            .then(|| self.empty_task_to_replace(&options, cx))
            .flatten();
        let runtime = self.runtime.clone();
        let catalog = self
            .tasks
            .iter()
            .find(|task| task.view.read(cx).options().same_host(&options))
            .map(|task| task.view.read(cx).catalog().to_vec());
        let view = cx.new(|cx| Workspace::new(options, runtime, self.sidebar.clone(), window, cx));
        if let Some(saved) = saved {
            view.update(cx, |view, cx| view.restore(saved, window, cx));
        }
        if let Some(catalog) = catalog {
            view.update(cx, |view, cx| view.set_catalog(catalog, cx));
        }
        let subscription = cx.subscribe_in(
            &view,
            window,
            |workbench, source, event, window, cx| match event {
                WorkspaceEvent::Open(options) => workbench.open((**options).clone(), window, cx),
                WorkspaceEvent::Activate(key) => workbench.activate(key, window, cx),
                WorkspaceEvent::CloseTask => {
                    let key = source.read(cx).key().to_owned();
                    workbench.close(&key, window, cx);
                }
                WorkspaceEvent::Settings(page) => {
                    window.close_all_dialogs(cx);
                    source.update(cx, |view, cx| view.set_visible(false, window, cx));
                    workbench.settings = Some(page.clone());
                    cx.notify();
                }
                WorkspaceEvent::CloseSettings => {
                    workbench.show_workspace(window, cx);
                }
                WorkspaceEvent::DraftChanged => workbench.persist(cx),
                WorkspaceEvent::SessionDeleted { id, options } => {
                    workbench.tasks.retain(|task| {
                        let view = task.view.read(cx);
                        !(view.options().same_host(options)
                            && view.summary().session.as_ref() == Some(id))
                    });
                    if !workbench
                        .tasks
                        .iter()
                        .any(|task| task.view.read(cx).key() == workbench.active)
                    {
                        if let Some(task) = workbench.tasks.iter().find(|task| !task.closed) {
                            let key = task.view.read(cx).key().to_owned();
                            workbench.activate(&key, window, cx);
                        } else {
                            let mut options = workbench.origin.clone();
                            options.resume = None;
                            options.recover_draft = false;
                            workbench.create(options, None, window, cx);
                        }
                    }
                    workbench.publish(cx);
                }
                WorkspaceEvent::Changed => {
                    let summary = source.read(cx).summary();
                    let finished = workbench
                        .tasks
                        .iter_mut()
                        .find(|task| task.view == *source)
                        .is_some_and(|task| {
                            let finished = task.was_busy && !summary.busy;
                            task.was_busy = summary.busy;
                            if task.closed && summary.attention {
                                task.closed = false;
                            }
                            finished
                        });
                    if summary.key != workbench.active && (summary.attention || finished) {
                        workbench.unread.insert(summary.key);
                    }
                    workbench.publish(cx);
                }
                WorkspaceEvent::Catalog(catalog) => {
                    let options = source.read(cx).options().clone();
                    for task in &workbench.tasks {
                        if task.view != *source && task.view.read(cx).options().same_host(&options)
                        {
                            task.view
                                .update(cx, |view, cx| view.set_catalog(catalog.clone(), cx));
                        }
                    }
                    workbench.publish(cx);
                }
            },
        );
        let observer = cx.observe(&view, |workbench, _, cx| workbench.sync_sidebar(cx));
        let key = view.read(cx).key().to_owned();
        let task = OpenTask {
            view,
            _subscription: subscription,
            was_busy: false,
            _observer: observer,
            closed: false,
        };
        if let Some(index) = replacement {
            self.discard_empty_task(index, window, cx);
            self.tasks.insert(index, task);
        } else {
            self.tasks.push(task);
        }
        self.activate(&key, window, cx);
    }
    fn activate(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !self
            .tasks
            .iter()
            .any(|task| task.view.read(cx).key() == key)
        {
            return;
        }
        let preserve_dialogs = self.active == key && self.settings.is_none();
        self.active = key.to_owned();
        self.settings = None;
        self.task_reveal = true;
        for task in &mut self.tasks {
            if task.view.read(cx).key() == key {
                task.closed = false;
            }
        }
        if self.restoring {
            return;
        }
        // Re-selecting the visible task must leave its pending authentication open.
        if !preserve_dialogs {
            window.close_all_dialogs(cx);
            window.close_sheet(cx);
        }
        self.unread.remove(key);
        // Suspend the previous task's overlays before the next task presents
        // its queued authentication. Tab insertion order must not decide which
        // environment's dialog remains on screen.
        for task in &self.tasks {
            if task.view.read(cx).key() != key {
                task.view
                    .update(cx, |view, cx| view.set_visible(false, window, cx));
            }
        }
        if let Some(task) = self
            .tasks
            .iter()
            .find(|task| task.view.read(cx).key() == key)
        {
            task.view
                .update(cx, |view, cx| view.set_visible(true, window, cx));
        }
        self.publish(cx);
    }
    fn close(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(task) = self
            .tasks
            .iter()
            .find(|task| task.view.read(cx).key() == key)
        else {
            return;
        };
        let saving = task.view.read(cx).files_saving(cx);
        let attention = task.view.read(cx).files_need_attention(cx);
        if saving || attention {
            self.confirm_file_close(key.to_owned(), saving, window, cx);
            return;
        }
        self.close_task(key, window, cx);
    }

    fn confirm_file_close(
        &mut self,
        key: String,
        saving: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let owner = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let keep_owner = owner.clone();
            let keep_key = key.clone();
            let discard_owner = owner.clone();
            let discard_key = key.clone();
            dialog
                .title(if saving { "文件正在保存" } else { "关闭未保存文件的任务？" })
                .child(if saving {
                    "保存结果尚未确认，任务会继续保留。请等待保存结束后再关闭。"
                } else {
                    "任务中有未保存的文件更改，或保存结果尚未确认。放弃并关闭会丢弃本任务保留的文件草稿，包括其他工作区的草稿。"
                })
                // Enter outside an explicitly focused button must not discard
                // file drafts through the Dialog's default Confirm action.
                .on_ok(|_, _, _| false)
                .footer(
                    h_flex().justify_end().gap_2()
                        .child(Button::new("keep-task-files").label("返回编辑")
                            .on_click(move |_, window, cx| {
                                window.close_dialog(cx);
                                let _ = keep_owner.update(cx, |workbench, cx| {
                                    let Some(view) = workbench.tasks.iter()
                                        .find(|task| task.view.read(cx).key() == keep_key)
                                        .map(|task| task.view.clone()) else { return };
                                    workbench.activate(&keep_key, window, cx);
                                    view.update(cx, |view, cx| view.focus_file_edits(window, cx));
                                });
                            }))
                        .when(!saving, |footer| footer.child(
                            Button::new("discard-task-files").label("放弃并关闭").danger()
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    let _ = discard_owner.update(cx, |workbench, cx| {
                                        let Some(task) = workbench.tasks.iter().find(|task| task.view.read(cx).key() == discard_key) else {
                                            return;
                                        };
                                        // A save can start after the confirmation
                                        // was shown. Its result must remain owned.
                                        if task.view.read(cx).files_saving(cx) {
                                            workbench.confirm_file_close(discard_key.clone(), true, window, cx);
                                        } else {
                                            task.view.update(cx, |view, cx| view.discard_file_edits(cx));
                                            workbench.close_task(&discard_key, window, cx);
                                        }
                                    });
                                })
                        )),
                )
        });
    }

    fn close_task(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self
            .tasks
            .iter()
            .position(|task| task.view.read(cx).key() == key)
        else {
            return;
        };
        let busy = self.tasks[ix].view.read(cx).summary().busy;
        let failed = self.tasks[ix].view.read(cx).connection_unestablished();
        let mut options = self.tasks[ix].view.read(cx).options().clone();
        // A failed restored tab may still carry a removed model in its launch
        // options. Closing it must leave a usable blank task with the default.
        options.model = None;
        options.resume = None;
        options.recover_draft = false;
        options.relink_on_resume = false;
        let was_active = self.active == key;
        let adjacent = toolbar::close_neighbor(&self.tasks, ix, |task| task.closed)
            .map(|ix| self.tasks[ix].view.read(cx).key().to_owned());
        if busy {
            // Closing a tab is navigation. Keep its transport and event
            // subscription alive until the user explicitly stops the run.
            self.tasks[ix].closed = true;
            self.tasks[ix]
                .view
                .update(cx, |view, cx| view.set_visible(false, window, cx));
        } else {
            self.tasks[ix]
                .view
                .update(cx, |view, cx| view.set_visible(false, window, cx));
            self.tasks.remove(ix);
        }
        self.unread.remove(key);
        if self.tasks.iter().all(|task| task.closed) {
            if failed {
                options = self.origin.clone();
                if options.ssh.is_some() || options.wsl.is_some() {
                    options.server = None;
                    options.database = None;
                }
                options.ssh = None;
                options.ssh_user = None;
                options.ssh_port = None;
                options.identity = None;
                options.wsl = None;
                options.wsl_user = None;
                options.workspace = None;
                options.model = None;
                options.resume = None;
                options.recover_draft = false;
                options.relink_on_resume = false;
            }
            self.open(options, window, cx);
        } else if was_active {
            let next = adjacent.expect("an adjacent open task");
            self.activate(&next, window, cx);
        } else {
            self.publish(cx);
        }
    }
    fn publish(&mut self, cx: &mut Context<Self>) {
        let summaries: Vec<_> = self
            .tasks
            .iter()
            .filter(|task| !task.closed)
            .map(|task| {
                let mut summary = task.view.read(cx).summary();
                summary.unread = self.unread.contains(&summary.key);
                summary
            })
            .collect();
        for task in &self.tasks {
            // Session IDs are scoped to a service/data source, just like the catalog.
            let options = task.view.read(cx).options();
            let navigation_tasks = self
                .tasks
                .iter()
                .filter(|other| other.view.read(cx).options().same_host(options))
                .map(|other| {
                    let mut summary = other.view.read(cx).summary();
                    summary.unread = self.unread.contains(&summary.key);
                    summary
                })
                .collect();
            task.view.update(cx, |view, cx| {
                view.set_open_tasks(summaries.clone(), navigation_tasks, cx)
            });
        }
        self.sync_sidebar(cx);
        self.task_reveal = true;
        cx.notify();
        self.persist(cx);
    }
    fn sync_sidebar(&self, cx: &mut Context<Self>) {
        if let Some(task) = self
            .tasks
            .iter()
            .find(|task| task.view.read(cx).key() == self.active)
        {
            self.sidebar
                .update(cx, |sidebar, cx| sidebar.bind(&task.view, cx));
        }
    }
    fn persist(&self, cx: &App) {
        if self.restoring {
            return;
        }
        self.storage.save(storage::Snapshot::new(
            self.origin.clone(),
            self.active.clone(),
            self.tasks
                .iter()
                .filter(|task| !task.closed || task.view.read(cx).has_draft(cx))
                .map(|task| task.view.read(cx).saved(cx))
                .collect(),
        ));
    }
    fn show_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.take().is_some() {
            window.close_all_dialogs(cx);
            self.with_active(window, cx, |view, window, cx| {
                view.set_visible(true, window, cx)
            });
            cx.notify();
        }
    }
}
impl Render for Workbench {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui_kit::component::{
            ActiveTheme,
            resizable::{h_resizable, resizable_panel},
            v_flex,
        };
        let body = if let Some(settings) = &self.settings {
            div().size_full().child(settings.clone()).into_any_element()
        } else {
            div()
                .size_full()
                .children(
                    self.tasks
                        .iter()
                        .find(|task| task.view.read(cx).key() == self.active)
                        .map(|task| task.view.clone()),
                )
                .into_any_element()
        };
        let body = if self.settings.is_none() && self.sidebar.read(cx).visible() {
            h_resizable("window-panes")
                .with_state(&self.panes)
                .on_resize(|state, window, cx| {
                    if let Some(width) = state.read(cx).sizes().first().copied() {
                        crate::preferences::Preferences::set_sidebar(width / window.rem_size(), cx);
                    }
                })
                .child(
                    resizable_panel()
                        .size(window.rem_size() * crate::preferences::Preferences::sidebar(cx))
                        .size_range(window.rem_size() * 12. ..window.rem_size() * 24.)
                        .child(self.sidebar.clone()),
                )
                .child(resizable_panel().child(body))
                .into_any_element()
        } else {
            body
        };
        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .font_family(cx.theme().font_family.clone())
            .key_context("Orbit")
            .on_action(
                cx.listener(|view, _: &crate::workspace::CloseTask, window, cx| {
                    if view.settings.is_some() {
                        view.show_workspace(window, cx);
                    } else {
                        let key = view.active.clone();
                        view.close(&key, window, cx);
                    }
                }),
            )
            .on_action(
                cx.listener(|view, _: &crate::workspace::StopTask, window, cx| {
                    view.with_active(window, cx, |view, _, cx| view.stop_task(cx));
                }),
            )
            .on_action(
                cx.listener(|view, _: &crate::workspace::ToggleSidebar, _, cx| {
                    view.sidebar.update(cx, |sidebar, cx| sidebar.toggle(cx))
                }),
            )
            .on_action(
                cx.listener(|view, _: &crate::workspace::NewConversation, window, cx| {
                    view.with_active(window, cx, |view, _, cx| view.new_task(cx))
                }),
            )
            .on_action(cx.listener(
                |view, _: &crate::workspace::AppearanceSettings, window, cx| {
                    view.with_active(window, cx, |view, window, cx| {
                        view.window_settings(window, cx)
                    })
                },
            ))
            .on_action(cx.listener(
                |view, _: &crate::workspace::WorkspaceCommands, window, cx| {
                    view.show_workspace(window, cx);
                    view.with_active(window, cx, |view, window, cx| {
                        view.window_commands(window, cx)
                    })
                },
            ))
            .on_action(
                cx.listener(|view, _: &crate::workspace::SearchSessions, window, cx| {
                    view.with_active(window, cx, |view, window, cx| {
                        view.window_search(window, cx)
                    })
                }),
            )
            .on_action(
                cx.listener(|view, _: &crate::workspace::OpenProject, window, cx| {
                    view.with_active(window, cx, |view, window, cx| {
                        view.window_command(
                            crate::workspace::WorkspaceCommand::OpenProject,
                            window,
                            cx,
                        )
                    })
                }),
            )
            .on_action(
                cx.listener(|view, _: &crate::workspace::ShowChanges, window, cx| {
                    view.show_workspace(window, cx);
                    view.with_active(window, cx, |view, window, cx| {
                        view.window_command(crate::workspace::WorkspaceCommand::Changes, window, cx)
                    })
                }),
            )
            .on_action(
                cx.listener(|view, _: &crate::workspace::FocusComposer, window, cx| {
                    view.show_workspace(window, cx);
                    view.with_active(window, cx, |view, window, cx| {
                        view.focus_composer(window, cx)
                    });
                    cx.notify();
                }),
            )
            .on_action(
                cx.listener(|view, _: &crate::workspace::ToggleInspector, window, cx| {
                    view.show_workspace(window, cx);
                    view.with_active(window, cx, |view, window, cx| {
                        view.toggle_inspector(window, cx)
                    });
                    cx.notify();
                }),
            )
            .on_action(
                cx.listener(|view, _: &crate::workspace::NextTask, window, cx| {
                    view.adjacent(true, window, cx)
                }),
            )
            .on_action(
                cx.listener(|view, _: &crate::workspace::PreviousTask, window, cx| {
                    view.adjacent(false, window, cx)
                }),
            )
            .child(self.toolbar(window, cx))
            .child(div().flex_1().min_h_0().child(body))
    }
}
