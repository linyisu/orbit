//! A single window-owned sidebar. Tabs supply data; they never own navigation state.
use super::navigation::{project_items, scrolling_title};
use super::*;
use gpui_kit::component::{list::ListItem, spinner::Spinner, tooltip::Tooltip};

pub(crate) struct Sidebar {
    owner: Option<WeakEntity<Workspace>>,
    visible: bool,
    hosts: Vec<HostNavigation>,
    host: Option<usize>,
    info: Option<SessionInfo>,
    navigation_tasks: Vec<WorkspaceSummary>,
    hovered_title: Option<(SharedString, Instant)>,
}
struct HostNavigation {
    options: Options,
    sessions: Vec<SessionEntry>,
    groups: Vec<TreeItem>,
    tree: Entity<TreeState>,
}
impl Sidebar {
    pub(crate) fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            owner: None,
            visible: true,
            hosts: vec![],
            host: None,
            info: None,
            navigation_tasks: vec![],
            hovered_title: None,
        }
    }
    pub(crate) fn bind(&mut self, owner: &Entity<Workspace>, cx: &mut Context<Self>) {
        let view = owner.read(cx);
        let selection_changed = self.owner.as_ref().and_then(WeakEntity::upgrade).as_ref()
            != Some(owner)
            || self.info.as_ref().map(|info| &info.id) != view.info.as_ref().map(|info| &info.id);
        let options = view.options.clone();
        let sessions = view.sessions.clone();
        let location = view.location.clone();
        self.info = view.info.clone();
        self.navigation_tasks = view.navigation_tasks.clone();
        let host = self
            .hosts
            .iter()
            .position(|host| host.options.same_host(&options));
        let host = host.unwrap_or_else(|| {
            self.hosts.push(HostNavigation {
                options,
                sessions: vec![],
                groups: vec![],
                tree: cx.new(|cx| TreeState::new(cx)),
            });
            self.hosts.len() - 1
        });
        if self.host != Some(host) {
            self.host = Some(host);
            self.hovered_title = None;
        }
        let host = &mut self.hosts[host];
        let catalog_changed = host.sessions != sessions;
        if catalog_changed {
            let mut groups = project_items(&sessions, &host.groups);
            for group in &mut groups {
                if group.id.as_ref() == format!("project:{location}")
                    && !host.groups.iter().any(|old| old.id == group.id)
                {
                    *group = group.clone().expanded(true);
                }
            }
            host.groups = groups;
            host.sessions = sessions;
            host.tree
                .update(cx, |tree, cx| tree.set_items(host.groups.clone(), cx));
        }
        let selected = self
            .info
            .as_ref()
            .map(|info| SharedString::from(format!("session:{}", info.id)));
        if selection_changed || catalog_changed {
            host.tree.update(cx, |tree, cx| {
                let ix = selected.as_ref().and_then(|id| tree.index_of(id));
                if tree.selected_index() != ix {
                    tree.set_selected_index(ix, cx);
                }
            });
        }
        self.owner = Some(owner.downgrade());
        cx.notify();
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
    fn project_tree(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.weak_entity();
        let hovered = self.hovered_title.clone();
        let tasks = self.navigation_tasks.clone();
        let active = self
            .info
            .as_ref()
            .map(|info| format!("session:{}", info.id));
        div()
            .id("project-conversations")
            .size_full()
            .capture_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "enter" {
                    let id = view
                        .navigation()
                        .read(cx)
                        .selected_item()
                        .map(|item| item.id.clone());
                    if let Some(id) =
                        id.and_then(|id| id.strip_prefix("session:").map(str::to_owned))
                    {
                        view.select_session(&id, window, cx);
                        cx.stop_propagation();
                    }
                }
            }))
            .child(Tree::new(self.navigation(), move |_, entry, _, _, cx| {
                let id = entry.item().id.clone();
                let owner = owner.clone();
                let hover_owner = owner.clone();
                let hover_id = id.clone();
                let folder = entry.is_folder();
                let expanded = entry.is_expanded();
                let title: SharedString = entry
                    .item()
                    .label
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .into();
                let started = hovered
                    .as_ref()
                    .filter(|(key, _)| key == &id)
                    .map(|(_, time)| *time);
                let task = id.strip_prefix("session:").and_then(|session| {
                    tasks
                        .iter()
                        .find(|task| task.session.as_deref() == Some(session))
                });
                let status = task.map(|task| task.status);
                ListItem::new(id.clone())
                    .accessibility_label(entry.item().label.clone())
                    .h_8()
                    .overflow_hidden()
                    .confirmed(active.as_deref() == Some(id.as_ref()))
                    .on_hover(move |hovering, _, cx| {
                        let _ = hover_owner.update(cx, |view, cx| {
                            if *hovering {
                                view.hovered_title = Some((hover_id.clone(), Instant::now()));
                            } else if view
                                .hovered_title
                                .as_ref()
                                .is_some_and(|(id, _)| id == &hover_id)
                            {
                                view.hovered_title = None;
                            }
                            cx.notify();
                        });
                    })
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .gap_2()
                            .child(div().w_4().flex_shrink_0().when(folder, |slot| {
                                slot.child(
                                    Icon::new(if expanded {
                                        IconName::FolderOpen
                                    } else {
                                        IconName::Folder
                                    })
                                    .size_4(),
                                )
                            }))
                            .child(
                                div()
                                    .w_0()
                                    .flex_1()
                                    .min_w_0()
                                    .h_6()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .map(|label| match started.filter(|_| !cx.reduce_motion()) {
                                        Some(started) => {
                                            label.child(scrolling_title(title, started))
                                        }
                                        None => label.child(title),
                                    }),
                            )
                            .child(
                                div()
                                    .id((ElementId::from(id.clone()), "activity"))
                                    .w_4()
                                    .h_4()
                                    .flex_shrink_0()
                                    .when_some(task, |slot, task| {
                                        slot.when(task.attention, |slot| {
                                            slot.child(
                                                Icon::new(IconName::Info)
                                                    .size_4()
                                                    .text_color(cx.theme().muted_foreground),
                                            )
                                        })
                                        .when(task.loading && !task.attention, |slot| {
                                            slot.child(Spinner::new().small())
                                        })
                                        .when_some(
                                            status,
                                            |slot, status| {
                                                slot.tooltip(move |window, cx| {
                                                    Tooltip::new(status).build(window, cx)
                                                })
                                            },
                                        )
                                    }),
                            ),
                    )
                    .when(folder, |row| {
                        let path = id.strip_prefix("project:").unwrap_or(&id).to_owned();
                        row.tooltip(move |window, cx| Tooltip::new(path.clone()).build(window, cx))
                    })
                    .on_click(move |_, window, cx| {
                        if let Some(session) = id.strip_prefix("session:") {
                            let _ = owner
                                .update(cx, |view, cx| view.select_session(session, window, cx));
                        }
                    })
            }))
    }
}
impl Render for Sidebar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(owner) = self.owner.as_ref().and_then(|owner| owner.upgrade()) else {
            return div().into_any_element();
        };
        let active = owner.read(cx);
        v_flex()
            .key_context("Orbit")
            .on_action(cx.listener(|view, _: &ToggleSidebar, window, cx| {
                view.toggle(cx);
                view.with_owner(
                    cx,
                    |view, window, cx| view.input.update(cx, |input, cx| input.focus(window, cx)),
                    window,
                );
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
                view.with_owner(cx, |view, _, cx| view.open_project(cx), window)
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
                    |view, window, cx| view.input.update(cx, |input, cx| input.focus(window, cx)),
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
                    |view, _, cx| {
                        view.inspector = if view.inspector.is_some() {
                            None
                        } else {
                            Some(super::inspector::Section::Execution)
                        };
                        cx.notify();
                    },
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
                    .child(
                        Button::new("environment-switcher")
                            .ghost()
                            .small()
                            .w_full()
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .child(
                                        Icon::new(
                                            if active.options.ssh.is_some()
                                                || active.options.wsl.is_some()
                                            {
                                                IconName::Monitor
                                            } else {
                                                IconName::Laptop
                                            },
                                        )
                                        .size_4(),
                                    )
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .text_ellipsis()
                                            .child(active.environment()),
                                    )
                                    .child(Icon::new(IconName::ChevronDown).size_3()),
                            )
                            .accessibility_label("切换连接环境")
                            .dropdown_menu({
                                let owner = cx.weak_entity();
                                let hosts: Vec<_> = self
                                    .hosts
                                    .iter()
                                    .map(|host| {
                                        let label = host
                                            .options
                                            .ssh
                                            .as_ref()
                                            .map(|host| format!("SSH · {host}"))
                                            .or_else(|| {
                                                host.options
                                                    .wsl
                                                    .as_ref()
                                                    .map(|host| format!("WSL · {host}"))
                                            })
                                            .unwrap_or_else(|| "本地".into());
                                        (label, host.options.clone())
                                    })
                                    .collect();
                                move |menu, _, _| {
                                    let mut menu = menu;
                                    for (label, options) in &hosts {
                                        let owner = owner.clone();
                                        let mut options = options.clone();
                                        options.resume = None;
                                        options.recover_draft = false;
                                        options.relink_on_resume = false;
                                        menu =
                                            menu.item(PopupMenuItem::new(label.clone()).on_click(
                                                move |_, window, cx| {
                                                    let _ = owner.update(cx, |view, cx| {
                                                        view.with_owner(
                                                            cx,
                                                            |_, _, cx| {
                                                                cx.emit(WorkspaceEvent::Open(
                                                                    Box::new(options.clone()),
                                                                ))
                                                            },
                                                            window,
                                                        )
                                                    });
                                                },
                                            ));
                                    }
                                    let owner = owner.clone();
                                    menu.separator().item(
                                        PopupMenuItem::new("连接远程项目…").on_click(
                                            move |_, window, cx| {
                                                let _ = owner.update(cx, |view, cx| {
                                                    view.with_owner(
                                                        cx,
                                                        |view, window, cx| {
                                                            view.remote_connection_dialog(
                                                                window, cx,
                                                            )
                                                        },
                                                        window,
                                                    )
                                                });
                                            },
                                        ),
                                    )
                                }
                            }),
                    )
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
                            .tooltip("Ctrl+N")
                            .disabled(active.opening || !active.connected)
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.with_owner(cx, |view, _, cx| view.open(None, cx), window)
                            })),
                    )
                    .child(
                        Button::new("open-project")
                            .ghost()
                            .small()
                            .w_full()
                            .accessibility_label("打开项目")
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .child(Icon::new(IconName::Folder).size_4())
                                    .child("打开项目…"),
                            )
                            .tooltip("Ctrl+O")
                            .disabled(active.choosing_project || active.pending.is_some())
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.with_owner(cx, |view, _, cx| view.open_project(cx), window)
                            })),
                    )
                    .child(
                        Button::new("connect-remote")
                            .ghost()
                            .small()
                            .w_full()
                            .accessibility_label("连接远程项目")
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .child(Icon::new(IconName::Monitor).size_4())
                                    .child("连接远程项目…"),
                            )
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.with_owner(
                                    cx,
                                    |view, window, cx| view.remote_connection_dialog(window, cx),
                                    window,
                                )
                            })),
                    )
                    .child(
                        Button::new("find-task")
                            .ghost()
                            .small()
                            .w_full()
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .child(Icon::new(IconName::Search).size_4())
                                    .child("搜索任务"),
                            )
                            .tooltip("搜索任务 · Ctrl+P")
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.with_owner(
                                    cx,
                                    |view, window, cx| view.search_sessions(window, cx),
                                    window,
                                )
                            })),
                    )
                    .child(
                        Button::new("project-tasks")
                            .ghost()
                            .small()
                            .w_full()
                            .accessibility_label("项目任务")
                            .disabled(active.info.is_none())
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .child(Icon::new(IconName::ListTodo).size_4())
                                    .child("项目任务"),
                            )
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.with_owner(
                                    cx,
                                    |view, window, cx| view.show_project_tasks(false, window, cx),
                                    window,
                                )
                            })),
                    )
                    .child(
                        Button::new("plugin-marketplace")
                            .ghost()
                            .small()
                            .w_full()
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .child(Icon::new(IconName::Puzzle).size_4())
                                    .child("插件市场"),
                            )
                            .accessibility_label("插件市场")
                            .on_click(cx.listener(|view, _, window, cx| {
                                view.with_owner(
                                    cx,
                                    |view, window, cx| {
                                        view.execute(
                                            super::actions::WorkspaceCommand::Plugins,
                                            window,
                                            cx,
                                        )
                                    },
                                    window,
                                );
                            })),
                    ),
            )
            .child(
                div()
                    .px_4()
                    .py_2()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("项目"),
            )
            .child(div().flex_1().min_h_0().child(self.project_tree(cx)))
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
                            .tooltip("设置 · Ctrl+,")
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
