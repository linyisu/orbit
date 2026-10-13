//! A remote target is configured here; the workbench owns the opened connection
//! and its retry lifecycle. Inputs remain intact until a valid target is submitted.
#[path = "remote/browser.rs"]
mod browser;
use super::*;
use gpui_kit::component::{
    input::{Input, InputState},
    radio::{Radio, RadioGroup},
    scroll::ScrollableElement,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Ssh,
    Wsl,
}

struct Draft {
    mode: Mode,
    target: String,
    workspace: String,
    user: String,
    port: String,
    identity: String,
    server: String,
    database: String,
}

impl Draft {
    fn options(&self, base: &Options) -> anyhow::Result<Options> {
        let target = self.target.trim();
        anyhow::ensure!(
            !target.is_empty(),
            match self.mode {
                Mode::Ssh => "填写 SSH 主机或配置别名",
                Mode::Wsl => "填写 WSL 发行版名称",
            }
        );
        anyhow::ensure!(
            !target.starts_with('-') && !target.chars().any(char::is_control),
            "连接目标不能以横线开头或包含控制字符"
        );
        if self.mode == Mode::Ssh {
            anyhow::ensure!(
                !target.chars().any(char::is_whitespace) && !target.contains("://"),
                "SSH 主机使用 Host 别名或 user@hostname，不包含空格或 URL 前缀"
            );
        }
        let path = self.workspace.trim();
        anyhow::ensure!(!path.is_empty(), "填写目标机器上的工作区目录");
        anyhow::ensure!(
            !path.chars().any(char::is_control),
            "工作区目录包含无效字符"
        );
        let user = optional(&self.user);
        if let Some(user) = &user {
            anyhow::ensure!(
                !user.starts_with('-')
                    && !user.chars().any(|c| c.is_whitespace() || c.is_control()),
                "用户名不能以横线开头或包含空格"
            );
        }
        let port = if self.mode == Mode::Ssh {
            optional(&self.port)
                .map(|port| {
                    port.parse::<u16>()
                        .ok()
                        .filter(|port| *port != 0)
                        .ok_or_else(|| anyhow::anyhow!("SSH 端口必须为 1–65535"))
                })
                .transpose()?
        } else {
            None
        };
        for (name, value) in [
            ("服务程序", &self.server),
            ("数据库路径", &self.database),
            ("密钥路径", &self.identity),
        ] {
            anyhow::ensure!(!value.chars().any(char::is_control), "{name}包含无效字符");
        }
        anyhow::ensure!(
            !matches!(optional(&self.server).as_deref(), Some("~"))
                && !self.server.trim().starts_with("~/"),
            "服务程序使用目标机器上的绝对路径，或 PATH 中的命令名"
        );
        let mut options = base.clone();
        options.ssh = (self.mode == Mode::Ssh).then(|| target.into());
        options.ssh_user = (self.mode == Mode::Ssh).then_some(user.clone()).flatten();
        options.ssh_port = port;
        options.identity = (self.mode == Mode::Ssh)
            .then(|| optional(&self.identity))
            .flatten();
        options.wsl = (self.mode == Mode::Wsl).then(|| target.into());
        options.wsl_user = (self.mode == Mode::Wsl).then_some(user).flatten();
        options.workspace = Some(path.into());
        options.server = optional(&self.server);
        options.database = optional(&self.database).map(PathBuf::from);
        options.resume = None;
        options.recover_draft = false;
        options.relink_on_resume = false;
        // Config and identity are local inputs. Workspace and database remain
        // target paths because the remote target is already set above.
        options.resolve_paths()?;
        Ok(options)
    }
}

fn optional(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

struct RemoteConnection {
    owner: WeakEntity<Workspace>,
    options: Options,
    runtime: Arc<tokio::runtime::Runtime>,
    mode: Mode,
    host: Entity<InputState>,
    distribution: Entity<InputState>,
    workspace: Entity<InputState>,
    ssh_user: Entity<InputState>,
    wsl_user: Entity<InputState>,
    port: Entity<InputState>,
    identity: Entity<InputState>,
    server: Entity<InputState>,
    database: Entity<InputState>,
    scroll: ScrollHandle,
    field_rows: std::collections::HashMap<EntityId, usize>,
    field_focus: std::collections::HashMap<EntityId, FocusHandle>,
    hosts: Vec<orbit_client::discovery::Host>,
    distributions: Vec<String>,
    discovering: bool,
    advanced: bool,
    submitted: bool,
    error: Option<SharedString>,
    browser: Option<Entity<browser::DirectoryBrowser>>,
    browser_subscription: Option<Subscription>,
    _subscriptions: Vec<Subscription>,
    _discovery: Task<()>,
}

pub(super) fn open(
    options: Options,
    runtime: Arc<tokio::runtime::Runtime>,
    owner: WeakEntity<Workspace>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let form = cx.new(|cx| RemoteConnection::new(options, runtime, owner.clone(), window, cx));
    let first = std::cell::Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        if first.replace(false) {
            form.update(cx, |view, cx| view.focus_target(window, cx));
        }
        dialog
            .title("打开远程工作区")
            .width(
                (window.rem_size() * 34.).min(window.bounds().size.width - window.rem_size() * 2.),
            )
            .child(form.clone())
            .on_ok({
                let form = form.clone();
                move |_, window, cx| {
                    form.update(cx, |view, cx| {
                        if view.browser.is_none() {
                            view.connect(window, cx);
                        }
                    });
                    false
                }
            })
            .on_close({
                let owner = owner.clone();
                move |_, window, cx| {
                    let _ = owner.update(cx, |_, cx| {
                        cx.defer_in(window, |view, window, cx| {
                            view.show_connection_challenge(window, cx)
                        });
                    });
                }
            })
    });
}

/// Browse the current target directly; changing hosts remains a separate command.
pub(super) fn choose_project_directory(
    options: Options,
    runtime: Arc<tokio::runtime::Runtime>,
    environment: String,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let browser = cx.new(|cx| browser::DirectoryBrowser::new(options, runtime, window, cx));
    let owner = cx.weak_entity();
    browser.update(cx, |_, cx| {
        cx.on_release(move |_, cx| {
            // Switching tabs can close all dialogs without calling on_close.
            // Release the picker guard for that programmatic dismissal too.
            cx.defer(move |cx| {
                let _ = owner.update(cx, |view, cx| {
                    view.choosing_project = false;
                    cx.notify();
                });
            });
        })
        .detach();
    });
    let selection = cx.subscribe_in(
        &browser,
        window,
        |view, _, event: &browser::Selected, window, cx| {
            view.choosing_project = false;
            window.close_dialog(cx);
            view.switch_project(event.0.clone().into(), None, false, false, window, cx);
        },
    );
    let owner = cx.weak_entity();
    let first = std::cell::Cell::new(true);
    window.open_dialog(cx, move |dialog, window, cx| {
        // Retain the subscription for exactly the dialog lifetime.
        let _selection = &selection;
        if first.replace(false) {
            browser.update(cx, |browser, cx| browser.focus(window, cx));
        }
        dialog
            .title("打开项目")
            .width(
                (window.rem_size() * 40.).min(window.bounds().size.width - window.rem_size() * 2.),
            )
            .child(
                v_flex()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(environment.clone()),
                    )
                    .child(browser.clone()),
            )
            .on_close({
                let owner = owner.clone();
                move |_, window, cx| {
                    let _ = owner.update(cx, |view, cx| {
                        view.choosing_project = false;
                        cx.notify();
                        cx.defer_in(window, |view, window, cx| {
                            view.show_connection_challenge(window, cx)
                        });
                    });
                }
            })
    });
}

impl RemoteConnection {
    fn new(
        options: Options,
        runtime: Arc<tokio::runtime::Runtime>,
        owner: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let remote = options.ssh.is_some() || options.wsl.is_some();
        let mut field = |value: String, placeholder: &'static str| {
            cx.new(|cx| {
                let mut input = InputState::new(window, cx).placeholder(placeholder);
                input.set_value(value, window, cx);
                input
            })
        };
        let host = field(
            options.ssh.clone().unwrap_or_default(),
            "dev-server 或 user@hostname",
        );
        let distribution = field(options.wsl.clone().unwrap_or_default(), "Ubuntu");
        let workspace = field(
            if remote {
                options
                    .workspace
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "~".into())
            } else {
                "~".into()
            },
            "~/workspace/project",
        );
        let ssh_user = field(
            options.ssh_user.clone().unwrap_or_default(),
            "使用 SSH 配置中的用户",
        );
        let wsl_user = field(
            options.wsl_user.clone().unwrap_or_default(),
            "使用发行版的默认用户",
        );
        let port = field(
            options
                .ssh_port
                .map(|port| port.to_string())
                .unwrap_or_default(),
            "使用 SSH 配置中的端口",
        );
        let identity = field(
            options.identity.clone().unwrap_or_default(),
            "~/.ssh/id_ed25519",
        );
        let server = field(
            if remote {
                options.server.clone().unwrap_or_default()
            } else {
                String::new()
            },
            "自动部署 Orbit 内置服务",
        );
        let database = field(
            if remote {
                options
                    .database
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned())
                    .unwrap_or_default()
            } else {
                String::new()
            },
            "使用目标机器的默认数据库",
        );
        let mode = if options.wsl.is_some() {
            Mode::Wsl
        } else {
            Mode::Ssh
        };
        let inputs = [
            &host,
            &distribution,
            &workspace,
            &ssh_user,
            &wsl_user,
            &port,
            &identity,
            &server,
            &database,
        ];
        let mut subscriptions: Vec<_> = inputs
            .into_iter()
            .map(|input| {
                cx.subscribe_in(input, window, |view, _, event, window, cx| match event {
                    InputEvent::Change => {
                        view.error = None;
                        view.browser = None;
                        view.browser_subscription = None;
                        cx.notify();
                    }
                    InputEvent::PressEnter { shift: false, .. } => view.connect(window, cx),
                    _ => {}
                })
            })
            .collect();
        let scroll = ScrollHandle::new();
        let mut field_focus = std::collections::HashMap::new();
        for input in inputs {
            let id = input.entity_id();
            // Input has separate frame and text focus handles. Listen on the
            // enclosing row so both Tab navigation and text focus are covered.
            let focus = cx.focus_handle().tab_stop(false);
            subscriptions.push(cx.on_focus_in(&focus, window, move |_, window, cx| {
                // Focus listeners run during drawing; defer the invalidation
                // so another frame consumes the requested scroll position.
                cx.defer_in(window, move |view, _, cx| {
                    if let Some(row) = view.field_rows.get(&id) {
                        view.scroll.scroll_to_item(*row);
                        cx.notify();
                    }
                });
            }));
            field_focus.insert(id, focus);
        }
        let advanced = remote
            && (options.ssh_user.is_some()
                || options.ssh_port.is_some()
                || options.identity.is_some()
                || options.wsl_user.is_some()
                || options.server.is_some()
                || options.database.is_some());
        let revision = orbit_client::discovery::revision();
        let (hosts, distributions) = {
            let _runtime = runtime.enter();
            orbit_client::discovery::snapshot()
        };
        let discovery = cx.spawn(async move |owner, cx| {
            let mut revision = revision;
            for _ in 0..14 {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                let next = orbit_client::discovery::revision();
                if next != revision {
                    revision = next;
                    let (hosts, distributions) = orbit_client::discovery::snapshot();
                    if owner
                        .update(cx, |view, cx| {
                            view.hosts = hosts;
                            view.distributions = distributions;
                            cx.notify();
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            }
            let _ = owner.update(cx, |view, cx| {
                view.discovering = false;
                cx.notify();
            });
        });
        Self {
            owner,
            options,
            runtime,
            mode,
            host,
            distribution,
            workspace,
            ssh_user,
            wsl_user,
            port,
            identity,
            server,
            database,
            scroll,
            field_rows: Default::default(),
            field_focus,
            hosts,
            distributions,
            discovering: true,
            advanced,
            submitted: false,
            error: None,
            browser: None,
            browser_subscription: None,
            _subscriptions: subscriptions,
            _discovery: discovery,
        }
    }

    fn focus_target(&self, window: &mut Window, cx: &mut Context<Self>) {
        let input = if self.mode == Mode::Ssh {
            &self.host
        } else {
            &self.distribution
        };
        input.update(cx, |input, cx| input.focus(window, cx));
    }

    fn draft(&self, cx: &App) -> Draft {
        let read = |input: &Entity<InputState>| input.read(cx).value().to_string();
        Draft {
            mode: self.mode,
            target: read(if self.mode == Mode::Ssh {
                &self.host
            } else {
                &self.distribution
            }),
            workspace: read(&self.workspace),
            user: read(if self.mode == Mode::Ssh {
                &self.ssh_user
            } else {
                &self.wsl_user
            }),
            port: read(&self.port),
            identity: read(&self.identity),
            server: read(&self.server),
            database: read(&self.database),
        }
    }

    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.submitted {
            return;
        }
        match self.draft(cx).options(&self.options) {
            Ok(options) => {
                if self
                    .owner
                    .update(cx, |_, cx| cx.emit(WorkspaceEvent::Open(Box::new(options))))
                    .is_ok()
                {
                    self.submitted = true;
                    window.close_dialog(cx);
                }
            }
            Err(error) => {
                self.error = Some(error.to_string().into());
                cx.notify();
            }
        }
    }

    fn browse(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.draft(cx).options(&self.options) {
            Ok(options) => {
                let browser = cx.new(|cx| {
                    browser::DirectoryBrowser::new(options, self.runtime.clone(), window, cx)
                });
                browser.update(cx, |view, cx| view.focus(window, cx));
                self.browser_subscription = Some(cx.subscribe_in(
                    &browser,
                    window,
                    |view, _, event: &browser::Selected, window, cx| {
                        view.workspace.update(cx, |input, cx| {
                            input.set_value(event.0.clone(), window, cx);
                            input.focus(window, cx);
                        });
                        view.browser = None;
                        view.browser_subscription = None;
                        cx.notify();
                    },
                ));
                self.browser = Some(browser);
                self.error = None;
            }
            Err(error) => self.error = Some(error.to_string().into()),
        }
        cx.notify();
    }

    fn field(&self, label: &'static str, input: &Entity<InputState>) -> impl IntoElement {
        v_flex()
            .gap_1p5()
            .child(div().text_sm().child(label))
            .child(Input::new(input).aria_label(label).w_full())
    }

    fn choose_identity(&mut self, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("选择 SSH 密钥".into()),
        });
        cx.spawn(async move |owner, cx| {
            if let Ok(Ok(Some(paths))) = prompt.await
                && let Some(path) = paths.first()
            {
                let value = path.to_string_lossy().into_owned();
                let _ = owner.update_in(cx, |view, window, cx| {
                    view.identity
                        .update(cx, |input, cx| input.set_value(value, window, cx));
                });
            }
        })
        .detach();
    }
}

impl Render for RemoteConnection {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.weak_entity();
        let mode = self.mode;
        let known: Vec<_> = if mode == Mode::Ssh {
            self.hosts
                .iter()
                .map(|host| {
                    let label = if host.hostname.is_empty() {
                        host.name.clone()
                    } else {
                        format!("{} · {}", host.name, host.hostname)
                    };
                    (host.name.clone(), label)
                })
                .collect()
        } else {
            self.distributions
                .iter()
                .map(|name| (name.clone(), name.clone()))
                .collect()
        };
        let target = if mode == Mode::Ssh {
            &self.host
        } else {
            &self.distribution
        };
        let target_input = target.clone();
        let target_controls = h_flex()
            .gap_2()
            .child(
                div().flex_1().min_w_0().child(
                    Input::new(target)
                        .aria_label(if mode == Mode::Ssh {
                            "SSH 主机"
                        } else {
                            "WSL 发行版"
                        })
                        .w_full(),
                ),
            )
            .when(!known.is_empty(), |row| {
                row.child(
                    Button::new("known-remotes")
                        .icon(IconName::ChevronDown)
                        .label("已配置")
                        .dropdown_menu(move |mut menu, _, _| {
                            for (value, label) in &known {
                                let value = value.clone();
                                let input = target_input.clone();
                                menu = menu.item(PopupMenuItem::new(label.clone()).on_click(
                                    move |_, window, cx| {
                                        input.update(cx, |input, cx| {
                                            input.set_value(value.clone(), window, cx)
                                        });
                                    },
                                ));
                            }
                            menu
                        }),
                )
            });
        let mut rows: Vec<AnyElement> = vec![
            RadioGroup::horizontal("remote-mode")
                .selected_index(Some(if mode == Mode::Ssh { 0 } else { 1 }))
                .child(Radio::new("remote-ssh").label("SSH"))
                .child(Radio::new("remote-wsl").label("WSL"))
                .on_change(cx.listener(|view, ix, window, cx| {
                    view.mode = if *ix == 0 { Mode::Ssh } else { Mode::Wsl };
                    view.error = None;
                    view.browser = None;
                    view.browser_subscription = None;
                    view.focus_target(window, cx);
                    cx.notify();
                }))
                .into_any_element(),
        ];
        let mut field_rows = std::collections::HashMap::new();
        // Both transport inputs occupy this row, including during a mode
        // change before the newly selected input has rendered.
        field_rows.insert(self.host.entity_id(), rows.len());
        field_rows.insert(self.distribution.entity_id(), rows.len());
        rows.push(
            v_flex()
                .gap_1p5()
                .child(div().text_sm().child(if mode == Mode::Ssh {
                    "SSH 主机"
                } else {
                    "WSL 发行版"
                }))
                .child(target_controls)
                .into_any_element(),
        );
        rows.push(
            div()
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(if mode == Mode::Ssh {
                    "读取本机 SSH 配置，支持跳板机、密钥和密码验证。目标系统需为 Linux。"
                } else {
                    "使用本机 wsl.exe 连接发行版，工作区路径使用 Linux 格式。"
                })
                .into_any_element(),
        );
        field_rows.insert(self.workspace.entity_id(), rows.len());
        rows.push(
            v_flex()
                .gap_1p5()
                .child(div().text_sm().child("工作区目录"))
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            div().flex_1().min_w_0().child(
                                Input::new(&self.workspace)
                                    .aria_label("工作区目录")
                                    .w_full(),
                            ),
                        )
                        .child(
                            Button::new("remote-browse")
                                .icon(IconName::FolderOpen)
                                .label(if self.browser.is_some() {
                                    "关闭浏览"
                                } else {
                                    "浏览目录"
                                })
                                .on_click(cx.listener(|view, _, window, cx| {
                                    if view.browser.is_some() {
                                        view.browser = None;
                                        view.browser_subscription = None;
                                        view.workspace.update(cx, |input, cx| {
                                            input.focus(window, cx);
                                        });
                                        cx.notify();
                                    } else {
                                        view.browse(window, cx);
                                    }
                                })),
                        ),
                )
                .when_some(self.browser.clone(), |view, browser| {
                    view.child(div().flex_shrink_0().pt_2().child(browser))
                })
                .into_any_element(),
        );
        rows.push(
            Button::new("remote-advanced")
                .ghost()
                .small()
                .icon(if self.advanced {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .label("连接选项")
                .on_click(cx.listener(|view, _, _, cx| {
                    view.advanced = !view.advanced;
                    cx.notify();
                }))
                .into_any_element(),
        );
        if self.advanced {
            let user = if mode == Mode::Ssh {
                &self.ssh_user
            } else {
                &self.wsl_user
            };
            field_rows.insert(user.entity_id(), rows.len());
            rows.push(self.field("用户（可选）", user).into_any_element());
            if mode == Mode::Ssh {
                field_rows.insert(self.port.entity_id(), rows.len());
                rows.push(self.field("端口（可选）", &self.port).into_any_element());
                field_rows.insert(self.identity.entity_id(), rows.len());
                rows.push(
                    v_flex()
                        .gap_1p5()
                        .child(div().text_sm().child("本机私钥（可选）"))
                        .child(
                            h_flex()
                                .gap_2()
                                .child(div().flex_1().min_w_0().child(
                                    Input::new(&self.identity).aria_label("本机私钥").w_full(),
                                ))
                                .child(
                                    Button::new("remote-key-file")
                                        .icon(IconName::FolderOpen)
                                        .tooltip("选择 SSH 密钥…")
                                        .on_click(
                                            cx.listener(|view, _, _, cx| view.choose_identity(cx)),
                                        ),
                                ),
                        )
                        .into_any_element(),
                );
            }
            field_rows.insert(self.server.entity_id(), rows.len());
            rows.push(
                self.field("目标服务程序（可选）", &self.server)
                    .into_any_element(),
            );
            field_rows.insert(self.database.entity_id(), rows.len());
            rows.push(
                self.field("目标数据库（可选）", &self.database)
                    .into_any_element(),
            );
            rows.push(div().text_xs().text_color(cx.theme().muted_foreground)
                .child("留空时自动部署服务，并使用目标机器的默认数据库。服务程序填写目标绝对路径或命令名；数据库支持 ~/。模型与凭据从本机配置同步。")
                .into_any_element());
        }
        let row_focus: std::collections::HashMap<_, _> = field_rows
            .iter()
            .filter_map(|(id, row)| self.field_focus.get(id).map(|focus| (*row, focus.clone())))
            .collect();
        self.field_rows = field_rows;
        v_flex()
            .gap_4()
            .child(
                v_flex()
                    .id("remote-form-scroll")
                    .gap_4()
                    .pr_2()
                    .max_h(window.bounds().size.height * 0.55)
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(rows.into_iter().enumerate().map(|(index, row)| {
                        div()
                            .flex_shrink_0()
                            .when_some(row_focus.get(&index), |row, focus| row.track_focus(focus))
                            .child(row)
                    }))
                    .vertical_scrollbar(&self.scroll),
            )
            .when_some(self.error.clone(), |view, error| {
                view.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
            .child(
                h_flex()
                    .gap_2()
                    .when(self.discovering, |row| {
                        row.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("读取本机连接配置…"),
                        )
                    })
                    .child(div().flex_1())
                    .child(
                        Button::new("remote-cancel")
                            .label("取消")
                            .on_click(|_, window, cx| window.close_dialog(cx)),
                    )
                    .child(
                        Button::new("remote-connect")
                            .primary()
                            .label("连接工作区")
                            .disabled(self.submitted)
                            .on_click(move |_, window, cx| {
                                let _ = owner.update(cx, |view, cx| view.connect(window, cx));
                            }),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{Draft, Mode, Options};
    use clap::Parser;

    fn draft(mode: Mode) -> Draft {
        Draft {
            mode,
            target: "dev".into(),
            workspace: "~/workspace/project '中文'".into(),
            user: "".into(),
            port: "".into(),
            identity: "".into(),
            server: "".into(),
            database: "".into(),
        }
    }

    #[test]
    fn remote_target_keeps_target_paths_and_drops_local_service_overrides() {
        let mut local = Options::parse_from(["orbit-gui"]);
        local.server = Some("/desktop/orbit".into());
        local.database = Some("/desktop/orbit.db".into());
        local.resume = Some("old-local-session".into());
        let options = draft(Mode::Ssh).options(&local).unwrap();
        assert_eq!(options.ssh.as_deref(), Some("dev"));
        assert_eq!(
            options.workspace.as_deref(),
            Some(std::path::Path::new("~/workspace/project '中文'"))
        );
        assert!(options.server.is_none() && options.database.is_none() && options.resume.is_none());
    }

    #[test]
    fn mode_change_clears_ssh_fields_and_validates_the_active_transport() {
        let local = Options::parse_from(["orbit-gui"]);
        let mut draft = draft(Mode::Ssh);
        for target in ["-oProxyCommand=bad", "ssh://dev", "dev host", "dev\nother"] {
            draft.target = target.into();
            assert!(draft.options(&local).is_err());
        }
        draft.target = "user@dev".into();
        for port in ["0", "65536", "ssh"] {
            draft.port = port.into();
            assert!(draft.options(&local).is_err());
        }
        draft.port = "2222".into();
        draft.identity = "~/.ssh/custom key".into();
        let ssh = draft.options(&local).unwrap();
        assert_eq!(ssh.ssh_port, Some(2222));
        draft.mode = Mode::Wsl;
        draft.target = "Ubuntu 24.04".into();
        draft.user = "linux-user".into();
        let wsl = draft.options(&ssh).unwrap();
        assert!(wsl.ssh.is_none() && wsl.ssh_port.is_none() && wsl.identity.is_none());
        assert_eq!(wsl.wsl_user.as_deref(), Some("linux-user"));
    }

    #[test]
    fn editing_remote_connection_keeps_explicit_target_overrides() {
        let base = Options::parse_from(["orbit-gui", "--ssh", "old-target"]);
        let mut draft = draft(Mode::Ssh);
        draft.target = " dev-alias ".into();
        draft.user = " builder ".into();
        draft.port = " 2222 ".into();
        draft.identity = "keys/dev key".into();
        draft.server = "/opt/orbit service/orbit".into();
        draft.database = "~/state/orbit.db".into();
        let options = draft.options(&base).unwrap();
        assert_eq!(options.ssh.as_deref(), Some("dev-alias"));
        assert_eq!(options.ssh_user.as_deref(), Some("builder"));
        assert_eq!(options.ssh_port, Some(2222));
        assert!(std::path::Path::new(options.identity.as_ref().unwrap()).is_absolute());
        assert_eq!(options.server.as_deref(), Some("/opt/orbit service/orbit"));
        assert_eq!(
            options.database.as_deref(),
            Some(std::path::Path::new("~/state/orbit.db"))
        );
        draft.server = "~/bin/orbit".into();
        assert!(
            draft
                .options(&base)
                .err()
                .unwrap()
                .to_string()
                .contains("绝对路径")
        );
        draft.server.clear();
        for field in ["", " ", "\n"] {
            draft.workspace = field.into();
            assert!(draft.options(&base).is_err());
        }
    }
}
