//! Target-owned directory navigation, including SSH authentication. No remote
//! path is inspected through the desktop filesystem or interpolated by the UI.
use super::*;
use gpui_kit::component::{list::ListItem, spinner::Spinner};
use orbit_client::{directories::Listing, interaction, transport::Connection};

pub(super) struct Selected(pub(super) String);

pub(super) struct DirectoryBrowser {
    options: Options,
    runtime: Arc<tokio::runtime::Runtime>,
    connection: Option<Arc<Connection>>,
    progress: interaction::Progress,
    path: Entity<InputState>,
    password: Entity<InputState>,
    tree: Entity<TreeState>,
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

impl DirectoryBrowser {
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
        let password = cx.new(|cx| InputState::new(window, cx).masked(true));
        let path_subscription = cx.subscribe_in(&path, window, |view, path, event, _, cx| {
            if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
                view.load(path.read(cx).value().trim().to_owned(), cx);
            }
        });
        let password_subscription =
            cx.subscribe_in(&password, window, |view, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { shift: false, .. }) {
                    view.answer(true, window, cx);
                }
            });
        let (progress, mut updates) = interaction::channel();
        let update_task = cx.spawn(async move |owner, cx| {
            loop {
                tokio::select! {
                    changed = updates.stage.changed() => {
                        if changed.is_err() { break; }
                        let stage = updates.stage.borrow_and_update().clone();
                        if owner.update(cx, |view, cx| {
                            view.stage = stage.into();
                            cx.notify();
                        }).is_err() { break; }
                    }
                    challenge = updates.prompts.recv() => {
                        let Some(challenge) = challenge else { break };
                        if owner.update_in(cx, |view, window, cx| {
                            let first = view.challenges.is_empty();
                            view.challenges.push_back(challenge);
                            if first && !view.challenges.front().is_some_and(|item| item.confirm) {
                                view.password.update(cx, |input, cx| {
                                    input.set_value("", window, cx);
                                    input.focus(window, cx);
                                });
                            }
                            cx.notify();
                        }).is_err() { break; }
                    }
                }
            }
        });
        let mut browser = Self {
            options,
            runtime,
            connection: None,
            progress,
            path,
            password,
            tree: cx.new(|cx| TreeState::new(cx)),
            listing: None,
            busy: false,
            stage: "准备读取目录".into(),
            error: None,
            challenges: Default::default(),
            request: None,
            receive: Task::ready(()),
            _updates: update_task,
            _subscriptions: vec![path_subscription, password_subscription],
        };
        browser.load(initial, cx);
        browser
    }

    fn load(&mut self, path: String, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if path.is_empty() || path.chars().any(char::is_control) {
            self.error = Some("填写有效的目标目录。".into());
            cx.notify();
            return;
        }
        self.busy = true;
        self.error = None;
        let options = self.options.clone();
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
        self.receive = cx.spawn(async move |owner, cx| {
            let result = response
                .await
                .unwrap_or_else(|_| Err("目录读取已中断，请重试。".into()));
            let _ = owner.update_in(cx, |view, window, cx| {
                view.request = None;
                view.busy = false;
                view.challenges.clear();
                match result {
                    Ok((connection, listing)) => {
                        view.connection = Some(connection);
                        view.path.update(cx, |input, cx| {
                            input.set_value(listing.path.clone(), window, cx);
                        });
                        let items = listing
                            .directories
                            .iter()
                            .map(|name| TreeItem::new(join_path(&listing.path, name), name.clone()))
                            .collect::<Vec<_>>();
                        view.tree.update(cx, |tree, cx| tree.set_items(items, cx));
                        view.listing = Some(listing);
                    }
                    Err(error) => view.error = Some(error.into()),
                }
                cx.notify();
            });
        });
        cx.notify();
    }

    fn answer(&mut self, accepted: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(challenge) = self.challenges.pop_front() else {
            return;
        };
        let value = accepted.then(|| {
            if challenge.confirm {
                "yes".to_owned()
            } else {
                self.password.read(cx).value().to_string()
            }
        });
        let _ = challenge.reply.send(value);
        self.password.update(cx, |input, cx| {
            input.set_value("", window, cx);
            if !self.challenges.is_empty() {
                input.focus(window, cx);
            }
        });
        cx.notify();
    }
}

impl Drop for DirectoryBrowser {
    fn drop(&mut self) {
        if let Some(request) = &self.request {
            request.abort();
        }
    }
}

fn join_path(parent: &str, name: &str) -> String {
    format!("{}/{}", parent.trim_end_matches('/'), name)
}

fn parent_path(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit_once('/')
        .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
        .unwrap_or("/")
}

impl Render for DirectoryBrowser {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.weak_entity();
        let busy = self.busy;
        let current = self.listing.as_ref().map(|listing| listing.path.clone());
        let parent = current.as_deref().map(parent_path).map(str::to_owned);
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        div().flex_1().min_w_0().child(
                            Input::new(&self.path)
                                .aria_label("浏览目标目录")
                                .disabled(busy)
                                .w_full(),
                        ),
                    )
                    .child(
                        Button::new("remote-directory-go")
                            .label("前往")
                            .disabled(busy)
                            .on_click(cx.listener(|view, _, _, cx| {
                                let path = view.path.read(cx).value().trim().to_owned();
                                view.load(path, cx);
                            })),
                    ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("remote-directory-parent")
                            .ghost()
                            .small()
                            .icon(IconName::ArrowUp)
                            .label("上一级")
                            .disabled(busy || current.as_deref() == Some("/") || current.is_none())
                            .on_click(cx.listener(move |view, _, _, cx| {
                                if let Some(path) = &parent {
                                    view.load(path.clone(), cx);
                                }
                            })),
                    )
                    .child(
                        Button::new("remote-directory-home")
                            .ghost()
                            .small()
                            .icon(IconName::House)
                            .label("主目录")
                            .disabled(busy)
                            .on_click(cx.listener(|view, _, _, cx| view.load("~".into(), cx))),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("remote-directory-select")
                            .small()
                            .label("使用此目录")
                            .disabled(busy || current.is_none())
                            .on_click(cx.listener(move |_, _, _, cx| {
                                if let Some(path) = &current {
                                    cx.emit(Selected(path.clone()));
                                }
                            })),
                    ),
            )
            .when(busy, |view| {
                view.child(
                    h_flex().gap_2().child(Spinner::new().small()).child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(self.stage.clone()),
                    ),
                )
            })
            .when_some(self.challenges.front(), |view, challenge| {
                let confirm = challenge.confirm;
                view.child(
                    v_flex()
                        .gap_2()
                        .child(div().text_sm().child(challenge.prompt.clone()))
                        .when(!confirm, |view| {
                            view.child(Input::new(&self.password).aria_label("SSH 验证").w_full())
                        })
                        .child(
                            h_flex()
                                .gap_2()
                                .justify_end()
                                .child(
                                    Button::new("remote-auth-cancel")
                                        .label("取消验证")
                                        .on_click(cx.listener(|view, _, window, cx| {
                                            view.answer(false, window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("remote-auth-accept")
                                        .primary()
                                        .label(if confirm { "信任并继续" } else { "继续" })
                                        .on_click(cx.listener(|view, _, window, cx| {
                                            view.answer(true, window, cx)
                                        })),
                                ),
                        ),
                )
            })
            .when_some(self.error.clone(), |view, error| {
                view.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
            .when_some(self.listing.as_ref(), |view, listing| {
                view.child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("当前目录：{}", listing.path)),
                )
            })
            .when(
                self.challenges.is_empty()
                    && self
                        .listing
                        .as_ref()
                        .is_some_and(|listing| listing.directories.is_empty()),
                |view| {
                    view.child(
                        div()
                            .py_4()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("此目录没有子目录"),
                    )
                },
            )
            .when(
                self.challenges.is_empty()
                    && self
                        .listing
                        .as_ref()
                        .is_some_and(|listing| !listing.directories.is_empty()),
                |view| {
                    view.child(
                        div()
                            .h_40()
                            .min_h_0()
                            .capture_key_down(cx.listener(|view, event: &KeyDownEvent, _, cx| {
                                if event.keystroke.key == "enter" && !view.busy {
                                    let path = view
                                        .tree
                                        .read(cx)
                                        .selected_item()
                                        .map(|item| item.id.to_string());
                                    if let Some(path) = path {
                                        view.load(path, cx);
                                        cx.stop_propagation();
                                    }
                                }
                            }))
                            .child(Tree::new(&self.tree, move |_, entry, _, _, _| {
                                let path = entry.item().id.to_string();
                                let full_path = path.clone();
                                let owner = owner.clone();
                                ListItem::new(entry.item().id.clone())
                                    .accessibility_label(entry.item().label.clone())
                                    .disabled(busy)
                                    .h_8()
                                    .overflow_hidden()
                                    .child(
                                        h_flex()
                                            .w_full()
                                            .min_w_0()
                                            .gap_2()
                                            .child(
                                                Icon::new(IconName::Folder)
                                                    .size_4()
                                                    .flex_shrink_0(),
                                            )
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .text_ellipsis()
                                                    .child(entry.item().label.clone()),
                                            ),
                                    )
                                    .tooltip(move |window, cx| {
                                        gpui_kit::component::tooltip::Tooltip::new(
                                            full_path.clone(),
                                        )
                                        .build(window, cx)
                                    })
                                    .on_click(move |_, _, cx| {
                                        let _ = owner
                                            .update(cx, |view, cx| view.load(path.clone(), cx));
                                    })
                            })),
                    )
                },
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{join_path, parent_path};

    #[::core::prelude::v1::test]
    fn directory_navigation_preserves_target_names_and_root() {
        assert_eq!(join_path("/", "project '中文'"), "/project '中文'");
        assert_eq!(join_path("/home/dev/", "a b"), "/home/dev/a b");
        assert_eq!(parent_path("/home/dev/project"), "/home/dev");
        assert_eq!(parent_path("/home/"), "/");
        assert_eq!(parent_path("/"), "/");
    }
}
