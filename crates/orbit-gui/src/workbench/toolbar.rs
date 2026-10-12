//! The tab viewport and window actions belong to the window, not a conversation.
use super::*;
use crate::workspace::WorkspaceCommand;
use gpui_kit::{
    assets::IconName,
    component::{
        ActiveTheme, ElementExt, Icon, Sizable, TitleBar,
        button::{Button, ButtonVariants},
        h_flex,
        menu::{DropdownMenu, PopupMenuItem},
        spinner::Spinner,
        tab::{Tab, TabBar},
        tooltip::Tooltip,
    },
    prelude::FluentBuilder,
};

impl Workbench {
    pub(super) fn adjacent(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let keys: Vec<_> = self
            .tasks
            .iter()
            .filter(|task| !task.closed)
            .map(|task| task.view.read(cx).key().to_owned())
            .collect();
        if let Some(index) = keys.iter().position(|key| *key == self.active) {
            let next = if forward {
                (index + 1) % keys.len()
            } else {
                (index + keys.len() - 1) % keys.len()
            };
            self.activate(&keys[next], window, cx);
        }
    }
    pub(super) fn with_active(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        f: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>),
    ) {
        if let Some(view) = self
            .tasks
            .iter()
            .find(|task| task.view.read(cx).key() == self.active)
            .map(|task| task.view.clone())
        {
            view.update(cx, |view, cx| f(view, window, cx));
        }
    }
    pub(super) fn toolbar(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let tasks: Vec<_> = self
            .tasks
            .iter()
            .filter(|task| !task.closed)
            .map(|task| {
                let mut summary = task.view.read(cx).summary();
                summary.unread = self.unread.contains(&summary.key);
                summary
            })
            .collect();
        let active = tasks
            .iter()
            .position(|task| task.key == self.active)
            .unwrap_or(0);
        let tabs = TabBar::new("window-tasks")
            .w_full()
            .segmented()
            .track_scroll(&self.task_scroll)
            .max_width(window.rem_size() * 14.)
            .menu(true)
            .selected_index(active)
            .children(tasks.iter().map(|task| {
                let key = task.key.clone();
                let title = format!("{} · {} · {}", task.title, task.project, task.status);
                Tab::new()
                    .px_2()
                    .label(task.title.clone())
                    .prefix(Icon::new(IconName::BookOpen).size_4())
                    .when(task.attention, |tab| {
                        tab.prefix(Icon::new(IconName::CircleAlert).size_3())
                    })
                    .when(task.loading && !task.attention, |tab| {
                        tab.prefix(Spinner::new().small())
                    })
                    .when(task.unread && !task.attention && !task.loading, |tab| {
                        tab.prefix(div().size_1p5().rounded_full().bg(cx.theme().primary))
                    })
                    .tooltip(move |window, cx| Tooltip::new(title.clone()).build(window, cx))
                    .suffix(
                        Button::new(SharedString::from(format!("close-task-{key}")))
                            .ghost()
                            .xsmall()
                            .icon(IconName::Close)
                            .accessibility_label(format!("关闭 {}", task.title))
                            .tooltip(crate::keymap::tooltip(
                                if task.busy {
                                    "关闭标签页，任务继续运行"
                                } else {
                                    "关闭标签页，保留会话记录"
                                },
                                "close_task",
                                cx,
                            ))
                            .on_click(cx.listener(move |view, _, window, cx| {
                                cx.stop_propagation();
                                view.close(&key, window, cx);
                            })),
                    )
            }))
            .on_click(cx.listener(move |view, index: &usize, window, cx| {
                if let Some(task) = tasks.get(*index) {
                    view.activate(&task.key, window, cx);
                }
            }));
        let owner = cx.weak_entity();
        TitleBar::new().child(
            h_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(
                    Button::new("toggle-sidebar")
                        .ghost()
                        .small()
                        .icon(IconName::PanelLeft)
                        .accessibility_label("切换侧栏")
                        .tooltip(crate::keymap::tooltip("切换侧栏", "toggle_sidebar", cx))
                        .on_click(cx.listener(|view, _, _, cx| {
                            view.sidebar.update(cx, |sidebar, cx| sidebar.toggle(cx))
                        })),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(tabs)
                        .on_prepaint(move |bounds, _, cx| {
                            let _ = owner.update(cx, |view, cx| {
                                if !view.task_reveal && view.task_width == Some(bounds.size.width) {
                                    return;
                                }
                                view.task_width = Some(bounds.size.width);
                                view.task_reveal = false;
                                let before = view.task_scroll.offset();
                                if let Some(tab) = view.task_scroll.bounds_for_item(active) {
                                    let viewport = view.task_scroll.bounds();
                                    let mut offset = before;
                                    if tab.right() + offset.x > viewport.right() {
                                        offset.x = viewport.right() - tab.right();
                                    }
                                    if tab.left() + offset.x < viewport.left() {
                                        offset.x = viewport.left() - tab.left();
                                    }
                                    offset.x =
                                        offset.x.clamp(-view.task_scroll.max_offset().x, px(0.));
                                    view.task_scroll.set_offset(offset);
                                }
                                if view.task_scroll.offset() != before {
                                    cx.notify();
                                }
                            });
                        }),
                )
                .child(
                    Button::new("new-task-tab")
                        .ghost()
                        .small()
                        .icon(IconName::Plus)
                        .accessibility_label("新任务")
                        .tooltip(crate::keymap::tooltip("新任务", "new_conversation", cx))
                        .on_click(cx.listener(|view, _, window, cx| {
                            view.with_active(window, cx, |view, _, cx| view.new_task(cx))
                        })),
                )
                .child(
                    Button::new("project-actions")
                        .ghost()
                        .small()
                        .icon(IconName::Ellipsis)
                        .accessibility_label("项目操作")
                        .tooltip("项目操作")
                        .dropdown_menu({
                            let owner = cx.weak_entity();
                            move |menu, _, _| {
                                let mut menu = menu;
                                for (command, label) in [
                                    (WorkspaceCommand::OpenProject, "打开本地项目…"),
                                    (WorkspaceCommand::RemoteProject, "连接远程项目…"),
                                    (WorkspaceCommand::File, "打开文件…"),
                                    (WorkspaceCommand::Tasks, "项目任务"),
                                    (WorkspaceCommand::History, "会话历史"),
                                    (WorkspaceCommand::Export, "导出补丁"),
                                    (WorkspaceCommand::Diagnostics, "诊断"),
                                ] {
                                    let owner = owner.clone();
                                    menu = menu.item(PopupMenuItem::new(label).on_click(
                                        move |_, window, cx| {
                                            let _ = owner.update(cx, |view, cx| {
                                                view.show_workspace(window, cx);
                                                view.with_active(window, cx, |view, window, cx| {
                                                    view.window_command(command, window, cx)
                                                })
                                            });
                                        },
                                    ));
                                }
                                menu
                            }
                        }),
                )
                .child(
                    Button::new("toggle-inspector")
                        .ghost()
                        .small()
                        .icon(IconName::PanelRight)
                        .accessibility_label("工作面板")
                        .tooltip(crate::keymap::tooltip(
                            "执行、计划、变更与 Agent",
                            "toggle_inspector",
                            cx,
                        ))
                        .on_click(cx.listener(|view, _, window, cx| {
                            view.show_workspace(window, cx);
                            view.with_active(window, cx, |view, window, cx| {
                                view.toggle_inspector(window, cx)
                            })
                        })),
                ),
        )
    }
}
