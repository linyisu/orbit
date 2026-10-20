//! A single window-owned sidebar. Tabs supply data; they never own navigation state.
mod environment;
mod organization;
#[path = "session_controls.rs"]
mod session_controls;
mod tree;
use super::*;
use crate::preferences::{Placement, Preferences, Projects};
use gpui_kit::component::{
    menu::{ContextMenuExt, PopupMenu},
    spinner::Spinner,
};

pub(crate) struct Sidebar {
    owner: Option<WeakEntity<Workspace>>,
    visible: bool,
    hosts: Vec<HostNavigation>,
    host: Option<usize>,
    last_local: Option<usize>,
    last_remote: Option<usize>,
    info: Option<SessionInfo>,
    navigation_tasks: Vec<WorkspaceSummary>,
    hovered_title: Option<(SharedString, Instant)>,
    _preferences: Subscription,
}
struct HostNavigation {
    options: Options,
    sessions: Vec<SessionEntry>,
    groups: Vec<TreeItem>,
    tree: Entity<TreeState>,
    placements: Vec<Placement>,
    projects: Projects,
}

impl HostNavigation {
    fn rebuild(&mut self, cx: &mut App) {
        let selected = self
            .tree
            .read(cx)
            .selected_item()
            .cloned();
        self.groups = organization::grouped_items(
            &self.sessions,
            &self.placements,
            &self.projects,
            &self.groups,
        );
        self.tree.update(cx, |tree, cx| {
            tree.set_items(self.groups.clone(), cx);
            // Moving a selected session into a collapsed category must reveal
            // its destination without expanding unrelated project folders.
            tree.set_selected_item(selected.as_ref(), cx);
        });
    }
}

impl Sidebar {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let preferences = cx.observe_global::<Preferences>(|sidebar: &mut Self, cx| {
            for host in &mut sidebar.hosts {
                let placements = host
                    .sessions
                    .iter()
                    .map(|entry| Preferences::session_placement(&host.options, &entry.id, cx))
                    .collect::<Vec<_>>();
                let projects = Preferences::projects(&host.options, cx);
                if host.placements != placements || host.projects != projects {
                    host.placements = placements;
                    host.projects = projects;
                    host.rebuild(cx);
                }
            }
            cx.notify();
        });
        Self {
            owner: None,
            visible: true,
            hosts: vec![],
            host: None,
            last_local: None,
            last_remote: None,
            info: None,
            navigation_tasks: vec![],
            hovered_title: None,
            _preferences: preferences,
        }
    }

    pub(crate) fn bind(
        &mut self,
        owner: &Entity<Workspace>,
        targets: Vec<Options>,
        cx: &mut Context<Self>,
    ) {
        let view = owner.read(cx);
        let selection_changed = self
            .owner
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .as_ref()
            != Some(owner)
            || self.info.as_ref().map(|info| &info.id) != view.info.as_ref().map(|info| &info.id);
        let options = view.options.clone();
        let sessions = view.sessions.clone();
        let location = view.location.clone();
        self.info = view.info.clone();
        self.navigation_tasks = view.navigation_tasks.clone();
        for target in targets {
            self.remember_host(target, cx);
        }
        let host = self.remember_host(options.clone(), cx);
        if self.host != Some(host) {
            self.host = Some(host);
            self.hovered_title = None;
        }
        if options.ssh.is_some() || options.wsl.is_some() {
            self.last_remote = Some(host);
        } else {
            self.last_local = Some(host);
        }
        let host = &mut self.hosts[host];
        host.options = options;
        let previous_selected = host
            .tree
            .read(cx)
            .selected_item()
            .map(|item| item.id.clone());
        let catalog_changed = host.sessions != sessions;
        if catalog_changed {
            Preferences::migrate_project_pins(
                &host.options,
                &sessions
                    .iter()
                    .map(|entry| (entry.id.to_string(), entry.workspace.to_string()))
                    .collect::<Vec<_>>(),
                cx,
            );
            host.projects = Preferences::projects(&host.options, cx);
            host.placements = sessions
                .iter()
                .map(|entry| Preferences::session_placement(&host.options, &entry.id, cx))
                .collect();
            let mut groups = organization::grouped_items(
                &sessions,
                &host.placements,
                &host.projects,
                &host.groups,
            );
            for group in groups
                .iter_mut()
                .flat_map(|section| &mut section.children)
            {
                if group.id.as_ref() == format!("project:{location}")
                    && !host
                        .groups
                        .iter()
                        .flat_map(|section| &section.children)
                        .any(|old| old.id == group.id)
                {
                    *group = group.clone().expanded(true);
                }
            }
            host.groups = groups;
            host.sessions = sessions;
            host.tree
                .update(cx, |tree, cx| tree.set_items(host.groups.clone(), cx));
        }
        let active = self
            .info
            .as_ref()
            .map(|info| SharedString::from(format!("session:{}", info.id)));
        let selected = if selection_changed {
            active
        } else {
            previous_selected.or(active)
        };
        if selection_changed || catalog_changed {
            host.tree.update(cx, |tree, cx| {
                let ix = selected
                    .as_ref()
                    .and_then(|id| tree.index_of(id));
                if tree.selected_index() != ix {
                    tree.set_selected_index(ix, cx);
                }
            });
        }
        self.owner = Some(owner.downgrade());
        cx.notify();
    }

    /// Retain navigation state for restored tabs before they are first activated.
    fn remember_host(&mut self, options: Options, cx: &mut Context<Self>) -> usize {
        if let Some(ix) = self
            .hosts
            .iter()
            .position(|host| host.options.same_host(&options))
        {
            return ix;
        }
        self.hosts.push(HostNavigation {
            options,
            sessions: vec![],
            groups: vec![],
            tree: cx.new(|cx| TreeState::new(cx)),
            placements: vec![],
            projects: Projects::default(),
        });
        let ix = self.hosts.len() - 1;
        if self.hosts[ix].is_remote() {
            self.last_remote.get_or_insert(ix);
        } else {
            self.last_local.get_or_insert(ix);
        }
        ix
    }

    pub(crate) fn visible(&self) -> bool {
        self.visible
    }

    fn navigation(&self) -> &Entity<TreeState> {
        &self.hosts[self.host.expect("bound sidebar")].tree
    }

    fn with_owner(
        &mut self,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>),
        window: &mut Window,
    ) {
        if let Some(owner) = &self.owner {
            let _ = owner.update(cx, |view, cx| f(view, window, cx));
        }
    }

    fn select_session(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.with_owner(
            cx,
            |view, window, cx| view.select_session(id, window, cx),
            window,
        );
    }

    pub(crate) fn toggle(&mut self, cx: &mut Context<Self>) {
        self.visible = !self.visible;
        cx.notify();
    }
}

impl Render for Sidebar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(owner) = self
            .owner
            .as_ref()
            .and_then(|owner| owner.upgrade())
        else {
            return div().into_any_element();
        };
        let active = owner.read(cx);
        let new_task_disabled = active.opening || !active.connected;
        v_flex()
            .key_context("Orbit")
            .on_action(cx.listener(|view, _: &ToggleSidebar, window, cx| {
                view.toggle(cx);
                view.with_owner(cx, |view, window, cx| view.focus_page(window, cx), window);
            }))
            .on_action(cx.listener(|view, _: &NextTask, window, cx| {
                view.with_owner(cx, |view, _, cx| view.adjacent_task(true, cx), window)
            }))
            .on_action(cx.listener(|view, _: &PreviousTask, window, cx| {
                view.with_owner(cx, |view, _, cx| view.adjacent_task(false, cx), window)
            }))
            .on_action(cx.listener(|view, _: &NewConversation, window, cx| {
                view.with_owner(cx, |view, _, cx| view.open(None, cx), window)
            }))
            .on_action(cx.listener(|view, _: &OpenProject, window, cx| {
                view.with_owner(cx, |view, window, cx| view.open_project(window, cx), window)
            }))
            .on_action(
                cx.listener(|view, action: &AppearanceSettings, window, cx| {
                    view.with_owner(
                        cx,
                        |view, window, cx| view.appearance(action, window, cx),
                        window,
                    )
                }),
            )
            .on_action(cx.listener(|view, _: &WorkspaceCommands, window, cx| {
                view.with_owner(cx, |view, window, cx| view.commands(window, cx), window)
            }))
            .on_action(cx.listener(|view, _: &SearchSessions, window, cx| {
                view.with_owner(
                    cx,
                    |view, window, cx| view.search_sessions(window, cx),
                    window,
                )
            }))
            .on_action(cx.listener(|view, _: &FocusComposer, window, cx| {
                view.with_owner(
                    cx,
                    |view, window, cx| view.focus_composer(window, cx),
                    window,
                )
            }))
            .on_action(cx.listener(|view, action: &SendMessage, window, cx| {
                view.with_owner(cx, |view, window, cx| view.send(action, window, cx), window)
            }))
            .on_action(cx.listener(|view, _: &ShowChanges, window, cx| {
                view.with_owner(
                    cx,
                    |view, window, cx| {
                        view.execute(super::actions::WorkspaceCommand::Changes, window, cx)
                    },
                    window,
                )
            }))
            .on_action(cx.listener(|view, _: &ToggleInspector, window, cx| {
                view.with_owner(
                    cx,
                    |view, window, cx| view.toggle_inspector(window, cx),
                    window,
                )
            }))
            .size_full()
            .bg(cx.theme().sidebar)
            .text_color(cx.theme().sidebar_foreground)
            .child(
                v_flex()
                    .p_3()
                    .gap_1()
                    .child(self.environment_switcher(cx))
                    .child(
                        Button::new("new-session")
                            .ghost()
                            .small()
                            .w_full()
                            .accessibility_label("新任务")
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .child(Icon::new(IconName::Plus).size_4())
                                    .child("新任务"),
                            )
                            .tooltip(crate::keymap::tooltip("新任务", "new_conversation", cx))
                            .disabled(new_task_disabled)
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.with_owner(cx, |view, _, cx| view.open(None, cx), window)
                            })),
                    )
                    .child(
                        Button::new("find-task")
                            .accessibility_label("搜索任务")
                            .ghost()
                            .small()
                            .w_full()
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .child(Icon::new(IconName::Search).size_4())
                                    .child("搜索"),
                            )
                            .tooltip(crate::keymap::tooltip("搜索任务", "search_sessions", cx))
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.with_owner(
                                    cx,
                                    |view, window, cx| view.search_sessions(window, cx),
                                    window,
                                )
                            })),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(self.project_tree(cx)),
            )
            .when(
                matches!(
                    Preferences::save_status(cx),
                    crate::keymap::SaveStatus::Failed { .. }
                ),
                |sidebar| {
                    sidebar.child(
                        Button::new("retry-sidebar-settings")
                            .ghost()
                            .small()
                            .icon(IconName::Info)
                            .label("设置未保存 · 重试")
                            .tooltip("置顶、分类或其他设置尚未写入磁盘，点击重试保存")
                            .on_click(|_, _, cx| {
                                Preferences::retry_save(cx);
                            }),
                    )
                },
            )
            .child(
                h_flex()
                    .p_2()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("appearance")
                            .ghost()
                            .w_full()
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .child(Icon::new(IconName::Settings).small())
                                    .child("设置"),
                            )
                            .accessibility_label("设置")
                            .tooltip(crate::keymap::tooltip("设置", "appearance_settings", cx))
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.with_owner(
                                    cx,
                                    |view, window, cx| {
                                        view.appearance(&AppearanceSettings, window, cx)
                                    },
                                    window,
                                )
                            })),
                    ),
            )
            .into_any_element()
    }
}
