use super::*;
use gpui_kit::component::input::{Input, InputState};

#[path = "remote.rs"]
mod remote;

impl Workspace {
    pub(super) fn open_project(&mut self, cx: &mut Context<Self>) {
        self.choose_project_directory(None, cx);
    }

    pub(super) fn choose_project_directory(
        &mut self,
        resume: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if self.choosing_project || self.opening {
            return;
        }
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(if resume.is_some() {
                "选择新目录并恢复".into()
            } else {
                "打开项目".into()
            }),
        });
        self.choosing_project = true;
        self.project_picker = cx.spawn(async move |owner, cx| {
            let result = prompt.await;
            let _ = owner.update_in(cx, |view, window, cx| {
                view.choosing_project = false;
                match result {
                    Ok(Ok(Some(paths))) => {
                        if let Some(path) = paths.into_iter().next() {
                            let relink = resume.is_some();
                            view.switch_project(path, resume, true, relink, window, cx);
                        }
                    }
                    Ok(Err(error)) => view.report_error(format!("无法选择目录：{error}"), cx),
                    Err(error) => view.report_error(format!("目录选择器已关闭：{error}"), cx),
                    Ok(Ok(None)) => {}
                }
                cx.notify();
            });
        });
        cx.notify();
    }

    pub(super) fn switch_project(
        &mut self,
        path: PathBuf,
        resume: Option<String>,
        local: bool,
        relink: bool,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut options = self.options.clone();
        if local {
            if options.ssh.is_some() || options.wsl.is_some() {
                options.server = None;
                options.database = None;
            }
            options.ssh = None;
            options.ssh_user = None;
            options.ssh_port = None;
            options.identity = None;
            options.wsl = None;
            options.wsl_user = None;
        }
        options.workspace = Some(path);
        options.resume = resume;
        options.recover_draft = false;
        options.relink_on_resume = relink;
        cx.emit(WorkspaceEvent::Open(Box::new(options)));
    }

    pub(super) fn remote_connection_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.suspend_connection_challenge(window, cx);
        remote::open(
            self.options.clone(),
            self.runtime.clone(),
            cx.weak_entity(),
            window,
            cx,
        );
    }
    pub(super) fn search_sessions(&self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::command::{Command as Palette, CommandItem, CommandState};
        let owner = cx.weak_entity();
        let entries = self.sessions.clone();
        let state = cx.new(|cx| CommandState::new(window, cx));
        let first = std::cell::Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                state.update(cx, |state, cx| state.focus(window, cx));
            }
            let entries2 = entries.clone();
            let owner = owner.clone();
            dialog.title("跳转到会话").child(
                Palette::new(&state)
                    .placeholder("会话标题…")
                    .items(entries.iter().map(|entry| {
                        CommandItem::new().label(format!("{} · {}", entry.title, entry.workspace))
                    }))
                    .on_confirm(move |index, window, cx| {
                        if let Some(entry) = entries2.get(index.row) {
                            window.close_dialog(cx);
                            let _ = owner
                                .update(cx, |view, cx| view.select_session(&entry.id, window, cx));
                        }
                    }),
            )
        });
    }
}

// The queue owns replies while a tab is hidden. Dialogs hold a clone so closing
// a modal for navigation never cancels authentication on another workspace.
#[derive(Clone)]
pub(super) struct ConnectionChallenge {
    prompt: String,
    confirm: bool,
    reply: std::rc::Rc<std::cell::RefCell<Option<tokio::sync::oneshot::Sender<Option<String>>>>>,
}

impl From<orbit_client::interaction::Challenge> for ConnectionChallenge {
    fn from(challenge: orbit_client::interaction::Challenge) -> Self {
        Self {
            prompt: challenge.prompt,
            confirm: challenge.confirm,
            reply: std::rc::Rc::new(std::cell::RefCell::new(Some(challenge.reply))),
        }
    }
}
impl ConnectionChallenge {
    fn pending(&self) -> bool {
        self.reply
            .borrow()
            .as_ref()
            .is_some_and(|reply| !reply.is_closed())
    }
    fn answer(&self, answer: Option<String>) -> bool {
        self.reply
            .borrow_mut()
            .take()
            .is_some_and(|reply| reply.send(answer).is_ok())
    }
}

impl Workspace {
    pub(super) fn connection_challenge(
        &mut self,
        challenge: orbit_client::interaction::Challenge,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.connection_challenges.push_back(challenge.into());
        self.show_connection_challenge(window, cx);
    }

    pub(super) fn suspend_connection_challenge(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.auth_dialog_open {
            self.auth_dialog_open = false;
            // Navigation may have opened another modal above authentication.
            // Never leave an old environment's authentication underneath it.
            window.close_all_dialogs(cx);
        }
    }

    pub(super) fn clear_connection_challenges(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for challenge in self.connection_challenges.drain(..) {
            challenge.answer(None);
        }
        self.suspend_connection_challenge(window, cx);
    }

    fn finish_connection_challenge(
        &mut self,
        challenge: &ConnectionChallenge,
        answer: Option<String>,
        close: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self
            .connection_challenges
            .front()
            .is_some_and(|current| std::rc::Rc::ptr_eq(&current.reply, &challenge.reply))
        {
            return;
        }
        self.connection_challenges.pop_front();
        challenge.answer(answer);
        self.auth_dialog_open = false;
        cx.emit(WorkspaceEvent::Changed);
        cx.notify();
        if close {
            window.close_dialog(cx);
        }
        // Native Escape/Confirm invokes its close request after the callback.
        cx.defer_in(window, |view, window, cx| {
            view.show_connection_challenge(window, cx)
        });
    }

    pub(super) fn show_connection_challenge(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.visible || self.auth_dialog_open {
            return;
        }
        while self
            .connection_challenges
            .front()
            .is_some_and(|challenge| !challenge.pending())
        {
            self.connection_challenges.pop_front();
        }
        let Some(challenge) = self.connection_challenges.front().cloned() else {
            return;
        };
        self.auth_dialog_open = true;
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("密码或密钥口令")
        });
        let owner = cx.weak_entity();
        let environment = self.environment();
        let first = std::cell::Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            let keyboard_owner = owner.clone();
            let keyboard_challenge = challenge.clone();
            let keyboard_input = input.clone();
            let close_owner = owner.clone();
            let close_challenge = challenge.clone();
            let cancel_owner = owner.clone();
            let cancel_challenge = challenge.clone();
            let answer_owner = owner.clone();
            let answer_challenge = challenge.clone();
            let answer_input = input.clone();
            let confirm = challenge.confirm;
            if first.replace(false) && !confirm {
                input.update(cx, |input, cx| input.focus(window, cx));
            }
            dialog
                .title(if confirm {
                    "验证 SSH 主机"
                } else {
                    "SSH 身份验证"
                })
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(environment.clone()),
                )
                .child(challenge.prompt.clone())
                .when(!confirm, |dialog| {
                    dialog.child(Input::new(&input).aria_label("密码或密钥口令"))
                })
                .on_ok(move |_, window, cx| {
                    let answer = if confirm {
                        "yes".into()
                    } else {
                        keyboard_input.read(cx).value().to_string()
                    };
                    let _ = keyboard_owner.update(cx, |view, cx| {
                        view.finish_connection_challenge(
                            &keyboard_challenge,
                            Some(answer),
                            false,
                            window,
                            cx,
                        )
                    });
                    true
                })
                .on_close(move |_, window, cx| {
                    let _ = close_owner.update(cx, |view, cx| {
                        view.finish_connection_challenge(&close_challenge, None, false, window, cx)
                    });
                })
                .footer(
                    h_flex()
                        .gap_2()
                        .child(Button::new("ssh-cancel").label("取消").on_click(
                            move |_, window, cx| {
                                let _ = cancel_owner.update(cx, |view, cx| {
                                    view.finish_connection_challenge(
                                        &cancel_challenge,
                                        None,
                                        true,
                                        window,
                                        cx,
                                    )
                                });
                            },
                        ))
                        .child(
                            Button::new("ssh-answer")
                                .primary()
                                .label(if confirm { "信任并连接" } else { "继续" })
                                .on_click(move |_, window, cx| {
                                    let answer = if confirm {
                                        "yes".into()
                                    } else {
                                        answer_input.read(cx).value().to_string()
                                    };
                                    let _ = answer_owner.update(cx, |view, cx| {
                                        view.finish_connection_challenge(
                                            &answer_challenge,
                                            Some(answer),
                                            true,
                                            window,
                                            cx,
                                        )
                                    });
                                }),
                        ),
                )
        });
    }
}

#[cfg(test)]
mod tests {
    use super::ConnectionChallenge;
    use orbit_client::interaction::Challenge;
    use tokio::sync::oneshot;

    #[test]
    fn hidden_authentication_survives_dialog_drop_and_answers_once() {
        let (reply, mut response) = oneshot::channel();
        let queued = ConnectionChallenge::from(Challenge {
            prompt: "test password".into(),
            confirm: false,
            reply,
        });
        let dialog = queued.clone();
        drop(dialog);
        assert!(matches!(
            response.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert!(queued.answer(Some("test-only-answer".into())));
        assert_eq!(
            response.try_recv().unwrap(),
            Some("test-only-answer".into())
        );
        assert!(!queued.answer(None));
    }

    #[test]
    fn cancelling_authentication_releases_queued_and_visible_replies() {
        let (reply, mut response) = oneshot::channel();
        let queued = ConnectionChallenge::from(Challenge {
            prompt: "trust test host".into(),
            confirm: true,
            reply,
        });
        let visible = queued.clone();
        assert!(queued.answer(None));
        assert_eq!(response.try_recv().unwrap(), None);
        assert!(!visible.pending());
        assert!(!visible.answer(Some("yes".into())));
    }
}
