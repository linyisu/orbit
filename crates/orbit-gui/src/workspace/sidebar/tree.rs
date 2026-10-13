//! Native tree presentation and project drag targets.
use super::super::navigation::scrolling_title;
use super::*;
use gpui_kit::component::{list::ListItem, tooltip::Tooltip};

impl Sidebar {
    fn toggle_project(&mut self, id: &SharedString, window: &mut Window, cx: &mut Context<Self>) {
        let Some(host) = self.host.map(|ix| &mut self.hosts[ix]) else {
            return;
        };
        if let Some(project) = host
            .groups
            .iter()
            .flat_map(|section| &section.children)
            .find(|item| item.id == *id)
        {
            project.clone().expanded(!project.is_expanded());
            host.tree.update(cx, |tree, cx| {
                tree.set_items(host.groups.clone(), cx);
                tree.set_selected_item(Some(project), cx);
                tree.focus(window, cx);
            });
            cx.notify();
        }
    }
    pub(super) fn project_tree(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.weak_entity();
        let options = self.hosts[self.host.expect("bound sidebar")]
            .options
            .clone();
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
                // Nested row menus own Enter when their trigger or popup has
                // focus. Only the tree's own keyboard path opens a session.
                if !window
                    .context_stack()
                    .last()
                    .is_some_and(|context| context.contains(gpui_kit::base::tree_key_context()))
                {
                    return;
                }
                if matches!(event.keystroke.key.as_str(), "enter" | "f2" | "delete") {
                    let id = view
                        .navigation()
                        .read(cx)
                        .selected_item()
                        .map(|item| item.id.clone());
                    if let Some(id) =
                        id.and_then(|id| id.strip_prefix("session:").map(str::to_owned))
                    {
                        match event.keystroke.key.as_str() {
                            "f2" => view.with_owner(
                                cx,
                                |view, window, cx| view.rename_session(&id, window, cx),
                                window,
                            ),
                            "delete" => view.with_owner(
                                cx,
                                |view, window, cx| view.delete_session(&id, window, cx),
                                window,
                            ),
                            _ => view.select_session(&id, window, cx),
                        }
                        cx.stop_propagation();
                    }
                }
            }))
            .child(Tree::new(self.navigation(), move |ix, entry, _, _, cx| {
                let id = entry.item().id.clone();
                let owner = owner.clone();
                let menu_owner = owner.clone();
                let menu_id = id.clone();
                let button_owner = owner.clone();
                let button_id = id.clone();
                let hover_owner = owner.clone();
                let hover_id = id.clone();
                let folder = entry.is_folder();
                let expanded = entry.is_expanded();
                let project = id.starts_with("project:");
                if matches!(id.as_ref(), "pinned" | "projects") {
                    return ListItem::new(id).h_8().child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(div().flex_1().child(entry.item().label.clone()))
                            .child(
                                Icon::new(if expanded {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .size_3(),
                            ),
                    );
                }
                let drag = organization::project_drag(
                    owner.clone(),
                    options.clone(),
                    id.strip_prefix("project:").unwrap_or_default().to_owned(),
                    entry.item().label.clone(),
                    ix,
                );
                let drag_target = drag.clone();
                let drop_owner = owner.clone();
                let drop_id = id.clone();
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
                    .when(project, |row| {
                        // Kit's tree expands folders on mouse-down. Defer this
                        // project click until release so its native drag threshold
                        // can distinguish a click from a row reorder. A drag
                        // consumes the click and leaves expansion untouched.
                        row.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                            .on_drag(drag, |drag, _, _, cx| cx.new(|_| drag.clone()))
                            .cursor_grab()
                    })
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
                            .id((ElementId::from(id.clone()), "content"))
                            .w_full()
                            .min_w_0()
                            .gap_2()
                            .pl(rems(entry.depth().saturating_sub(1) as f32 * 0.75))
                            .child(
                                div()
                                    .id((ElementId::from(id.clone()), "project-icon"))
                                    .w_4()
                                    .flex_shrink_0()
                                    .when(folder, |slot| {
                                        slot.child(
                                            Icon::new(if id.as_ref() == "pinned" {
                                                IconName::Pin
                                            } else if id.starts_with("category:") {
                                                IconName::Tags
                                            } else if expanded {
                                                IconName::FolderOpen
                                            } else {
                                                IconName::Folder
                                            })
                                            .size_4(),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .id((ElementId::from(id.clone()), "title"))
                                    .w_0()
                                    .flex_1()
                                    .min_w_0()
                                    .h_6()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .when(project, |label| {
                                        let path = id.strip_prefix("project:").unwrap().to_owned();
                                        label.tooltip(move |window, cx| {
                                            Tooltip::new(path.clone()).build(window, cx)
                                        })
                                    })
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
                            )
                            .when(project || !folder, |content| {
                                content.child(
                                    // Stop bubbling after the popover's trigger
                                    // receives the press, before the row can drag.
                                    div()
                                        .id((ElementId::from(button_id.clone()), "menu-trigger"))
                                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                            cx.stop_propagation()
                                        })
                                        .child(
                                            Button::new((
                                                ElementId::from(button_id.clone()),
                                                "menu",
                                            ))
                                            .ghost()
                                            .xsmall()
                                            .icon(IconName::Ellipsis)
                                            .accessibility_label(format!(
                                                "管理{}：{}",
                                                if project { "项目" } else { "会话" },
                                                entry.item().label
                                            ))
                                            .tooltip(if project {
                                                "项目操作"
                                            } else {
                                                "会话操作"
                                            })
                                            .on_click(|_, _, cx| cx.stop_propagation())
                                            .dropdown_menu(move |menu, window, cx| {
                                                organization::row_menu(
                                                    button_owner.clone(),
                                                    &button_id,
                                                    menu,
                                                    window,
                                                    cx,
                                                )
                                            }),
                                        ),
                                )
                            })
                            .map(move |content| {
                                if folder && !project {
                                    content.into_any_element()
                                } else {
                                    content
                                        .context_menu(move |menu, window, cx| {
                                            organization::row_menu(
                                                menu_owner.clone(),
                                                &menu_id,
                                                menu,
                                                window,
                                                cx,
                                            )
                                        })
                                        .into_any_element()
                                }
                            }),
                    )
                    .when(project, |row| {
                        row.drag_over::<organization::ProjectDrag>(move |style, dragged, _, cx| {
                            if dragged.owner_matches(&drag_target) {
                                let style =
                                    style.bg(cx.theme().accent).border_color(cx.theme().ring);
                                if dragged.drop_before(&drag_target) {
                                    style.border_t_2()
                                } else {
                                    style.border_b_2()
                                }
                            } else {
                                style
                            }
                        })
                        .on_drop(
                            move |drag: &organization::ProjectDrag, window, cx| {
                                organization::drop_project(
                                    drag,
                                    drop_id.strip_prefix("project:").unwrap(),
                                    &drop_owner,
                                    window,
                                    cx,
                                );
                            },
                        )
                    })
                    .on_click(move |_, window, cx| {
                        if project {
                            cx.stop_propagation();
                            let _ =
                                owner.update(cx, |view, cx| view.toggle_project(&id, window, cx));
                        } else if let Some(session) = id.strip_prefix("session:") {
                            let _ = owner
                                .update(cx, |view, cx| view.select_session(session, window, cx));
                        }
                    })
            }))
    }
}
