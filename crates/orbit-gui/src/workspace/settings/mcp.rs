//! MCP configuration belongs to the client configuration, even for remote sessions.
use super::super::*;
use gpui_kit::component::{
    button::ButtonVariant,
    checkbox::Checkbox,
    dialog::DialogButtonProps,
    input::{Input, InputState, NumberInput},
    switch::Switch,
};
use orbit_config::config::connections::mcp::McpDraft;
use orbit_protocol::resources::{Action, McpServer, Snapshot};
use orbit_tools::mcp::McpServerConfig;

pub(super) struct Mcp {
    owner: WeakEntity<Workspace>,
    session: Option<String>,
    workspace: String,
    path: PathBuf,
    runtime: Arc<tokio::runtime::Runtime>,
    draft: Option<McpDraft>,
    name: Entity<InputState>,
    command: Entity<InputState>,
    args: Entity<TextareaState>,
    env: Entity<TextareaState>,
    timeout: Entity<InputState>,
    tools: Entity<TextareaState>,
    editing: bool,
    existing: Option<String>,
    template: Option<McpServerConfig>,
    enabled: bool,
    busy: bool,
    error: Option<String>,
    saved: bool,
    base_reloaded: bool,
    task: Task<()>,
    statuses: Option<Vec<McpServer>>,
    expanded: std::collections::HashSet<String>,
    status_busy: bool,
    status_error: Option<String>,
    status_task: Task<()>,
}
impl Mcp {
    pub(super) fn has_pending_work(&self) -> bool {
        self.busy || self.editing
    }

    pub(super) fn new(
        options: &Options,
        runtime: Arc<tokio::runtime::Runtime>,
        owner: WeakEntity<Workspace>,
        session: Option<String>,
        workspace: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self {
            owner,
            session,
            workspace,
            path: options.config.clone(),
            runtime,
            draft: None,
            name: cx.new(|cx| InputState::new(window, cx).placeholder("例如 filesystem")),
            command: cx.new(|cx| InputState::new(window, cx).placeholder("例如 npx")),
            args: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(3, 6)
                    .placeholder("每行一个参数，不需要引号")
            }),
            env: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(3, 6)
                    .placeholder("每行一个 KEY=value")
            }),
            timeout: cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value("120")
                    .min(1.)
                    .step(1.)
            }),
            tools: cx.new(|cx| {
                TextareaState::new(window, cx)
                    .auto_grow(3, 6)
                    .placeholder("每行一个工具名；留空允许所有工具")
            }),
            editing: false,
            existing: None,
            template: None,
            enabled: false,
            busy: false,
            error: None,
            saved: false,
            base_reloaded: false,
            task: Task::ready(()),
            statuses: None,
            expanded: std::collections::HashSet::new(),
            status_busy: false,
            status_error: None,
            status_task: Task::ready(()),
        };
        view.load(cx);
        let entity = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = entity.update(cx, |view, cx| view.refresh_status(cx));
        });
        view
    }
    pub(super) fn bind_context(
        &mut self,
        session: Option<String>,
        workspace: String,
        cx: &mut Context<Self>,
    ) {
        if self.session == session && self.workspace == workspace {
            return;
        }
        self.status_task = Task::ready(());
        self.session = session;
        self.workspace = workspace;
        self.statuses = None;
        self.expanded.clear();
        self.status_busy = false;
        self.status_error = None;
        self.refresh_status(cx);
    }
    fn refresh_status(&mut self, cx: &mut Context<Self>) {
        if self.status_busy {
            return;
        }
        let Some(session) = self.session.clone() else {
            self.status_error = Some("打开工作区后可查看会话的 MCP 状态。".into());
            cx.notify();
            return;
        };
        let workspace = self.workspace.clone();
        let workspace_owner = self.owner.clone();
        self.status_busy = true;
        self.status_error = None;
        self.status_task = cx.spawn(async move |owner, cx| {
            // Rebinding settings can run inside a Workspace update. Dispatch
            // on the next task turn to avoid borrowing that parent recursively.
            let (result, receiver) = tokio::sync::oneshot::channel();
            let sent = workspace_owner
                .update(cx, |view, _| {
                    anyhow::ensure!(
                        view.info
                            .as_ref()
                            .is_some_and(|info| info.id == session && info.workspace == workspace),
                        "工作区已切换，请重新打开 MCP 工具页面以刷新会话状态。"
                    );
                    view.backend
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("请先连接工作区"))?
                        .send(Command::Settings {
                            request: Request::Resources {
                                session: session.clone(),
                                expected_workspace: Some(workspace.clone()),
                                action: Action::Mcp,
                            },
                            result,
                        })
                })
                .map_err(|error| error.to_string())
                .and_then(|result| result.map_err(|error| error.to_string()));
            let result = match sent {
                Ok(()) => receiver
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|result| result),
                Err(error) => Err(error),
            };
            let _ = owner.update(cx, |view, cx| {
                if view.session.as_ref() != Some(&session) || view.workspace != workspace {
                    return;
                }
                view.status_busy = false;
                match result {
                    Ok(Response::Resources(Snapshot::Mcp(servers))) => {
                        view.statuses = Some(servers)
                    }
                    Ok(_) => {
                        view.status_error =
                            Some("服务没有返回 MCP 数据，请更新工作区服务后重试。".into())
                    }
                    Err(error) => view.status_error = Some(error),
                }
                cx.notify();
            });
        });
        cx.notify();
    }
    fn load(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let path = self.path.clone();
        let job = self.runtime.spawn_blocking(move || McpDraft::load(&path));
        self.receive(job, false, cx);
    }
    fn reload_base(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let path = self.path.clone();
        let job = self.runtime.spawn_blocking(move || McpDraft::load(&path));
        self.busy = true;
        self.error = None;
        self.base_reloaded = false;
        self.task = cx.spawn(async move |owner, cx| {
            let result = job.await;
            let _ = owner.update(cx, |view, cx| {
                view.busy = false;
                match result {
                    Ok(Ok(draft)) => {
                        // Preserve edited fields, but retain current unexposed server options.
                        view.template = view
                            .existing
                            .as_ref()
                            .and_then(|name| draft.servers().get(name).cloned());
                        view.draft = Some(draft);
                        view.base_reloaded = true;
                    }
                    Ok(Err(e)) => view.error = Some(format!("{e:#}")),
                    Err(e) => view.error = Some(e.to_string()),
                }
                cx.notify();
            });
        });
        cx.notify();
    }
    fn receive(
        &mut self,
        job: tokio::task::JoinHandle<anyhow::Result<McpDraft>>,
        saved: bool,
        cx: &mut Context<Self>,
    ) {
        self.busy = true;
        self.error = None;
        self.base_reloaded = false;
        self.task = cx.spawn(async move |owner, cx| {
            let result = job.await;
            let _ = owner.update(cx, |view, cx| {
                view.busy = false;
                match result {
                    Ok(Ok(draft)) => {
                        view.draft = Some(draft);
                        view.editing = false;
                        view.saved = saved;
                    }
                    Ok(Err(e)) => view.error = Some(format!("{e:#}")),
                    Err(e) => view.error = Some(e.to_string()),
                }
                cx.notify();
            });
        });
        cx.notify();
    }
    fn edit(&mut self, name: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.template = name
            .as_ref()
            .and_then(|name| self.draft.as_ref()?.servers().get(name).cloned());
        self.existing = name.clone();
        self.enabled = self.template.as_ref().is_some_and(|s| s.enabled);
        self.editing = true;
        self.error = None;
        self.saved = false;
        self.base_reloaded = false;
        self.name.update(cx, |input, cx| {
            input.set_value(name.unwrap_or_default(), window, cx)
        });
        self.command.update(cx, |input, cx| {
            input.set_value(
                self.template
                    .as_ref()
                    .map(|s| s.command.clone())
                    .unwrap_or_default(),
                window,
                cx,
            )
        });
        self.args.update(cx, |input, cx| {
            input.set_value(
                self.template
                    .as_ref()
                    .map(|s| s.args.join("\n"))
                    .unwrap_or_default(),
                window,
                cx,
            )
        });
        self.env.update(cx, |input, cx| {
            input.set_value(
                self.template
                    .as_ref()
                    .map(|s| {
                        s.env
                            .iter()
                            .map(|(k, v)| format!("{k}={v}"))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default(),
                window,
                cx,
            )
        });
        self.timeout.update(cx, |input, cx| {
            input.set_value(
                self.template
                    .as_ref()
                    .map_or(120, |server| server.timeout_seconds)
                    .to_string(),
                window,
                cx,
            )
        });
        self.tools.update(cx, |input, cx| {
            input.set_value(
                self.template
                    .as_ref()
                    .map(|server| server.tools.join("\n"))
                    .unwrap_or_default(),
                window,
                cx,
            )
        });
        cx.notify();
    }
    fn save(&mut self, cx: &mut Context<Self>) {
        let name = self.name.read(cx).value().trim().to_string();
        if self.existing.is_none()
            && self
                .draft
                .as_ref()
                .is_some_and(|d| d.servers().contains_key(&name))
        {
            self.error = Some("名称已存在，请使用其他名称".into());
            cx.notify();
            return;
        }
        let env = self.env.read(cx).value();
        let mut values = std::collections::BTreeMap::new();
        for line in env.lines().filter(|l| !l.trim().is_empty()) {
            let Some((key, value)) = line.split_once('=') else {
                self.error = Some("环境变量格式应为 KEY=value".into());
                cx.notify();
                return;
            };
            if key.trim().is_empty()
                || values
                    .insert(key.trim().to_string(), value.to_string())
                    .is_some()
            {
                self.error = Some("环境变量名称不能为空或重复".into());
                cx.notify();
                return;
            }
        }
        let Ok(timeout_seconds) = self.timeout.read(cx).value().parse::<u64>() else {
            self.error = Some("超时应为正整数（秒）。".into());
            cx.notify();
            return;
        };
        if timeout_seconds == 0 {
            self.error = Some("超时应为正整数（秒）。".into());
            cx.notify();
            return;
        }
        let server = McpServerConfig {
            transport: "stdio".into(),
            command: self.command.read(cx).value().trim().into(),
            args: self
                .args
                .read(cx)
                .value()
                .lines()
                .map(str::to_string)
                .collect(),
            env: values,
            timeout_seconds,
            tools: self
                .tools
                .read(cx)
                .value()
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect(),
            enabled: self.enabled,
        };
        self.persist(name, Some(server), cx);
    }
    fn persist(&mut self, name: String, server: Option<McpServerConfig>, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let Some(draft) = self.draft.take() else {
            return;
        };
        let path = self.path.clone();
        // Return the draft on failure so validation never discards unsaved form values.
        self.busy = true;
        self.error = None;
        self.base_reloaded = false;
        let job = self.runtime.spawn_blocking(move || {
            let result = draft.save(&path, &name, server);
            (draft, result)
        });
        self.task = cx.spawn(async move |owner, cx| {
            let result = job.await;
            let _ = owner.update(cx, |view, cx| {
                view.busy = false;
                match result {
                    Ok((_, Ok(draft))) => {
                        view.draft = Some(draft);
                        view.editing = false;
                        view.saved = true;
                    }
                    Ok((draft, Err(e))) => {
                        view.draft = Some(draft);
                        view.error = Some(format!("{e:#}"));
                    }
                    Err(e) => view.error = Some(e.to_string()),
                }
                cx.notify();
            });
        });
        cx.notify();
    }
    fn confirm_remove(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let owner = cx.weak_entity();
        window.open_alert_dialog(cx, move |dialog, _, _| {
            let owner = owner.clone();
            let name = name.clone();
            dialog
                .confirm()
                .title(format!("移除“{name}”？"))
                .description("服务器将从配置中移除。当前会话已启动的工具保留到会话结束。")
                .button_props(
                    DialogButtonProps::default()
                        .ok_text("移除")
                        .ok_variant(ButtonVariant::Danger)
                        .cancel_text("取消"),
                )
                .on_ok(move |_, _, cx| {
                    let _ = owner.update(cx, |view, cx| view.persist(name.clone(), None, cx));
                    true
                })
        });
    }
}
impl Render for Mcp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut content = v_flex().w_full().max_w(rems(60.)).gap_4()
            .child(h_flex().gap_3()
                .child(div().flex_1().min_w_0().whitespace_normal().text_sm().text_color(cx.theme().muted_foreground)
                    .child("配置保存在本机，对所有项目的新会话生效；连接 SSH 工作区时，命令在远程运行。"))
                .child(Button::new("mcp-refresh").ghost().small().label("刷新")
                    .disabled(self.busy || self.status_busy || self.editing)
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.load(cx);
                        view.refresh_status(cx);
                    }))))
            .when(self.saved, |view| view.child(div().text_sm().text_color(cx.theme().muted_foreground)
                .child("已保存。新建会话以应用 MCP 配置。")))
            .when(self.base_reloaded, |view| view.child(div().text_sm().text_color(cx.theme().muted_foreground)
                .child("已重载最新配置，表单内容已保留。请检查后再次保存。")))
            .when_some(self.error.clone(), |view, error| view.child(div().text_sm().text_color(cx.theme().danger).child(error)))
            .when(self.editing && self.error.is_some(), |view| view.child(h_flex().child(
                Button::new("mcp-reload-base").small().label("保留表单并重载配置")
                    .disabled(self.busy).on_click(cx.listener(|view, _, _, cx| view.reload_base(cx))),
            )));
        if self.editing {
            return content.child(
                v_flex()
                    .gap_3()
                    .child("服务器名称")
                    .child(
                        Input::new(&self.name)
                            .aria_label("服务器名称")
                            .disabled(self.busy || self.existing.is_some()),
                    )
                    .child("启动命令")
                    .child(
                        Input::new(&self.command)
                            .aria_label("启动命令")
                            .disabled(self.busy),
                    )
                    .child("参数 · 每行一个")
                    .child(
                        Textarea::new(&self.args)
                            .aria_label("启动参数，每行一个")
                            .disabled(self.busy),
                    )
                    .child("环境变量 · KEY=value")
                    .child(
                        Textarea::new(&self.env)
                            .aria_label("环境变量，每行一个 KEY=value")
                            .disabled(self.busy),
                    )
                    .child("请求超时（秒）")
                    .child(
                        div()
                            .w_32()
                            .child(NumberInput::new(&self.timeout).disabled(self.busy)),
                    )
                    .child("允许的工具 · 每行一个原始工具名")
                    .child(
                        Textarea::new(&self.tools)
                            .aria_label("允许的 MCP 工具，每行一个原始工具名")
                            .disabled(self.busy),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("工具列表留空时允许所有工具；工具仍遵循当前会话的审批设置。"),
                    )
                    .child(
                        Checkbox::new("mcp-enabled")
                            .label("在新会话启用服务器")
                            .checked(self.enabled)
                            .disabled(self.busy)
                            .on_click(cx.listener(|v, value, _, cx| {
                                v.enabled = *value;
                                cx.notify();
                            })),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("mcp-save")
                                    .primary()
                                    .label(if self.busy { "处理中…" } else { "保存" })
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|v, _, _, cx| v.save(cx))),
                            )
                            .child(
                                Button::new("mcp-cancel")
                                    .label("返回列表")
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|v, _, _, cx| {
                                        v.editing = false;
                                        v.error = None;
                                        v.base_reloaded = false;
                                        cx.notify();
                                    })),
                            ),
                    ),
            );
        }
        let mut runtime = v_flex()
            .gap_3()
            .mt_4()
            .pt_4()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("当前会话的连接状态"),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .whitespace_normal()
                    .child(format!(
                        "{} · 会话启动时的连接结果与已注册工具。配置修改会在新会话应用。",
                        self.workspace
                    )),
            )
            .when_some(self.status_error.clone(), |view, error| {
                view.child(div().text_sm().text_color(cx.theme().danger).child(error))
            });
        if let Some(servers) = &self.statuses {
            if servers.is_empty() {
                runtime = runtime.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("当前会话没有配置 MCP 服务器。"),
                );
            }
            for server in servers {
                let name = server.name.clone();
                let expanded = self.expanded.contains(&name);
                runtime = runtime.child(
                    v_flex()
                        .gap_2()
                        .py_3()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(
                            h_flex()
                                .gap_3()
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .gap_1()
                                        .child(server.name.clone())
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(format!(
                                                    "{} · {} 个可用工具",
                                                    server.detail,
                                                    server.tools.len()
                                                )),
                                        ),
                                )
                                .child(
                                    Button::new(SharedString::from(format!("mcp-tools:{name}")))
                                        .small()
                                        .label(if expanded {
                                            "收起工具"
                                        } else {
                                            "查看工具"
                                        })
                                        .disabled(server.tools.is_empty())
                                        .on_click(cx.listener(move |view, _, _, cx| {
                                            if !view.expanded.remove(&name) {
                                                view.expanded.insert(name.clone());
                                            }
                                            cx.notify();
                                        })),
                                ),
                        )
                        .when(expanded, |view| {
                            view.children(server.tools.iter().map(|tool| {
                                v_flex()
                                    .gap_1()
                                    .pl_4()
                                    .py_2()
                                    .child(div().text_sm().child(tool.name.clone()))
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(cx.theme().muted_foreground)
                                            .child(tool.description.clone()),
                                    )
                            }))
                        }),
                );
            }
        } else if self.status_busy {
            runtime = runtime.child(
                h_flex()
                    .gap_2()
                    .child(gpui_kit::component::spinner::Spinner::new())
                    .child("正在读取会话状态…"),
            );
        }
        content = content.child(
            h_flex()
                .child(
                    div()
                        .flex_1()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("服务器配置"),
                )
                .child(
                    Button::new("mcp-add")
                        .icon(IconName::Plus)
                        .label("添加服务器…")
                        .disabled(self.busy || self.draft.is_none())
                        .on_click(cx.listener(|v, _, w, cx| v.edit(None, w, cx))),
                ),
        );
        if let Some(draft) = &self.draft {
            if draft.servers().is_empty() {
                content = content.child(
                    div()
                        .py_8()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("还没有 MCP 服务器。添加后，Agent 就能使用服务器提供的工具。"),
                );
            }
            for (name, server) in draft.servers() {
                let edit_name = name.clone();
                let toggle_name = name.clone();
                let remove_name = name.clone();
                let server = server.clone();
                let enabled = server.enabled;
                content = content.child(
                    h_flex()
                        .gap_4()
                        .py_3()
                        .border_b_1()
                        .border_color(cx.theme().border)
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1()
                                .child(name.clone())
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(server.command.clone()),
                                ),
                        )
                        .child(
                            Button::new(SharedString::from(format!("edit-{name}")))
                                .small()
                                .label("编辑…")
                                .disabled(self.busy)
                                .on_click(cx.listener(move |v, _, w, cx| {
                                    v.edit(Some(edit_name.clone()), w, cx)
                                })),
                        )
                        .child(
                            Switch::new(SharedString::from(format!("enable-{name}")))
                                .label("新会话启用")
                                .checked(enabled)
                                .disabled(self.busy)
                                .on_click(cx.listener(move |v, value, _, cx| {
                                    let mut server = server.clone();
                                    server.enabled = *value;
                                    v.persist(toggle_name.clone(), Some(server), cx);
                                })),
                        )
                        .child(
                            Button::new(SharedString::from(format!("remove-{name}")))
                                .ghost()
                                .small()
                                .label("移除…")
                                .disabled(self.busy)
                                .on_click(cx.listener(move |view, _, window, cx| {
                                    view.confirm_remove(remove_name.clone(), window, cx)
                                })),
                        ),
                )
            }
        } else if self.busy {
            content = content.child("正在加载…");
        }
        content.child(runtime)
    }
}
