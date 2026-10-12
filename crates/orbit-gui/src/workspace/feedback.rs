//! One non-blocking notification and one details surface for workspace failures.
use super::*;
use gpui_kit::component::notification::Notification;

impl Workspace {
    pub(super) fn report_error(&mut self, error: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.error = Some(error.into());
        if let Some(form) = &self.question_form {
            let form = form.clone();
            cx.defer(move |cx| form.update(cx, |form, cx| form.retry(cx)));
        }
        cx.spawn(async move |owner, cx| {
            let _ = owner.update_in(cx, |view, window, cx| {
                if view.error.is_none() || !view.visible {
                    return;
                }
                let owner = cx.weak_entity();
                window.push_notification(
                    Notification::error("查看详情")
                        .title(if view.connected && view.info.is_some() {
                            "操作未完成"
                        } else if view.options.resume.is_some() {
                            "无法打开会话"
                        } else {
                            "无法打开项目"
                        })
                        .id::<Workspace>()
                        .on_click(move |_, window, cx| {
                            let _ = owner.update(cx, |view, cx| view.show_error(window, cx));
                        }),
                    cx,
                );
            });
        })
        .detach();
        cx.notify();
    }

    pub(super) fn show_error(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(error) = self.error.clone() else {
            return;
        };
        let disconnected = !self.connected || self.info.is_none();
        let resuming = self.options.resume.is_some();
        let owner = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let retry = owner.clone();
            dialog
                .title(if disconnected {
                    if resuming {
                        "无法打开会话"
                    } else {
                        "无法打开项目"
                    }
                } else {
                    "操作未完成"
                })
                .child(
                    div()
                        .id("failure-details")
                        .max_h(rems(18.))
                        .overflow_y_scroll()
                        .text_sm()
                        .child(error.clone()),
                )
                .footer(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("close-failure")
                                .label("关闭")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .when(disconnected, |row| {
                            row.child(Button::new("retry-project").label("重试").on_click(
                                move |_, window, cx| {
                                    window.close_dialog(cx);
                                    let _ = retry.update(cx, |view, cx| view.connect(window, cx));
                                },
                            ))
                        }),
                )
        });
    }
}
