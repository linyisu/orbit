//! User-turn navigation; the message scroller remains the scroll owner.
use gpui_kit::component::{
    ActiveTheme, Disableable, Sizable,
    button::{Button, ButtonVariants},
    message_scroller::MessageScrollerState,
    scroll::Scrollbar,
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

struct Anchor {
    row: usize,
    preview: SharedString,
}

pub(super) struct Navigation {
    anchors: Vec<Anchor>,
    active: usize,
    messages: Entity<MessageScrollerState>,
    dots: UniformListScrollHandle,
    focus: FocusHandle,
    _subscription: Subscription,
}

actions!(
    conversation_navigation,
    [PreviousRound, NextRound, FirstRound, LatestRound]
);
pub(crate) fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("up", PreviousRound, Some("ConversationNavigation")),
        KeyBinding::new("down", NextRound, Some("ConversationNavigation")),
        KeyBinding::new("home", FirstRound, Some("ConversationNavigation")),
        KeyBinding::new("end", LatestRound, Some("ConversationNavigation")),
    ]);
}

impl Navigation {
    pub(super) fn new(messages: Entity<MessageScrollerState>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&messages, |_, _, cx| cx.notify());
        Self {
            anchors: vec![],
            active: 0,
            messages,
            dots: UniformListScrollHandle::new(),
            focus: cx.focus_handle(),
            _subscription: subscription,
        }
    }
    pub(super) fn append(&mut self, row: usize, prompt: &str, cx: &mut Context<Self>) {
        let mut chars = prompt
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .chars()
            .collect::<Vec<_>>();
        if chars.len() > 160 {
            chars.truncate(160);
            chars.push('…');
        }
        let preview = if chars.is_empty() {
            "图片提问".into()
        } else {
            chars.into_iter().collect::<String>().into()
        };
        self.anchors.push(Anchor { row, preview });
        if self.messages.read(cx).is_following_tail() {
            self.set_active(self.anchors.len() - 1, cx);
        }
        cx.notify();
    }
    fn set_active(&mut self, active: usize, cx: &mut Context<Self>) {
        if self.active == active {
            return;
        }
        self.active = active;
        self.dots.scroll_to_item(active, ScrollStrategy::Center);
        cx.notify();
    }
    pub(super) fn visible_row(&mut self, row: usize, cx: &mut Context<Self>) {
        if self.anchors.is_empty() {
            return;
        }
        let active = if self.messages.read(cx).is_following_tail() {
            self.anchors.len() - 1
        } else {
            self.anchors
                .partition_point(|anchor| anchor.row <= row)
                .saturating_sub(1)
        };
        self.set_active(active, cx);
    }
    fn jump(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(anchor) = self.anchors.get(index) else {
            return;
        };
        let row = anchor.row;
        self.messages.update(cx, |scroll, cx| {
            scroll.scroll_to_item(row, cx);
        });
        self.set_active(index, cx);
    }
    fn latest(&mut self, cx: &mut Context<Self>) {
        self.messages
            .update(cx, |scroll, cx| scroll.scroll_to_end(cx));
        self.set_active(self.anchors.len().saturating_sub(1), cx);
    }
    fn dot(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let active = index == self.active;
        let label = format!(
            "第 {} 轮{}：{}",
            index + 1,
            if active { "，当前" } else { "" },
            self.anchors[index].preview
        );
        div()
            .id(("round-preview", index))
            .h_5()
            .w_full()
            .flex()
            .items_center()
            .justify_center()
            .tooltip({
                let label = label.clone();
                move |window, cx| {
                    let width = window.viewport_size().width / 4.;
                    let label = label.clone();
                    Tooltip::element(move |window, _| {
                        div()
                            .w(width - window.rem_size())
                            .whitespace_normal()
                            .child(label.clone())
                    })
                    .max_w(width)
                    .build(window, cx)
                }
            })
            .child(
                Button::new(("round", index))
                    .ghost()
                    .small()
                    .size_5()
                    .p_0()
                    .accessibility_label(label.clone())
                    .child(
                        div()
                            .size_2()
                            .rounded_full()
                            .border_1()
                            .border_color(if active {
                                cx.theme().primary
                            } else {
                                cx.theme().muted_foreground
                            })
                            .when(active, |dot| dot.bg(cx.theme().primary)),
                    )
                    .on_click(cx.listener(move |view, _, window, cx| {
                        window.focus(&view.focus, cx);
                        view.jump(index, cx);
                    })),
            )
            .into_any_element()
    }
}
impl Render for Navigation {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.anchors.is_empty() {
            return div().into_any_element();
        }
        let owner = cx.weak_entity();
        v_flex()
            .h_full()
            .w(rems(crate::theme::NAVIGATION_WIDTH_REM))
            .flex_shrink_0()
            .min_h_0()
            .py_2()
            .items_center()
            .key_context("ConversationNavigation")
            .track_focus(&self.focus)
            .on_action(cx.listener(|view, _: &PreviousRound, _, cx| {
                view.jump(view.active.saturating_sub(1), cx)
            }))
            .on_action(cx.listener(|view, _: &NextRound, _, cx| {
                view.jump((view.active + 1).min(view.anchors.len() - 1), cx)
            }))
            .on_action(cx.listener(|view, _: &FirstRound, _, cx| view.jump(0, cx)))
            .on_action(cx.listener(|view, _: &LatestRound, _, cx| view.latest(cx)))
            .child(
                div()
                    .relative()
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .child(
                        uniform_list(
                            "conversation-dots",
                            self.anchors.len(),
                            move |range, _, cx| {
                                owner
                                    .update(cx, |view, cx| {
                                        range.map(|index| view.dot(index, cx)).collect::<Vec<_>>()
                                    })
                                    .unwrap_or_default()
                            },
                        )
                        .size_full()
                        .track_scroll(&self.dots),
                    )
                    .child(Scrollbar::vertical(&self.dots)),
            )
            .child(
                Button::new("current-round")
                    .ghost()
                    .xsmall()
                    .label((self.active + 1).to_string())
                    .tooltip(format!(
                        "当前 {}/{} 轮 · 定位当前问题",
                        self.active + 1,
                        self.anchors.len()
                    ))
                    .accessibility_label(format!(
                        "定位当前问题，第 {} 轮，共 {} 轮",
                        self.active + 1,
                        self.anchors.len()
                    ))
                    .on_click(cx.listener(|view, _, window, cx| {
                        window.focus(&view.focus, cx);
                        view.dots
                            .scroll_to_item(view.active, ScrollStrategy::Center);
                        view.jump(view.active, cx);
                    })),
            )
            .child(
                Button::new("latest-round")
                    .ghost()
                    .small()
                    .icon(gpui_kit::assets::IconName::ArrowDown)
                    .tooltip("回到最新消息 · End")
                    .accessibility_label("回到最新消息")
                    .disabled(!self.messages.read(cx).is_scrolled_up())
                    .on_click(cx.listener(|view, _, window, cx| {
                        window.focus(&view.focus, cx);
                        view.latest(cx);
                    })),
            )
            .into_any_element()
    }
}

/// The row at the viewport's top owns the current-round marker, including
/// the gap before the next row. List overdraw must not select an offscreen row.
pub(super) fn current_row(rows: &[(usize, Bounds<Pixels>)], top: Pixels) -> Option<usize> {
    rows.iter()
        .filter(|(_, bounds)| bounds.top() <= top)
        .max_by_key(|(index, _)| *index)
        .or_else(|| rows.iter().min_by_key(|(index, _)| *index))
        .map(|(index, _)| *index)
}

#[cfg(test)]
mod tests {
    use super::current_row;
    use gpui_kit::{Bounds, point, px, size};
    #[test]
    fn viewport_anchor_ignores_overdraw_and_keeps_the_round_in_row_gaps() {
        let rows =
            [(3, -280., 240.), (4, -20., 100.), (5, 112., 300.)].map(|(index, y, height)| {
                (
                    index,
                    Bounds::new(point(px(0.), px(y)), size(px(400.), px(height))),
                )
            });
        assert_eq!(current_row(&rows, px(0.)), Some(4));
        assert_eq!(current_row(&rows, px(95.)), Some(4));
        assert_eq!(current_row(&rows, px(112.)), Some(5));
        assert_eq!(current_row(&rows, px(-300.)), Some(3));
        assert_eq!(current_row(&[], px(0.)), None);
    }
}
