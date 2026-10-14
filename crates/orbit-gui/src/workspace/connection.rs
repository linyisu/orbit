use super::*;
use gpui_kit::component::input::{Editor, EditorState, Input, InputState};

#[path = "remote.rs"]
mod remote;

/// Diagnostics belong to one connection attempt and never contain askpass
/// replies. Keep the retained, filtered text after a failed connection.
pub(super) struct ConnectionLogs {
    environment: String,
    lines: Vec<String>,
    secrets: Vec<String>,
    displayed: String,
    editor: Entity<EditorState>,
}

impl ConnectionLogs {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            environment: String::new(),
            lines: Vec::new(),
            secrets: Vec::new(),
            displayed: String::new(),
            editor: cx.new(|cx| {
                EditorState::new(window, cx)
                    .language("plaintext")
                    .line_number(false)
                    .soft_wrap(true)
            }),
        }
    }

    pub(super) fn reset(
        &mut self,
        environment: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.environment = environment;
        self.secrets.clear();
        self.sync(Vec::new(), window, cx);
    }

    pub(super) fn sync(&mut self, lines: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.lines = lines
            .into_iter()
            .map(|line| filtered_log(&line, &self.secrets))
            .collect();
        let displayed = self.lines.join("\n");
        if displayed != self.displayed {
            self.editor.update(cx, |editor, cx| {
                editor.set_value(displayed.clone(), window, cx)
            });
            self.displayed = displayed;
        }
        cx.notify();
    }

    fn record_secret(&mut self, secret: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !secret.is_empty()
            && !self
                .secrets
                .iter()
                .any(|stored| stored == secret)
        {
            self.secrets.push(secret.to_owned());
            self.sync(self.lines.clone(), window, cx);
        }
    }
}

fn filtered_log(line: &str, secrets: &[String]) -> String {
    let lower = line.to_ascii_lowercase();
    let compact: String = lower
        .chars()
        .filter(|character| !character.is_ascii_whitespace())
        .collect();
    if [
        "password=",
        "password:",
        "passphrase=",
        "passphrase:",
        "token=",
        "api_key=",
        "api-key:",
        "authorization:",
        "secret=",
    ]
    .iter()
    .any(|key| compact.contains(key))
        || lower.contains("bearer ")
    {
        return "[含凭据的日志已隐藏]".into();
    }
    let mut filtered = line.to_owned();
    for secret in secrets
        .iter()
        .filter(|secret| !secret.is_empty())
    {
        filtered = filtered.replace(secret, "[已隐藏凭据]");
    }
    filtered
}

impl Render for ConnectionLogs {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .min_w_0()
            .gap_3()
            .on_action(|_: &gpui_kit::component::input::Escape, window, cx| {
                window.dispatch_action(Box::new(gpui_kit::base::actions::Cancel), cx)
            })
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .whitespace_normal()
                    .child(self.environment.clone()),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(if self.lines.is_empty() {
                        "暂无连接日志。连接失败时会保留已收到的诊断。".to_owned()
                    } else {
                        format!("最近 {} 条诊断 · 每次重新连接重新记录", self.lines.len())
                    }),
            )
            .child(
                Editor::new(&self.editor)
                    .readonly(true)
                    .h(window.viewport_size().height * 0.5)
                    .aria_label("连接日志，只读，可选择复制"),
            )
    }
}

impl Workspace {
    pub(super) fn open_connection_logs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let panel = self.connection_logs.clone();
        window.open_sheet(cx, move |sheet, window, _| {
            let copy = panel.clone();
            sheet
                .title("连接日志")
                .size(
                    (window.rem_size() * 48.)
                        .min((window.viewport_size().width - window.rem_size() * 2.).max(px(0.))),
                )
                .child(panel.clone())
                .footer(
                    h_flex()
                        .w_full()
                        .flex_wrap()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("copy-connection-logs")
                                .small()
                                .label("复制日志")
                                .on_click(move |_, _, cx| {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        copy.read(cx).displayed.clone(),
                                    ));
                                }),
                        )
                        .child(
                            Button::new("close-connection-logs")
                                .small()
                                .ghost()
                                .label("关闭")
                                .on_click(|_, window, cx| {
                                    window.dispatch_action(
                                        Box::new(gpui_kit::base::actions::Cancel),
                                        cx,
                                    )
                                }),
                        ),
                )
        });
    }

    pub(super) fn open_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.choosing_project || self.opening {
            return;
        }
        if self.options.ssh.is_some() || self.options.wsl.is_some() {
            self.suspend_connection_challenge(window, cx);
            self.choosing_project = true;
            remote::choose_project_directory(
                self.options.clone(),
                self.runtime.clone(),
                self.environment(),
                window,
                cx,
            );
            cx.notify();
        } else {
            self.choose_project_directory(None, cx);
        }
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let options = project_options(&self.options, path, resume, local, relink);
        self.open_target(options, window, cx);
    }

    /// Retarget an unsent conversation in place so its text and local attachments survive.
    pub(super) fn open_target(
        &mut self,
        options: Options,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if options.resume.is_none() && self.is_new_conversation(cx) && self.pending.is_none() {
            self.location = options
                .workspace
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default()
                .into();
            self.options = options;
            self.info = None;
            self.sessions.clear();
            self.connect(window, cx);
            cx.emit(WorkspaceEvent::Changed);
        } else {
            cx.emit(WorkspaceEvent::Open(Box::new(options)));
        }
    }

    pub(super) fn choose_platform(
        &mut self,
        wsl: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.suspend_connection_challenge(window, cx);
        remote::open(
            self.options.clone(),
            self.runtime.clone(),
            cx.weak_entity(),
            Some(wsl),
            window,
            cx,
        );
    }

    pub(super) fn remote_connection_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.suspend_connection_challenge(window, cx);
        remote::open(
            self.options.clone(),
            self.runtime.clone(),
            cx.weak_entity(),
            None,
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
        crate::dialog::open(window, cx, move |dialog, window, cx| {
            if first.replace(false) {
                state.update(cx, |state, cx| state.focus(window, cx));
            }
            let entries2 = entries.clone();
            let owner = owner.clone();
            dialog.title("跳转到会话").child(
                Palette::new(&state)
                    .bordered(false)
                    .placeholder("搜索会话标题或目录…")
                    .max_h(crate::dialog::body_height(window))
                    .items(entries.iter().map(|entry| {
                        let title = entry.title.clone();
                        let path = entry.workspace.clone();
                        CommandItem::new()
                            .label(title.clone())
                            .keywords([path.clone()])
                            .child(move |_, cx| {
                                v_flex()
                                    .min_w_0()
                                    .w_full()
                                    .gap_1()
                                    .py_1()
                                    .child(div().truncate().child(title.clone()))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .truncate()
                                            .child(path.clone()),
                                    )
                            })
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

/// A directory choice changes the project, not the selected host. The separate
/// local command is the only path that deliberately clears remote transport.
fn project_options(
    current: &Options,
    path: PathBuf,
    resume: Option<String>,
    local: bool,
    relink: bool,
) -> Options {
    let mut options = current.clone();
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
    options
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
        self.connection_challenges
            .push_back(challenge.into());
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
        if !challenge.confirm
            && let Some(answer) = &answer
        {
            self.connection_logs
                .update(cx, |logs, cx| logs.record_secret(answer, window, cx));
        }
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
        let Some(challenge) = self
            .connection_challenges
            .front()
            .cloned()
        else {
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
        crate::dialog::open(window, cx, move |dialog, window, cx| {
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
                        keyboard_input
                            .read(cx)
                            .value()
                            .to_string()
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
                        .child(
                            Button::new("ssh-cancel")
                                .label("取消")
                                .on_click(move |_, window, cx| {
                                    let _ = cancel_owner.update(cx, |view, cx| {
                                        view.finish_connection_challenge(
                                            &cancel_challenge,
                                            None,
                                            true,
                                            window,
                                            cx,
                                        )
                                    });
                                }),
                        )
                        .child(
                            Button::new("ssh-answer")
                                .primary()
                                .label(if confirm { "信任并连接" } else { "继续" })
                                .on_click(move |_, window, cx| {
                                    let answer = if confirm {
                                        "yes".into()
                                    } else {
                                        answer_input
                                            .read(cx)
                                            .value()
                                            .to_string()
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
    use super::{ConnectionChallenge, Options, filtered_log, project_options};
    use clap::Parser;
    use orbit_client::interaction::Challenge;
    use tokio::sync::oneshot;

    #[test]
    fn opening_another_remote_project_preserves_ssh_and_wsl_targets() {
        for args in [
            vec![
                "orbit-gui",
                "--ssh",
                "dev-alias",
                "--ssh-user",
                "builder",
                "--ssh-port",
                "2222",
                "--identity",
                "/desktop/keys/dev",
            ],
            vec!["orbit-gui", "--wsl", "Ubuntu", "--wsl-user", "builder"],
        ] {
            let mut current = Options::parse_from(args);
            current.workspace = Some("/remote/old".into());
            current.server = Some("/remote/bin/orbit-workspace-server".into());
            current.database = Some("~/state/orbit.db".into());
            current.resume = Some("previous-session".into());
            current.recover_draft = true;
            current.relink_on_resume = true;
            let selected = project_options(
                &current,
                "/remote/project '中文'".into(),
                None,
                false,
                false,
            );
            assert!(current.same_host(&selected));
            assert_eq!(
                selected.workspace.as_deref(),
                Some(std::path::Path::new("/remote/project '中文'"))
            );
            assert!(selected.resume.is_none());
            assert!(!selected.recover_draft && !selected.relink_on_resume);
            assert_eq!(selected.identity, current.identity);
            assert_eq!(selected.wsl_user, current.wsl_user);
        }
    }

    #[test]
    fn explicit_local_project_clears_remote_target_and_its_service_paths() {
        let mut current = Options::parse_from([
            "orbit-gui",
            "--ssh",
            "dev",
            "--ssh-user",
            "builder",
            "--ssh-port",
            "2222",
            "--identity",
            "/desktop/key",
        ]);
        current.server = Some("/remote/bin/orbit".into());
        current.database = Some("/remote/state.db".into());
        let selected = project_options(&current, "/local/project".into(), None, true, false);
        assert!(selected.ssh.is_none() && selected.ssh_user.is_none());
        assert!(selected.ssh_port.is_none() && selected.identity.is_none());
        assert!(selected.wsl.is_none() && selected.wsl_user.is_none());
        assert!(selected.server.is_none() && selected.database.is_none());
        assert_eq!(selected.config, current.config);
        assert_eq!(selected.auth_dir, current.auth_dir);
    }

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

    #[test]
    fn copied_diagnostics_hide_authentication_answers_and_credential_fields() {
        let secrets = vec!["test-only-password".to_owned()];
        assert_eq!(
            filtered_log("echo test-only-password", &secrets),
            "echo [已隐藏凭据]"
        );
        assert_eq!(
            filtered_log("Authorization: Bearer test-value", &[]),
            "[含凭据的日志已隐藏]"
        );
        assert_eq!(
            filtered_log("PASSWORD=test-value", &[]),
            "[含凭据的日志已隐藏]"
        );
        assert_eq!(
            filtered_log("password = test-value", &[]),
            "[含凭据的日志已隐藏]"
        );
        assert_eq!(
            filtered_log("Permission denied (publickey,password).", &secrets),
            "Permission denied (publickey,password)."
        );
        assert_eq!(filtered_log("已完成握手", &[String::new()]), "已完成握手");
    }
}
