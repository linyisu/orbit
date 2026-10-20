//! MCP configuration belongs to the client configuration, even for remote sessions.
mod import;
use super::super::*;
use gpui_kit::component::{
    button::ButtonVariant,
    checkbox::Checkbox,
    dialog::DialogButtonProps,
    input::{Input, InputState, NumberInput},
    switch::Switch,
};
use orbit_config::config::connections::mcp::{McpDraft, environment};
use orbit_config::config::connections::mcp::{McpServerConfig, ServerInput};
use orbit_protocol::resources::{Action, McpServer, Snapshot};

pub(super) struct Mcp {
    importer: Option<Entity<import::Importer>>,
    _import_events: Option<Subscription>,
    imported: bool,
    owner: WeakEntity<Workspace>,
    session: Option<String>,
    workspace: String,
    path: PathBuf,
    startup_override: bool,
    startup_revision: Option<u64>,
    _preferences: Subscription,
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
    applied: bool,
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
        self.busy || self.editing || self.importer.is_some()
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
        crate::preferences::Preferences::observe_saves(cx);
        let preferences = cx.observe_global::<crate::preferences::Preferences>(|_, cx| cx.notify());
        let mut view = Self {
            importer: None,
            _import_events: None,
            imported: false,
            owner,
            session,
            workspace,
            path: options.config.clone(),
            startup_override: options.allow_mcp_start,
            startup_revision: None,
            _preferences: preferences,
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
                    .placeholder("每行 KEY=value，例如 DEMO_TOOL_HOME=/opt/sample-tool")
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
            applied: false,
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
        self.applied = false;
        self.refresh_status(cx);
    }

    fn refresh_status(&mut self, cx: &mut Context<Self>) {
        self.request_status(Action::Mcp, cx);
    }

    fn apply_current(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = &self.draft else { return };
        let config = orbit_config::config::connections::mcp::McpConfig {
            servers: draft.servers().clone(),
        };
        let config = match serde_json::to_value(config) {
            Ok(config) => config,
            Err(error) => {
                self.status_error = Some(error.to_string());
                cx.notify();
                return;
            }
        };
        self.request_status(
            Action::ReloadMcp {
                config,
                allow_start: self.startup_override
                    || crate::preferences::Preferences::allows_mcp_start(&self.path, cx),
            },
            cx,
        );
    }

    fn request_status(&mut self, action: Action, cx: &mut Context<Self>) {
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
        let applying = matches!(action, Action::ReloadMcp { .. });
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
                                action,
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
                        view.statuses = Some(servers);
                        if applying {
                            view.applied = true;
                        }
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
        let job = self
            .runtime
            .spawn_blocking(move || McpDraft::load(&path));
        self.receive(job, false, cx);
    }

    fn reload_base(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let path = self.path.clone();
        let job = self
            .runtime
            .spawn_blocking(move || McpDraft::load(&path));
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
                        view.applied = false;
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
        self.imported = false;
        self.template = name.as_ref().and_then(|name| {
            self.draft
                .as_ref()?
                .servers()
                .get(name)
                .cloned()
        });
        self.existing = name.clone();
        self.enabled = self
            .template
            .as_ref()
            .is_some_and(|s| s.enabled);
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
                    .map(|server| environment::format(&server.env))
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
        let name = self
            .name
            .read(cx)
            .value()
            .trim()
            .to_string();
        let fields = [
            self.command
                .read(cx)
                .value()
                .to_string(),
            self.args.read(cx).value().to_string(),
            self.env.read(cx).value().to_string(),
            self.timeout
                .read(cx)
                .value()
                .to_string(),
            self.tools.read(cx).value().to_string(),
        ];
        let server = match (ServerInput {
            command: &fields[0],
            args: &fields[1],
            env: &fields[2],
            timeout_seconds: &fields[3],
            tools: &fields[4],
            enabled: self.enabled,
        })
        .parse()
        {
            Ok(server) => server,
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                cx.notify();
                return;
            }
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
        let creating = self.existing.is_none() && self.editing;
        let job = self.runtime.spawn_blocking(move || {
            let result = if creating && let Some(server) = server {
                draft.create(&path, &name, server)
            } else {
                draft.save(&path, &name, server)
            };
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
                        view.applied = false;
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

    fn import_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.session.clone() else {
            return;
        };
        let owner = self.owner.clone();
        let workspace = self.workspace.clone();
        let importer = cx.new(|cx| import::Importer::new(owner, session, workspace, window, cx));
        self._import_events =
            Some(
                cx.subscribe_in(&importer, window, |view, _, event, window, cx| {
                    match event {
                        import::Event::Closed => {
                            view.importer = None;
                        }
                        import::Event::Prepared { name, launch } => {
                            view.importer = None;
                            view.edit(None, window, cx);
                            view.imported = true;
                            view.name
                                .update(cx, |input, cx| input.set_value(name.clone(), window, cx));
                            view.command.update(cx, |input, cx| {
                                input.set_value(launch.command.clone(), window, cx)
                            });
                            view.args.update(cx, |input, cx| {
                                input.set_value(launch.args.join("\n"), window, cx)
                            });
                            view.env.update(cx, |input, cx| {
                                input.set_value(environment::format(&launch.env), window, cx)
                            });
                            view.tools.update(cx, |input, cx| {
                                input.set_value(launch.tools.join("\n"), window, cx)
                            });
                        }
                    }
                    cx.notify();
                }),
            );
        self.importer = Some(importer);
        cx.notify();
    }

    fn startup_control(&self, cx: &mut Context<Self>) -> AnyElement {
        use crate::{keymap::SaveStatus, preferences::Preferences};
        let allowed = self.startup_override || Preferences::allows_mcp_start(&self.path, cx);
        let mut view = v_flex().w_full().gap_2()
            .child(Switch::new("mcp-allow-start")
                .label("允许启动已启用的 MCP 服务器")
                .checked(allowed)
                .disabled(self.startup_override)
                .on_click(cx.listener(|view, allowed, _, cx| {
                    view
                        .startup_revision = Some(Preferences::set_mcp_start(&view
                        .path,
                        *allowed,
                        cx));
                    cx.notify();
                })))
            .child(div().text_sm().whitespace_normal().text_color(cx.theme().muted_foreground)
                .child(if self.startup_override {
                    "本次启动已通过 --allow-mcp-start 授权。服务器以目标机器上的当前用户身份运行。"
                } else {
                    "适用于使用此配置的所有项目。打开后，可应用到当前会话或在新会话启动服务器；关闭开关不会中断正在运行的工具。"
                }));
        if let Some(revision) = self.startup_revision {
            let status = Preferences::save_status(cx);
            if status.revision() >= revision {
                view = match status {
                    SaveStatus::Saving(_) => view.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("正在保存启动设置…"),
                    ),
                    SaveStatus::Saved(_) => view.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("启动设置已保存，可应用到当前会话。"),
                    ),
                    SaveStatus::Failed { message, .. } => view.child(
                        v_flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_sm()
                                    .whitespace_normal()
                                    .text_color(cx.theme().danger)
                                    .child(format!("启动设置尚未保存：{message}")),
                            )
                            .child(
                                Button::new("mcp-retry-startup-save")
                                    .small()
                                    .label("重试保存")
                                    .on_click(cx.listener(|view, _, _, cx| {
                                        view.startup_revision = Some(Preferences::retry_save(cx));
                                        cx.notify();
                                    })),
                            ),
                    ),
                };
            }
        }
        view.into_any_element()
    }

    fn confirm_remove(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let owner = cx.weak_entity();
        crate::dialog::alert(window, cx, move |dialog, _, _| {
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
        if let Some(importer) = &self.importer {
            return div()
                .w_full()
                .max_w(rems(60.))
                .child(importer.clone())
                .into_any_element();
        }
        let mut content = v_flex().w_full().max_w(rems(60.)).gap_4()
            .child(self.startup_control(cx))
            .child(h_flex().gap_3()
                .child(div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_normal()
                    .text_sm()
                    .text_color(cx
                    .theme()
                    .muted_foreground)
                    .child("配置保存在本机，可应用到当前会话，也用于新建会话。支持 stdio 服务；SSH / WSL \
                        会话在目标机器启动命令。参数、依赖和路径也须在目标机器可用。"))
                .child(Button::new("mcp-refresh").ghost().small().label("刷新")
                    .disabled(self.busy || self.status_busy || self.editing)
                    .on_click(cx.listener(|view, _, _, cx| {
                        view.load(cx);
                        view.refresh_status(cx);
                    }))))
            .when(self
                .saved
                && !self
                .applied,
                |view| view
                .child(div()
                .text_sm()
                .text_color(cx
                .theme()
                .muted_foreground)
                .child("已保存。点击“应用到当前会话”以重新连接。")))
            .when(self
                .applied,
                |view| view
                .child(div()
                .text_sm()
                .text_color(cx
                .theme()
                .muted_foreground)
                .child("连接结果已更新，下一次发送将使用当前可用工具。")))
            .when(self
                .base_reloaded,
                |view| view
                .child(div()
                .text_sm()
                .text_color(cx
                .theme()
                .muted_foreground)
                .child("已重载最新配置，表单内容已保留。请检查后再次保存。")))
            .when_some(self
                .error
                .clone(),
                |view,
                error| view
                .child(div()
                .text_sm()
                .text_color(cx
                .theme()
                .danger)
                .child(error)))
            .when(self.editing && self.error.is_some(), |view| view.child(h_flex().child(
                Button::new("mcp-reload-base").small().label("保留表单并重载配置")
                    .disabled(self
                        .busy)
                        .on_click(cx
                        .listener(|view,
                        _,
                        _,
                        cx| view
                        .reload_base(cx))),
            )));
        if self.editing {
            return content
                .child(
                    v_flex()
                        .gap_3()
                        .when(self.imported, |view| {
                            view
                        .child(div()
                        .text_sm()
                        .whitespace_normal()
                        .text_color(cx
                        .theme()
                        .muted_foreground)
                        .child("来源已准备好。请核对名称、启动参数和环境变量，再保存并启用。"))
                        })
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
                        .child("环境变量 · 每行 KEY=value，可加成对引号")
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
                                .child(
                                    "工具列表留空时允许所有工具；工具仍遵循当前会话的审批设置。",
                                ),
                        )
                        .child(
                            Checkbox::new("mcp-enabled")
                                .label("启用服务器")
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
                )
                .into_any_element();
        }
        let mut runtime = v_flex()
            .gap_3()
            .pt_4()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("当前会话的连接状态"),
                    )
                    .child(
                        Button::new("mcp-apply-current")
                            .small()
                            .label(if self.status_busy {
                                "正在连接"
                            } else {
                                "应用到当前会话"
                            })
                            .disabled(
                                self.busy
                                    || self.status_busy
                                    || self.editing
                                    || self.draft.is_none()
                                    || self.session.is_none(),
                            )
                            .on_click(cx.listener(|view, _, _, cx| view.apply_current(cx))),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .whitespace_normal()
                    .child(format!(
                        "{} · 应用配置会重新连接服务器，正在执行的任务继续使用原有工具。",
                        orbit_client::directories::display_path(&self.workspace)
                    )),
            )
            .when_some(self.status_error.clone(), |view, error| {
                view.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
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
                let detail = if server
                    .detail
                    .starts_with("启动需显式授权")
                {
                    if self.startup_override
                        || crate::preferences::Preferences::allows_mcp_start(&self.path, cx)
                    {
                        "点击“应用到当前会话”以启动并连接。".to_owned()
                    } else {
                        "尚未允许启动。打开上方开关后，应用到当前会话。".to_owned()
                    }
                } else {
                    server.detail.clone()
                };
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
                                                .whitespace_normal()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(format!(
                                                    "{} · {} 个可用工具",
                                                    detail,
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
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("服务器配置"),
                )
                .child(
                    Button::new("mcp-import")
                        .icon(IconName::Download)
                        .label("从来源导入")
                        .disabled(self.busy || self.session.is_none() || self.draft.is_none())
                        .on_click(
                            cx.listener(|view, _, window, cx| view.import_source(window, cx)),
                        ),
                )
                .child(
                    Button::new("mcp-add")
                        .icon(IconName::Plus)
                        .label("添加服务器")
                        .disabled(self.busy || self.draft.is_none())
                        .on_click(cx.listener(|v, _, w, cx| v.edit(None, w, cx))),
                ),
        );
        if let Some(draft) = &self.draft {
            if draft.servers().is_empty() {
                content = content.child(
                    div()
                        .py_1()
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
                                .label("启用")
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
        content
            .child(runtime)
            .into_any_element()
    }
}
