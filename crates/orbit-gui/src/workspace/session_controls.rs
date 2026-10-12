//! Object-bound session dialogs. Completion always belongs to the captured
//! workspace service and session, even after sidebar selection changes.
use super::*;
use gpui_kit::component::input::{Input, InputState};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Operation {
    Rename,
    Delete,
}

struct SessionControls {
    owner: WeakEntity<Workspace>,
    options: Options,
    id: String,
    location: SharedString,
    operation: Operation,
    input: Entity<InputState>,
    pending: bool,
    error: Option<SharedString>,
    _subscription: Subscription,
}

impl Workspace {
    pub(in crate::workspace) fn rename_session(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.session_controls(id, Operation::Rename, window, cx);
    }

    pub(in crate::workspace) fn delete_session(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.session_controls(id, Operation::Delete, window, cx);
    }

    pub(in crate::workspace) fn session_busy(&self, id: &str) -> bool {
        self.navigation_tasks
            .iter()
            .any(|task| task.session.as_deref() == Some(id) && task.busy)
            || (self.info.as_ref().is_some_and(|info| info.id == id)
                && (self.busy || self.pending.is_some()))
    }

    fn session_controls(
        &mut self,
        id: &str,
        operation: Operation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entry = self
            .sessions
            .iter()
            .find(|entry| entry.id.as_ref() == id)
            .cloned()
            .or_else(|| {
                self.info
                    .as_ref()
                    .filter(|info| {
                        info.id == id && (info.persisted || operation == Operation::Rename)
                    })
                    .map(|info| SessionEntry {
                        id: info.id.clone().into(),
                        title: info.title.clone().into(),
                        workspace: info.workspace.clone().into(),
                    })
            });
        let Some(entry) = entry else { return };
        let title = entry.title.to_string();
        if operation == Operation::Delete && self.session_busy(id) {
            self.report_error("会话正在运行，请等待任务结束后再删除。", cx);
            return;
        }
        let owner = cx.weak_entity();
        let options = self.options.clone();
        let form = cx.new(|cx| SessionControls::new(owner, options, entry, operation, window, cx));
        if operation == Operation::Rename {
            let first = std::cell::Cell::new(true);
            window.open_dialog(cx, move |dialog, window, cx| {
                if first.replace(false) {
                    let input = form.read(cx).input.clone();
                    input.update(cx, |input, cx| {
                        input.focus(window, cx);
                    });
                }
                dialog
                    .title("重命名会话")
                    .width(window.rem_size() * 28.)
                    .close_button(false)
                    .overlay_closable(false)
                    .child(form.clone())
                    .on_ok({
                        let form = form.clone();
                        move |_, _, cx| {
                            form.update(cx, |view, cx| view.submit(cx));
                            false
                        }
                    })
                    .on_cancel({
                        let form = form.clone();
                        move |_, _, cx| !form.read(cx).pending
                    })
            });
        } else {
            let title = if title.trim().is_empty() {
                "未命名会话".to_owned()
            } else {
                title
            };
            window.open_alert_dialog(cx, move |dialog, window, _| {
                dialog
                    .title(format!("删除“{title}”？"))
                    .width(window.rem_size() * 28.)
                    .child(form.clone())
                    .footer(div())
                    .on_ok({
                        let form = form.clone();
                        move |_, _, cx| {
                            form.update(cx, |view, cx| view.submit(cx));
                            false
                        }
                    })
                    .on_cancel({
                        let form = form.clone();
                        move |_, _, cx| !form.read(cx).pending
                    })
            });
        }
    }
}

impl SessionControls {
    fn new(
        owner: WeakEntity<Workspace>,
        options: Options,
        entry: SessionEntry,
        operation: Operation,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("会话名称");
            input.set_value(entry.title.clone(), window, cx);
            input
        });
        let subscription = cx.subscribe_in(&input, window, |view, _, event, _, cx| match event {
            InputEvent::Change => {
                view.error = None;
                cx.notify();
            }
            InputEvent::PressEnter { shift: false, .. } => view.submit(cx),
            _ => {}
        });
        Self {
            owner,
            options,
            id: entry.id.to_string(),
            location: entry.workspace,
            operation,
            input,
            pending: false,
            error: None,
            _subscription: subscription,
        }
    }

    fn submit(&mut self, cx: &mut Context<Self>) {
        if self.pending {
            return;
        }
        let title = self.input.read(cx).value().trim().to_owned();
        if self.operation == Operation::Rename && (title.is_empty() || title.chars().count() > 120)
        {
            self.error = Some("会话名称应为 1–120 个字符。".into());
            cx.notify();
            return;
        }
        let request = Request::Admin {
            action: if self.operation == Operation::Rename {
                orbit_protocol::admin::Action::RenameSession {
                    id: self.id.clone(),
                    title: title.clone(),
                }
            } else {
                orbit_protocol::admin::Action::DeleteSession {
                    id: self.id.clone(),
                }
            },
        };
        let (result, response) = tokio::sync::oneshot::channel();
        let sent = self.owner.update(cx, |view, _| -> Result<(), String> {
            if !view.options.same_host(&self.options) || !view.connected {
                return Err("连接已改变，请重新打开会话操作。".into());
            }
            if self.operation == Operation::Delete && view.session_busy(&self.id) {
                return Err("会话正在运行，请等待任务结束后再删除。".into());
            }
            view.backend
                .as_ref()
                .ok_or_else(|| "连接已断开，请重新连接后再试。".to_owned())?
                .send(Command::Settings { request, result })
                .map_err(|error| error.to_string())
        });
        let sent = sent.unwrap_or_else(|_| Err("工作区已关闭。".into()));
        if let Err(error) = sent {
            self.error = Some(error.into());
            cx.notify();
            return;
        }
        self.pending = true;
        self.error = None;
        let workspace = self.owner.clone();
        let options = self.options.clone();
        let id = self.id.clone();
        let operation = self.operation;
        // Keep reconciliation alive if the dialog is dismissed or its tab is
        // closed after the service has already committed the mutation.
        cx.spawn(async move |form, cx| {
            let result = response
                .await
                .unwrap_or_else(|_| Err("连接已断开，无法确认会话操作结果。".into()));
            let result = result.and_then(|response| match response {
                Response::Report(_) | Response::Ok => Ok(()),
                _ => Err("服务未返回会话操作结果，请更新服务后重试。".into()),
            });
            if result.is_ok() {
                let _ = workspace.update(cx, |view, cx| {
                    if !view.options.same_host(&options) {
                        return;
                    }
                    if operation == Operation::Rename {
                        if let Some(entry) = view.sessions.iter_mut().find(|entry| entry.id == id) {
                            entry.title = title.clone().into();
                        }
                        if let Some(info) = &mut view.info
                            && info.id == id
                        {
                            info.title = title.clone();
                        }
                    } else {
                        view.sessions.retain(|entry| entry.id != id);
                    }
                    view.refresh_title(cx);
                    cx.emit(WorkspaceEvent::Catalog(view.sessions.clone()));
                    view.command(Command::Request(Request::AllSessions), cx);
                    if operation == Operation::Delete {
                        cx.emit(WorkspaceEvent::SessionDeleted {
                            id: id.clone(),
                            options: Box::new(options.clone()),
                        });
                    }
                    cx.emit(WorkspaceEvent::Changed);
                    cx.notify();
                });
            }
            let _ = form.update_in(cx, |view, window, cx| {
                view.pending = false;
                match result {
                    Ok(()) => window.close_dialog(cx),
                    Err(error) => {
                        view.error = Some(error.into());
                        cx.notify();
                    }
                }
            });
        })
        .detach();
        cx.notify();
    }
}

impl Render for SessionControls {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rename = self.operation == Operation::Rename;
        v_flex()
            .gap_4()
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!("项目目录：{}", self.location)),
            )
            .when(rename, |view| {
                view.child(
                    v_flex()
                        .gap_1()
                        .child(div().text_sm().child("会话名称"))
                        .child(
                            Input::new(&self.input)
                                .aria_label("会话名称")
                                .disabled(self.pending),
                        ),
                )
            })
            .when(!rename, |view| {
                view.child(
                    div()
                        .text_sm()
                        .child("会话记录和私有附件将被永久删除，无法恢复。"),
                )
            })
            .when_some(self.error.clone(), |view, error| {
                view.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("session-cancel")
                            .label("取消")
                            .disabled(self.pending)
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("session-confirm")
                            .label(if self.pending {
                                if rename { "保存中" } else { "删除中" }
                            } else if rename {
                                "重命名"
                            } else {
                                "删除"
                            })
                            .when(rename, |button| button.primary())
                            .when(!rename, |button| button.danger())
                            .disabled(self.pending)
                            .on_click(cx.listener(|view, _, _, cx| view.submit(cx))),
                    ),
            )
    }
}
