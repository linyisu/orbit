//! One non-blocking notification and one details surface for workspace failures.
use super::*;
use gpui_kit::component::notification::Notification;

impl Workspace {
    pub(super) fn cancel_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.receive = Task::ready(());
        self.reconnect = Task::ready(());
        self.backend = None;
        self.clear_connection_challenges(window, cx);
        if let Some(tasks) = &self.tasks {
            tasks.update(cx, |tasks, cx| tasks.disconnected(window, cx));
        }
        self.recovery = Default::default();
        self.connected = false;
        self.opening = false;
        self.connection_stage = "连接已取消".into();
        self.error = Some("连接已取消。可以重新连接或修改连接信息。".into());
        cx.emit(WorkspaceEvent::Changed);
        cx.notify();
    }

    pub(super) fn connection_failure(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .p_6()
            .gap_3()
            .child(
                Icon::new(IconName::CircleAlert)
                    .size_6()
                    .text_color(cx.theme().muted_foreground),
            )
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::MEDIUM)
                    .child("未能连接工作区"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.environment()),
            )
            .child(
                div()
                    .max_w(rems(36.))
                    .text_sm()
                    .child(self.error.clone().unwrap_or_default()),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("connection-edit")
                            .small()
                            .label(
                                if self.options.ssh.is_some() || self.options.wsl.is_some() {
                                    "修改连接…"
                                } else {
                                    "选择项目…"
                                },
                            )
                            .on_click(cx.listener(|view, _, window, cx| {
                                if view.options.ssh.is_some() || view.options.wsl.is_some() {
                                    view.remote_connection_dialog(window, cx);
                                } else {
                                    view.choose_project_directory(view.options.resume.clone(), cx);
                                }
                            })),
                    )
                    .child(
                        Button::new("connection-retry")
                            .small()
                            .primary()
                            .label("重试")
                            .on_click(cx.listener(|view, _, window, cx| view.connect(window, cx))),
                    ),
            )
    }
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
        let remote = self.options.ssh.is_some() || self.options.wsl.is_some();
        window.open_dialog(cx, move |dialog, _, _| {
            let retry = owner.clone();
            let edit = owner.clone();
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
                            row.when(remote, |row| {
                                row.child(
                                    Button::new("edit-connection").label("修改连接…").on_click(
                                        move |_, window, cx| {
                                            window.close_dialog(cx);
                                            let _ = edit.update(cx, |view, cx| {
                                                view.remote_connection_dialog(window, cx)
                                            });
                                        },
                                    ),
                                )
                            })
                            .child(
                                Button::new("retry-project").label("重试").on_click(
                                    move |_, window, cx| {
                                        window.close_dialog(cx);
                                        let _ =
                                            retry.update(cx, |view, cx| view.connect(window, cx));
                                    },
                                ),
                            )
                        }),
                )
        });
    }
}
