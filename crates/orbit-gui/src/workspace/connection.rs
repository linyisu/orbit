use super::*;
use gpui_kit::component::input::{Input, InputState};

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
            options.ssh = None;
            options.wsl = None;
        }
        options.workspace = Some(path);
        options.resume = resume;
        options.recover_draft = false;
        options.relink_on_resume = relink;
        cx.emit(WorkspaceEvent::Open(Box::new(options)));
    }

    pub(super) fn remote_connection_dialog(&self, window: &mut Window, cx: &mut Context<Self>) {
        let path = self
            .options
            .workspace
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.location.to_string());
        let fields = [
            ("工作区目录", path),
            ("SSH 主机", self.options.ssh.clone().unwrap_or_default()),
            ("WSL 发行版", self.options.wsl.clone().unwrap_or_default()),
        ];
        let inputs = fields
            .iter()
            .map(|(_, value)| {
                cx.new(|cx| {
                    let mut state = InputState::new(window, cx);
                    state.set_value(value.clone(), window, cx);
                    state
                })
            })
            .collect::<Vec<_>>();
        let owner = cx.weak_entity();
        window.open_dialog(cx, move |dialog, _, _| {
            let inputs2 = inputs.clone();
            let owner = owner.clone();
            dialog
                .title("打开远程项目")
                .child(v_flex().gap_3().children(fields.iter().zip(&inputs).map(
                    |((label, _), input)| v_flex().gap_1().child(*label).child(Input::new(input)),
                )))
                .footer(
                    Button::new("connect-project")
                        .primary()
                        .label("连接")
                        .on_click(move |_, window, cx| {
                            let values = inputs2
                                .iter()
                                .map(|input| input.read(cx).value().trim().to_string())
                                .collect::<Vec<_>>();
                            if values[0].is_empty()
                                || (values[1].is_empty() == values[2].is_empty())
                            {
                                window.push_notification("填写目录，并只选择一种远程连接方式", cx);
                                return;
                            }
                            let _ = owner.update(cx, |view, cx| {
                                let mut options = view.options.clone();
                                options.ssh = (!values[1].is_empty()).then(|| values[1].clone());
                                options.wsl = (!values[2].is_empty()).then(|| values[2].clone());
                                options.workspace = Some(PathBuf::from(&values[0]));
                                options.resume = None;
                                options.recover_draft = false;
                                options.relink_on_resume = false;
                                cx.emit(WorkspaceEvent::Open(Box::new(options)));
                            });
                            window.close_dialog(cx);
                        }),
                )
        });
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

impl Workspace {
    pub(super) fn connection_challenge(
        &self,
        challenge: orbit_client::interaction::Challenge,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let input = cx.new(|cx| InputState::new(window, cx).masked(true));
        let sender = std::rc::Rc::new(std::cell::RefCell::new(Some(challenge.reply)));
        let on_close = sender.clone();
        window.open_dialog(cx, move |dialog, _, _| {
            let sender = sender.clone();
            let input2 = input.clone();
            let confirm = challenge.confirm;
            let on_close = on_close.clone();
            dialog
                .title("SSH 验证")
                .child(challenge.prompt.clone())
                .when(!confirm, |dialog| dialog.child(Input::new(&input)))
                .on_close(move |_, _, _| {
                    if let Some(sender) = on_close.borrow_mut().take() {
                        let _ = sender.send(None);
                    }
                })
                .footer(
                    Button::new("ssh-answer")
                        .primary()
                        .label(if confirm { "信任并连接" } else { "继续" })
                        .on_click(move |_, window, cx| {
                            if let Some(sender) = sender.borrow_mut().take() {
                                let _ = sender.send(Some(if confirm {
                                    "yes".into()
                                } else {
                                    input2.read(cx).value().to_string()
                                }));
                            }
                            window.close_dialog(cx);
                        }),
                )
        });
    }
}
