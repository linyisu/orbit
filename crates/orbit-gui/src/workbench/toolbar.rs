//! The tab viewport and window actions belong to the window, not a conversation.
use super::*;
use gpui_kit::{
    assets::IconName,
    component::{
        ActiveTheme, ElementExt, Icon, Sizable, TitleBar,
        button::{Button, ButtonVariants},
        h_flex,
        spinner::Spinner,
        tab::{Tab, TabBar},
        tooltip::Tooltip,
    },
    prelude::FluentBuilder,
};
use std::{cell::Cell, rc::Rc};

pub(super) type ControlProbe = Rc<Cell<[Option<Bounds<Pixels>>; 2]>>;

/// Decide against the pre-close order, including retained background tasks.
/// Prefer the nearest open tab on the left; only the first tab falls right.
pub(super) fn close_neighbor<T>(
    tabs: &[T],
    index: usize,
    closed: impl Fn(&T) -> bool,
) -> Option<usize> {
    if index >= tabs.len() {
        return None;
    }
    (0..index)
        .rev()
        .chain(index + 1..tabs.len())
        .find(|ix| !closed(&tabs[*ix]))
}

impl Workbench {
    pub(super) fn adjacent(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let keys: Vec<_> = self
            .tasks
            .iter()
            .filter(|task| !task.closed)
            .map(|task| task.view.read(cx).key().to_owned())
            .collect();
        if let Some(index) = keys
            .iter()
            .position(|key| *key == self.active)
        {
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
            .menu(false)
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
                        tab.prefix(
                            div()
                                .size_1p5()
                                .rounded_full()
                                .bg(cx.theme().primary),
                        )
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
                .child(titlebar_control(
                    Button::new("toggle-sidebar")
                        .ghost()
                        .small()
                        .icon(IconName::PanelLeft)
                        .accessibility_label("切换侧栏")
                        .tooltip(crate::keymap::tooltip("切换侧栏", "toggle_sidebar", cx))
                        .on_click(cx.listener(|view, _, _, cx| {
                            view.sidebar
                                .update(cx, |sidebar, cx| sidebar.toggle(cx))
                        })),
                    self.control_probe.clone(),
                    0,
                ))
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
                                    offset.x = offset
                                        .x
                                        .clamp(-view.task_scroll.max_offset().x, px(0.));
                                    view.task_scroll.set_offset(offset);
                                }
                                if view.task_scroll.offset() != before {
                                    cx.notify();
                                }
                            });
                        }),
                )
                .child(titlebar_control(
                    Button::new("toggle-inspector")
                        .ghost()
                        .small()
                        .icon(IconName::PanelRight)
                        .accessibility_label("会话详情")
                        .tooltip(crate::keymap::tooltip(
                            "查看会话详情",
                            "toggle_inspector",
                            cx,
                        ))
                        .on_click(cx.listener(|view, _, window, cx| {
                            view.show_workspace(window, cx);
                            view.with_active(window, cx, |view, window, cx| {
                                view.toggle_inspector(window, cx)
                            })
                        })),
                    self.control_probe.clone(),
                    1,
                )),
        )
    }
}

fn titlebar_control(
    control: impl IntoElement,
    probe: Option<ControlProbe>,
    index: usize,
) -> impl IntoElement {
    div()
        .occlude()
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .when_some(probe, |control, probe| {
            control.on_prepaint(move |bounds, _, _| {
                let mut controls = probe.get();
                controls[index] = Some(bounds);
                probe.set(controls);
            })
        })
        .child(control)
}

#[cfg(test)]
mod tests {
    use super::close_neighbor;
    #[test]
    fn closing_tabs_prefers_left_skips_hidden_runs_and_only_falls_right_at_the_start() {
        let tabs = [false, true, false, false];
        assert_eq!(close_neighbor(&tabs, 3, |closed| *closed), Some(2));
        assert_eq!(close_neighbor(&tabs, 2, |closed| *closed), Some(0));
        assert_eq!(close_neighbor(&tabs, 0, |closed| *closed), Some(2));
        assert_eq!(close_neighbor(&[false], 0, |closed| *closed), None);
        assert_eq!(
            close_neighbor(&[true, false, true], 1, |closed| *closed),
            None
        );
        assert_eq!(close_neighbor(&tabs, tabs.len(), |closed| *closed), None);
    }
}
