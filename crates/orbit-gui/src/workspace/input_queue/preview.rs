//! Compact view of the same service-backed queue used by the full editor.
use super::*;

pub(in crate::workspace) fn preview(
    panel: &Entity<InputQueue>,
    cx: &mut Context<Workspace>,
) -> AnyElement {
    let queue = panel.read(cx);
    let rows = queue
        .rows
        .iter()
        .filter(|row| row.confirmed && row.entry.state != State::Claimed)
        .collect::<Vec<_>>();
    if rows.is_empty() && queue.error.is_none() {
        return div().into_any_element();
    }
    let mutable = queue.requests.ready();
    let mut view = v_flex().w_full().min_w_0().gap_1().pb_2().child(
        h_flex()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "暂存消息 · {}{}",
                        rows.len(),
                        if queue.next.is_some() { "+" } else { "" }
                    )),
            )
            .child(
                Button::new("manage-queued-inputs")
                    .ghost()
                    .xsmall()
                    .label("管理…")
                    .on_click(cx.listener(|workspace, _, window, cx| {
                        workspace.open_input_queue(window, cx)
                    })),
            ),
    );
    for row in rows.iter().take(3) {
        let id = row.entry.id;
        let snippet = row.entry.content.chars().take(240).collect::<String>();
        let title = snippet.split_whitespace().collect::<Vec<_>>().join(" ");
        let remove = panel.downgrade();
        view = view.child(
            h_flex()
                .w_full()
                .min_w_0()
                .gap_2()
                .py_1()
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
                        .child(state_label(row.entry.state)),
                )
                .child(
                    Button::new(("edit-queued", id as u64))
                        .ghost()
                        .xsmall()
                        .icon(IconName::Pencil)
                        .accessibility_label(format!("编辑暂存消息 #{id}"))
                        .tooltip("查看或编辑这条暂存消息")
                        .on_click(cx.listener(move |workspace, _, window, cx| {
                            workspace.edit_queued_input(id, window, cx)
                        })),
                )
                .child(
                    Button::new(("cancel-queued", id as u64))
                        .ghost()
                        .xsmall()
                        .icon(IconName::X)
                        .accessibility_label(format!("撤回暂存消息 #{id}"))
                        .tooltip("从暂存队列中撤回")
                        .disabled(!mutable || !row.entry.editable() || row.uncertain)
                        .on_click(move |_, window, cx| {
                            let _ =
                                remove.update(cx, |queue, cx| queue.confirm_remove(id, window, cx));
                        }),
                ),
        );
    }
    if rows.len() > 3 || queue.next.is_some() {
        view = view.child(
            Button::new("more-queued-inputs")
                .ghost()
                .xsmall()
                .label("查看全部暂存消息…")
                .on_click(
                    cx.listener(|workspace, _, window, cx| workspace.open_input_queue(window, cx)),
                ),
        );
    }
    if let Some(error) = &queue.error {
        view = view.child(
            Button::new("retry-queued-inputs")
                .ghost()
                .xsmall()
                .label("暂存状态未更新 · 查看详情")
                .tooltip(error.clone())
                .on_click(
                    cx.listener(|workspace, _, window, cx| workspace.open_input_queue(window, cx)),
                ),
        );
    }
    view.into_any_element()
}
