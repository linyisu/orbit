//! Queue messages live beside the composer, without a second navigation surface.
use super::*;

pub(super) fn preview(
    queue: &mut InputQueue,
    window: &mut Window,
    cx: &mut Context<InputQueue>,
) -> AnyElement {
    // Read failures alone must not invent an empty queue UI. Keep actual local
    // drafts visible even when the server can no longer confirm their entry.
    let visible = queue
        .rows
        .iter()
        .filter(|row| {
            row.entry.state != State::Claimed
                || keep_absent_draft(
                    &row.draft.read(cx).value(),
                    &row.entry.content,
                    row.uncertain,
                )
        })
        .collect::<Vec<_>>();
    if visible.is_empty() {
        return div().into_any_element();
    }
    let mutable = queue.requests.ready();
    let mut view = v_flex()
        .id("staged-messages")
        .w_full()
        .min_w_0()
        .gap_1()
        .pb_2();
    for row in visible.iter().take(if queue.expanded { 0 } else { 3 }) {
        let id = row.entry.id;
        let dirty = row.draft.read(cx).value().as_ref() != row.entry.content;
        let snippet = row
            .draft
            .read(cx)
            .value()
            .chars()
            .take(240)
            .collect::<String>();
        let title = snippet.split_whitespace().collect::<Vec<_>>().join(" ");
        view = view.child(
            h_flex()
                .id(("staged-message", id as u64))
                .w_full()
                .min_w_0()
                .gap_2()
                .px_3()
                .py_1()
                .bg(cx.theme().muted)
                .rounded_lg()
                .child(Icon::new(IconName::List).small())
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_sm()
                        .text_ellipsis()
                        .child(title),
                )
                .child(
                    div()
                        .text_xs()
                        .flex_shrink_0()
                        .text_color(cx.theme().muted_foreground)
                        .child(if dirty {
                            "未保存草稿"
                        } else if row.uncertain || !row.confirmed {
                            "等待确认"
                        } else {
                            state_label(row.entry.state)
                        }),
                )
                .child(
                    Button::new(("edit-queued", id as u64))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Pencil)
                        .accessibility_label(format!("编辑暂存消息 #{id}"))
                        .tooltip("查看或编辑暂存消息")
                        .on_click(cx.listener(move |queue, _, window, cx| {
                            queue.select(Some(id), window, cx)
                        })),
                )
                .child(
                    Button::new(("cancel-queued", id as u64))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Trash)
                        .accessibility_label(format!("撤回暂存消息 #{id}"))
                        .tooltip("撤回暂存消息")
                        .disabled(
                            !mutable || !row.confirmed || !row.entry.editable() || row.uncertain,
                        )
                        .on_click(cx.listener(move |queue, _, _, cx| queue.remove(id, cx))),
                ),
        );
    }
    if visible.len() > 3 || queue.next.is_some() || queue.expanded {
        view = view
            .child(
                h_flex().gap_2().child(
                    Button::new("more-queued-inputs")
                        .ghost()
                        .xsmall()
                        .label(if queue.expanded {
                            "收起列表".to_owned()
                        } else {
                            format!(
                                "查看全部 {}{} 条暂存消息",
                                visible.len(),
                                if queue.next.is_some() { "+" } else { "" }
                            )
                        })
                        .on_click(cx.listener(|queue, _, _, cx| {
                            queue.expanded = !queue.expanded;
                            cx.notify();
                        })),
                ),
            )
            .when(queue.expanded, |view| {
                view.child(
                    div()
                        .h(rems(8.))
                        .max_h(window.viewport_size().height * 0.18)
                        .min_h_0()
                        .min_w_0()
                        .border_1()
                        .border_color(cx.theme().border)
                        .rounded_lg()
                        .child(List::new(&queue.list)),
                )
            })
            .when(queue.expanded, |view| {
                view.when_some(queue.next, |view, after| {
                    view.child(
                        Button::new("load-more-queued-inputs")
                            .ghost()
                            .xsmall()
                            .label("加载后续消息")
                            .disabled(
                                queue.requests.pending.is_some()
                                    || !queue.requests.connected
                                    || queue.requests.needs_refresh,
                            )
                            .on_click(
                                cx.listener(move |queue, _, _, cx| queue.load(Some(after), cx)),
                            ),
                    )
                })
            });
    }
    view = view.when_some(queue.error.clone(), |view, error| {
        view.child(
            h_flex()
                .min_w_0()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .whitespace_normal()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
                .child(
                    Button::new("retry-queued-inputs")
                        .ghost()
                        .xsmall()
                        .label("刷新")
                        .disabled(!queue.requests.connected || queue.requests.pending.is_some())
                        .on_click(cx.listener(|queue, _, _, cx| queue.load(None, cx))),
                ),
        )
    });
    if let Some(row) = queue
        .rows
        .iter()
        .find(|row| Some(row.entry.id) == queue.selected)
    {
        let error = row
            .error
            .clone()
            .filter(|error| queue.error.as_ref() != Some(error));
        view = view
            .child(queue.editor(window, cx))
            .when_some(error, |view, error| {
                view.child(div().text_xs().text_color(cx.theme().danger).child(error))
            });
    }
    view.into_any_element()
}
