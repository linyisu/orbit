//! Target-owned directory navigation, including SSH authentication. No remote
//! path is inspected through the desktop filesystem or interpolated by the UI.
use super::*;
use gpui_kit::component::spinner::Spinner;
use orbit_client::{directories::Listing, interaction, transport::Connection};

pub(super) struct Selected(pub(super) String);

pub(super) struct DirectoryBrowser {
    options: Option<Options>,
    runtime: Arc<tokio::runtime::Runtime>,
    connection: Option<Arc<Connection>>,
    progress: interaction::Progress,
    path: Entity<InputState>,
    password: Entity<InputState>,
    choices: Entity<choices::ChoiceInput>,
    embedded: bool,
    listing: Option<Listing>,
    busy: bool,
    stage: SharedString,
    error: Option<SharedString>,
    challenges: std::collections::VecDeque<interaction::Challenge>,
    request: Option<tokio::task::JoinHandle<()>>,
    receive: Task<()>,
    _updates: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<Selected> for DirectoryBrowser {}
impl EventEmitter<choices::ChoiceEvent> for DirectoryBrowser {}

impl DirectoryBrowser {
    pub(super) fn is_open(&self, cx: &App) -> bool {
        self.choices.read(cx).is_open()
    }

    pub(super) fn new(
        options: Options,
        runtime: Arc<tokio::runtime::Runtime>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let initial = options
            .workspace
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| "~".into());
        let path = cx.new(|cx| {
            let mut input = InputState::new(window, cx).placeholder("目标机器上的目录");
            input.set_value(initial.clone(), window, cx);
            input
        });
        let mut browser = Self::picker(runtime, path, window, cx);
        browser.options = Some(options);
        browser.embedded = false;
        browser.load(initial, cx);
        browser
    }

    /// Share the form's path input; defer remote I/O until the dropdown is opened.
    pub(super) fn picker(
        runtime: Arc<tokio::runtime::Runtime>,
        path: Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let choices = cx.new(|cx| {
            choices::ChoiceInput::new(
                path.clone(),
                "工作区目录",
                choices::ChoiceKind::Directory,
                window,
                cx,
            )
        });
        let status = cx.weak_entity();
        choices.update(cx, |choices, cx| {
            choices.set_items(
                vec![choices::Choice {
                    value: "~/".into(),
                    label: "~/".into(),
                }],
                window,
                cx,
            );
            choices.set_footer(std::rc::Rc::new(move |window, cx| {
                status
                    .upgrade()
                    .map(|owner| {
                        owner
                            .read(cx)
                            .status(owner.downgrade(), window, cx)
                            .into_any_element()
                    })
                    .unwrap_or_else(|| div().into_any_element())
            }));
        });
        let choice_events =
            cx.subscribe_in(&choices, window, |view, _, event, _, cx| match event {
                choices::ChoiceEvent::Selected(path) => view.load(path.clone(), cx),
                choices::ChoiceEvent::Browse | choices::ChoiceEvent::Submit if !view.embedded => {
                    view.load(
                        view.path
                            .read(cx)
                            .value()
                            .trim()
                            .to_owned(),
                        cx,
                    );
                }
                choices::ChoiceEvent::Browse => cx.emit(choices::ChoiceEvent::Browse),
                choices::ChoiceEvent::Submit => cx.emit(choices::ChoiceEvent::Submit),
            });
        let password = cx.new(|cx| InputState::new(window, cx).masked(true));
        let password_subscription =
            cx.subscribe_in(&password, window, |view, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
                    view.answer(true, window, cx);
                }
            });
        let (progress, update_task) = Self::interaction(cx);
        Self {
            options: None,
            runtime,
            connection: None,
            progress,
            path,
            password,
            choices,
            embedded: true,
            listing: None,
            busy: false,
            stage: "准备读取目录".into(),
            error: None,
            challenges: Default::default(),
            request: None,
            receive: Task::ready(()),
            _updates: update_task,
            _subscriptions: vec![choice_events, password_subscription],
        }
    }

    fn interaction(cx: &mut Context<Self>) -> (interaction::Progress, Task<()>) {
        let (progress, mut updates) = interaction::channel();
        let update_task = cx.spawn(async move |owner, cx| {
            loop {
                tokio::select! {
                    changed = updates.stage.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        let stage = updates.stage.borrow_and_update().clone();
                        if owner.update(cx, |view, cx| {
                            view.stage = stage.into();
                            view.refresh_status(cx);
                        }).is_err() {
                            break;
                        }
                    }
                    challenge = updates.prompts.recv() => {
                        let Some(challenge) = challenge else {
                            break;
                        };
                        if owner.update_in(cx, |view, window, cx| {
                            let first = view.challenges.is_empty();
                            view.challenges.push_back(challenge);
                            if first && !view.challenges.front().is_some_and(|item| item.confirm) {
                                view.password.update(cx, |input, cx| {
                                    input.set_value("", window, cx);
                                    input.focus(window, cx);
                                });
                            }
                            view.refresh_status(cx);
                        }).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        (progress, update_task)
    }

    /// Cancel the pending listing and discard its authentication channel.
    fn cancel_listing(&mut self, cx: &mut Context<Self>) {
        if let Some(request) = self.request.take() {
            request.abort();
        }
        self.receive = Task::ready(());
        self.busy = false;
        self.challenges.clear();
        let (progress, updates) = Self::interaction(cx);
        self.progress = progress;
        self._updates = updates;
    }

    /// Replace target credentials before listing, cancelling work belonging to the old target.
    pub(super) fn browse(&mut self, options: Options, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .options
            .as_ref()
            .is_none_or(|current| !current.same_host(&options))
        {
            self.invalidate(window, cx);
        }
        self.options = Some(options);
        self.load(
            self.path
                .read(cx)
                .value()
                .trim()
                .to_owned(),
            cx,
        );
    }

    pub(super) fn invalidate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_listing(cx);
        self.options = None;
        self.connection = None;
        self.listing = None;
        self.challenges.clear();
        self.error = None;
        self.choices.update(cx, |choices, cx| {
            choices.set_items(
                vec![choices::Choice {
                    value: "~/".into(),
                    label: "~/".into(),
                }],
                window,
                cx,
            )
        });
        self.refresh_status(cx);
    }

    fn refresh_status(&self, cx: &mut Context<Self>) {
        self.choices
            .update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    pub(super) fn focus(&self, window: &mut Window, cx: &mut App) {
        self.path
            .update(cx, |input, cx| input.focus(window, cx));
    }

    fn load(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(options) = self.options.clone() else {
            return;
        };
        if self.request.is_some() {
            self.cancel_listing(cx);
            self.connection = None;
        }
        if path.is_empty() || path.chars().any(char::is_control) {
            self.error = Some("填写有效的目标目录。".into());
            self.refresh_status(cx);
            return;
        }
        self.busy = true;
        self.error = None;
        let requested = path.clone();
        let connection = self.connection.clone();
        let progress = self.progress.clone();
        let (result, response) = tokio::sync::oneshot::channel();
        self.request = Some(self.runtime.spawn(async move {
            let result_value = async {
                let connection = match connection {
                    Some(connection) => connection,
                    None => {
                        let transport = options.connection_options()?;
                        orbit_client::prepare_connection(
                            &transport,
                            options.config,
                            options.database,
                            options.auth_dir,
                            Some(progress.clone()),
                        )
                        .await?
                    }
                };
                let listing =
                    orbit_client::directories::list(&connection, &path, &progress).await?;
                Ok::<_, anyhow::Error>((connection, listing))
            }
            .await
            .map_err(|error| format!("{error:#}"));
            let _ = result.send(result_value);
        }));
        self.receive =
            cx.spawn(async move |owner, cx| {
                let result = response
                    .await
                    .unwrap_or_else(|_| Err("目录读取已中断，请重试。".into()));
                let _ =
                    owner.update_in(cx, |view, window, cx| {
                        view.request = None;
                        view.busy = false;
                        view.challenges.clear();
                        match result {
                            Ok((connection, listing)) => {
                                view.connection = Some(connection);
                                if view.path.read(cx).value().as_ref() == requested {
                                    view.path.update(cx, |input, cx| {
                                        input.set_value(listing.path.clone(), window, cx)
                                    });
                                }
                                let mut items = vec![choices::Choice {
                                    value: "~/".into(),
                                    label: "~/".into(),
                                }];
                                if let Some(parent) = listing.parent_path() {
                                    items.push(choices::Choice {
                                        value: parent.into(),
                                        label: "../".into(),
                                    });
                                }
                                items.extend(listing.directories.iter().map(|name| {
                                    choices::Choice {
                                        value: listing.directory_path(name),
                                        label: format!("{name}/"),
                                    }
                                }));
                                view.choices
                                    .update(cx, |choices, cx| choices.set_items(items, window, cx));
                                view.listing = Some(listing);
                            }
                            Err(error) => view.error = Some(error.into()),
                        }
                        view.refresh_status(cx);
                    });
            });
        self.refresh_status(cx);
    }

    fn answer(&mut self, accepted: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(challenge) = self.challenges.pop_front() else {
            return;
        };
        let value = accepted.then(|| {
            if challenge.confirm {
                "yes".to_owned()
            } else {
                self.password
                    .read(cx)
                    .value()
                    .to_string()
            }
        });
        let _ = challenge.reply.send(value);
        let needs_password = self
            .challenges
            .front()
            .is_some_and(|item| !item.confirm);
        self.password.update(cx, |input, cx| {
            input.set_value("", window, cx);
            if needs_password {
                input.focus(window, cx);
            }
        });
        if !needs_password {
            self.path
                .update(cx, |input, cx| input.focus(window, cx));
        }
        self.refresh_status(cx);
    }
}

impl Drop for DirectoryBrowser {
    fn drop(&mut self) {
        if let Some(request) = &self.request {
            request.abort();
        }
    }
}

impl DirectoryBrowser {
    fn status(&self, owner: WeakEntity<Self>, _: &mut Window, cx: &App) -> AnyElement {
        if !self.busy && self.error.is_none() && self.challenges.is_empty() {
            return div().into_any_element();
        }
        v_flex()
            .gap_2()
            .p_2()
            .when(self.busy, |panel| {
                panel.child(
                    h_flex()
                        .gap_2()
                        .child(Spinner::new().small())
                        .child(
                            div()
                                .text_sm()
                                .child(self.stage.clone()),
                        ),
                )
            })
            .when_some(self.error.clone(), |panel, error| {
                panel.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(orbit_client::directories::display_diagnostic(&error).into_owned()),
                )
            })
            .when_some(self.challenges.front(), |panel, challenge| {
                let confirm = challenge.confirm;
                let cancel = owner.clone();
                let accept = owner.clone();
                panel.child(
                    v_flex()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .child(challenge.prompt.clone()),
                        )
                        .when(!confirm, |panel| {
                            panel.child(
                                Input::new(&self.password)
                                    .aria_label("SSH 验证")
                                    .w_full(),
                            )
                        })
                        .child(
                            h_flex()
                                .gap_2()
                                .justify_end()
                                .child(
                                    Button::new("remote-auth-cancel")
                                        .label("取消验证")
                                        .on_click(move |_, window, cx| {
                                            let _ = cancel.update(cx, |view, cx| {
                                                view.answer(false, window, cx)
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("remote-auth-accept")
                                        .primary()
                                        .label(if confirm { "信任并继续" } else { "继续" })
                                        .on_click(move |_, window, cx| {
                                            let _ = accept.update(cx, |view, cx| {
                                                view.answer(true, window, cx)
                                            });
                                        }),
                                ),
                        ),
                )
            })
            .into_any_element()
    }
}

impl Render for DirectoryBrowser {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .gap_3()
            .child(self.choices.clone())
            .when(!self.embedded, |panel| {
                panel.child(
                    h_flex().justify_end().child(
                        Button::new("remote-directory-select")
                            .label("使用此目录")
                            .disabled(self.busy || self.listing.is_none())
                            .on_click(cx.listener(|view, _, _, cx| {
                                cx.emit(Selected(view.path.read(cx).value().to_string()));
                            })),
                    ),
                )
            })
    }
}
