//! MCP configuration belongs to the client configuration, even for remote sessions.
use super::super::*;
use gpui_kit::component::{
    input::{Input, InputState},
    switch::Switch,
};
use orbit_config::config::connections::mcp::McpDraft;
use orbit_tools::mcp::McpServerConfig;

pub(super) struct Mcp {
    path: PathBuf,
    runtime: Arc<tokio::runtime::Runtime>,
    draft: Option<McpDraft>,
    name: Entity<InputState>,
    command: Entity<InputState>,
    args: Entity<TextareaState>,
    env: Entity<TextareaState>,
    editing: bool,
    existing: Option<String>,
    template: Option<McpServerConfig>,
    enabled: bool,
    busy: bool,
    error: Option<String>,
    saved: bool,
    base_reloaded: bool,
    task: Task<()>,
}
impl Mcp {
    pub(super) fn new(
        options: &Options,
        runtime: Arc<tokio::runtime::Runtime>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self {
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
            editing: false,
            existing: None,
            template: None,
            enabled: false,
            busy: false,
            error: None,
            saved: false,
            base_reloaded: false,
            task: Task::ready(()),
        };
        view.load(cx);
        view
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
            timeout_seconds: self.template.as_ref().map_or(120, |s| s.timeout_seconds),
            tools: self
                .template
                .as_ref()
                .map_or_else(Vec::new, |s| s.tools.clone()),
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
}
impl Render for Mcp {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut content=v_flex().w_full().max_w(rems(60.)).gap_4()
            .child(h_flex().gap_3().child(div().flex_1().text_sm().text_color(cx.theme().muted_foreground).child("配置 MCP 工具服务器。保存后新会话生效；连接远程工作区时，命令在远程运行。"))
                .child(Button::new("mcp-refresh").ghost().small().label("刷新").disabled(self.busy||self.editing).on_click(cx.listener(|v,_,_,cx|v.load(cx)))))
            .when(self.saved,|v|v.child(div().text_sm().text_color(cx.theme().muted_foreground).child("已保存。新建会话以应用 MCP 配置。")))
            .when(self.base_reloaded,|v|v.child(div().text_sm().text_color(cx.theme().muted_foreground).child("已重载最新配置，表单内容已保留。请检查后再次保存。")))
            .when_some(self.error.clone(),|v,e|v.child(div().text_sm().text_color(cx.theme().danger).child(e)))
            .when(self.editing && self.error.is_some(), |v| v.child(
                h_flex().child(Button::new("mcp-reload-base")
                    .small().label("保留表单并重载配置")
                    .disabled(self.busy).on_click(cx.listener(|v, _, _, cx| v.reload_base(cx))))
            ));
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
                    .child(
                        Switch::new("mcp-enabled")
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
            );
        }
        content = content.child(
            h_flex().child(
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
                        .py_8()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("还没有 MCP 服务器。添加后，Agent 就能使用服务器提供的工具。"),
                );
            }
            for (name, server) in draft.servers() {
                let edit_name = name.clone();
                let toggle_name = name.clone();
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
                                .label("编辑")
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
                        ),
                )
            }
        } else if self.busy {
            content = content.child("正在加载…");
        }
        content
    }
}
