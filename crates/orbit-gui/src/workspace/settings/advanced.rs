//! Deferred application settings, persisted through the reviewed config snapshot.
use super::super::*;
use super::section;
use gpui_kit::component::{
    checkbox::Checkbox,
    input::{Input, InputState},
};
use orbit_config::config::advanced::{AdvancedDraft, AgentSettings};
use orbit_protocol::Capability;
use std::collections::BTreeSet;

const FIELDS: [(&str, &str, &str); 11] = [
    ("会话轮数上限", "0 表示不限轮数。", "0"),
    ("会话工具调用上限", "0 表示不限调用次数。", "2000"),
    (
        "默认命令超时（秒）",
        "未指定时使用此值，必须大于 0。",
        "120",
    ),
    ("最长命令超时（秒）", "不能小于默认命令超时。", "600"),
    (
        "单次文件扫描上限（字节）",
        "用于 read_file；0 表示不限扫描量。",
        "67108864",
    ),
    (
        "并行子 Agent 上限",
        "同一父会话中可同时运行 1–128 个子 Agent。",
        "8",
    ),
    ("子 Agent 轮数上限", "0 表示不限轮数。", "0"),
    ("子 Agent 工具调用上限", "0 表示不限调用次数。", "2000"),
    (
        "子 Agent 进度提醒间隔（秒）",
        "0 关闭提醒；最大 86400。提醒不会终止子 Agent。",
        "1800",
    ),
    (
        "探索子 Agent 模型",
        "填写已配置的“连接::模型”；留空继承父会话。",
        "继承父会话",
    ),
    (
        "审查子 Agent 模型",
        "填写已配置的“连接::模型”；留空继承父会话。",
        "继承父会话",
    ),
];

const CAPABILITIES: [(Capability, &str, &str); 14] = [
    (Capability::All, "all", "全部可选工具"),
    (Capability::Subagents, "subagents", "子 Agent"),
    (Capability::Web, "web", "网页搜索与抓取"),
    (Capability::Mcp, "mcp", "MCP 服务器与工具"),
    (Capability::Skills, "skills", "技能目录与读取"),
    (Capability::Image, "image", "图片附件与读取"),
    (Capability::Document, "document", "文档读取"),
    (Capability::Files, "files", "文件读取、搜索与项目概览"),
    (Capability::Git, "git", "Git 操作"),
    (Capability::Write, "write", "文件写入与编辑"),
    (Capability::Capability, "capability", "工作区外路径授权请求"),
    (Capability::Command, "command", "命令执行与验证"),
    (Capability::History, "history", "归档上下文检索与裁剪"),
    (Capability::ProjectState, "project_state", "任务与持久记忆"),
];

pub(super) struct Advanced {
    path: PathBuf,
    auth: PathBuf,
    runtime: Arc<tokio::runtime::Runtime>,
    draft: Option<AdvancedDraft>,
    fields: Vec<Entity<InputState>>,
    tools: Entity<TextareaState>,
    auto_compact: bool,
    network_allowed: bool,
    subagents_enabled: bool,
    disabled: BTreeSet<Capability>,
    busy: bool,
    error: Option<String>,
    saved: bool,
    task: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl Advanced {
    pub(super) fn has_pending_work(&self, cx: &App) -> bool {
        if self.busy {
            return true;
        }
        let Some(draft) = &self.draft else {
            return false;
        };
        // Invalid input is still an unsaved edit. Compare parsed values so a
        // saved value such as "002" does not keep the placeholder indefinitely.
        let Ok(current) = self.values(cx) else {
            return true;
        };
        !same_form_values(&current, draft.settings())
    }

    pub(super) fn new(
        options: &Options,
        runtime: Arc<tokio::runtime::Runtime>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (auth, error) = match orbit_auth::directory(
            options.auth_dir.clone(),
            std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
            std::env::var_os("HOME").map(PathBuf::from),
        ) {
            Ok(path) => (path, None),
            Err(error) => (PathBuf::new(), Some(error.to_string())),
        };
        let fields: Vec<_> = FIELDS
            .iter()
            .map(|(_, _, placeholder)| {
                cx.new(|cx| InputState::new(window, cx).placeholder(*placeholder))
            })
            .collect();
        let mut subscriptions: Vec<_> = fields
            .iter()
            .map(|field| {
                cx.subscribe(field, |view: &mut Self, _, event, cx| match event {
                    InputEvent::Change => view.edited(cx),
                    InputEvent::PressEnter { shift: false, .. } => view.save(cx),
                    _ => {}
                })
            })
            .collect();
        let tools = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(3, 8)
                .placeholder("每行一个完整工具名，例如 read_file 或 mcp__server__tool")
        });
        subscriptions.push(cx.subscribe(&tools, |view: &mut Self, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                view.edited(cx);
            }
        }));
        let mut view = Self {
            path: options.config.clone(),
            auth,
            runtime,
            draft: None,
            fields,
            tools,
            auto_compact: false,
            network_allowed: false,
            subagents_enabled: false,
            disabled: BTreeSet::new(),
            busy: false,
            error,
            saved: false,
            task: Task::ready(()),
            _subscriptions: subscriptions,
        };
        view.load(cx);
        view
    }

    fn edited(&mut self, cx: &mut Context<Self>) {
        self.saved = false;
        cx.notify();
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.auth.as_os_str().is_empty() {
            return;
        }
        let path = self.path.clone();
        let auth = self.auth.clone();
        let job = self
            .runtime
            .spawn_blocking(move || AdvancedDraft::load(&path, &auth));
        self.busy = true;
        self.error = None;
        self.saved = false;
        self.task = cx.spawn(async move |owner, cx| {
            let result = job.await;
            let _ = owner.update_in(cx, |view, window, cx| {
                view.busy = false;
                match result {
                    Ok(Ok(draft)) => view.apply(draft, window, cx),
                    Ok(Err(error)) => view.error = Some(format!("{error:#}")),
                    Err(error) => view.error = Some(error.to_string()),
                }
                cx.notify();
            });
        });
        cx.notify();
    }

    fn apply(&mut self, draft: AdvancedDraft, window: &mut Window, cx: &mut Context<Self>) {
        let settings = draft.settings();
        let values = form_fields(settings);
        for (field, value) in self.fields.iter().zip(values) {
            field.update(cx, |input, cx| input.set_value(value, window, cx));
        }
        self.tools.update(cx, |input, cx| {
            input.set_value(settings.disabled_tools.join("\n"), window, cx)
        });
        self.auto_compact = settings.auto_compact;
        self.network_allowed = settings.command.network.is_allowed();
        self.subagents_enabled = settings.subagents.enabled;
        self.disabled = settings
            .disabled_capabilities
            .iter()
            .copied()
            .collect();
        self.draft = Some(draft);
    }

    fn values(&self, cx: &App) -> anyhow::Result<AgentSettings> {
        let mut settings = self
            .draft
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("请先载入配置。"))?
            .settings()
            .clone();
        let values: Vec<_> = self
            .fields
            .iter()
            .map(|field| field.read(cx).value().to_string())
            .collect();
        settings.max_turns = number(&values[0], FIELDS[0].0)?;
        settings.max_tool_calls = number(&values[1], FIELDS[1].0)?;
        settings.command.default_timeout_seconds = number(&values[2], FIELDS[2].0)?;
        settings.command.max_timeout_seconds = number(&values[3], FIELDS[3].0)?;
        settings.command.read_file_scan_bytes = number(&values[4], FIELDS[4].0)?;
        settings.subagents.max_concurrent = number(&values[5], FIELDS[5].0)?;
        settings.subagents.max_turns = number(&values[6], FIELDS[6].0)?;
        settings.subagents.max_tool_calls = number(&values[7], FIELDS[7].0)?;
        settings.subagents.timeout_seconds = number(&values[8], FIELDS[8].0)?;
        settings.subagents.explore_model = optional(&values[9]);
        settings.subagents.review_model = optional(&values[10]);
        settings.auto_compact = self.auto_compact;
        settings.set_network_allowed(self.network_allowed);
        settings.subagents.enabled = self.subagents_enabled;
        settings.disabled_capabilities = self.disabled.iter().copied().collect();
        settings.disabled_tools = self
            .tools
            .read(cx)
            .value()
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect();
        anyhow::ensure!(
            (1..=128).contains(&settings.subagents.max_concurrent),
            "并行子 Agent 上限必须在 1–128 之间。"
        );
        anyhow::ensure!(
            settings.subagents.timeout_seconds <= 86_400,
            "子 Agent 进度提醒间隔必须在 0–86400 秒之间。"
        );
        anyhow::ensure!(
            settings.command.default_timeout_seconds > 0
                && settings.command.default_timeout_seconds <= settings.command.max_timeout_seconds,
            "默认命令超时必须大于 0，且不能超过最长命令超时。"
        );
        Ok(settings)
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        if self.busy || self.draft.is_none() {
            return;
        }
        let settings = match self.values(cx) {
            Ok(settings) => settings,
            Err(error) => {
                self.error = Some(format!("{error:#}"));
                cx.notify();
                return;
            }
        };
        let draft = self.draft.as_ref().unwrap().clone();
        let path = self.path.clone();
        let auth = self.auth.clone();
        let job = self
            .runtime
            .spawn_blocking(move || draft.save(&path, &auth, settings));
        self.busy = true;
        self.error = None;
        self.saved = false;
        self.task = cx.spawn(async move |owner, cx| {
            let result = job.await;
            let _ = owner.update(cx, |view, cx| {
                view.busy = false;
                match result {
                    Ok(Ok(draft)) => {
                        view.draft = Some(draft);
                        view.saved = true;
                    }
                    Ok(Err(error)) => view.error = Some(format!("{error:#}")),
                    Err(error) => view.error = Some(error.to_string()),
                }
                cx.notify();
            });
        });
        cx.notify();
    }

    fn field(&self, ix: usize, cx: &App) -> AnyElement {
        v_flex()
            .min_w_0()
            .gap_1()
            .child(FIELDS[ix].0)
            .child(
                Input::new(&self.fields[ix])
                    .disabled(self.busy)
                    .aria_label(FIELDS[ix].0),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(FIELDS[ix].1),
            )
            .into_any_element()
    }
}

fn form_fields(settings: &AgentSettings) -> [String; FIELDS.len()] {
    [
        settings.max_turns.to_string(),
        settings.max_tool_calls.to_string(),
        settings
            .command
            .default_timeout_seconds
            .to_string(),
        settings
            .command
            .max_timeout_seconds
            .to_string(),
        settings
            .command
            .read_file_scan_bytes
            .to_string(),
        settings
            .subagents
            .max_concurrent
            .to_string(),
        settings.subagents.max_turns.to_string(),
        settings
            .subagents
            .max_tool_calls
            .to_string(),
        settings
            .subagents
            .timeout_seconds
            .to_string(),
        settings
            .subagents
            .explore_model
            .clone()
            .unwrap_or_default(),
        settings
            .subagents
            .review_model
            .clone()
            .unwrap_or_default(),
    ]
}

fn same_form_values(current: &AgentSettings, saved: &AgentSettings) -> bool {
    form_fields(current) == form_fields(saved)
        && current.auto_compact == saved.auto_compact
        && current.command.network.is_allowed() == saved.command.network.is_allowed()
        && current.subagents.enabled == saved.subagents.enabled
        && current
            .disabled_capabilities
            .iter()
            .collect::<BTreeSet<_>>()
            == saved
                .disabled_capabilities
                .iter()
                .collect::<BTreeSet<_>>()
        && current.disabled_tools == saved.disabled_tools
}

fn number<T: std::str::FromStr>(value: &str, label: &str) -> anyhow::Result<T> {
    value
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("{label}必须为非负整数。"))
}

fn optional(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

impl Render for Advanced {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let unavailable = self.busy || self.draft.is_none();
        let owner = cx.weak_entity();
        let compact_owner = owner.clone();
        let network_owner = owner.clone();
        let subagents_owner = owner.clone();
        let actions = h_flex()
            .gap_2()
            .flex_wrap()
            .child(
                Button::new("advanced-save")
                    .primary()
                    .label("保存设置")
                    .loading(self.busy)
                    .disabled(unavailable)
                    .on_click(cx.listener(|view, _, _, cx| view.save(cx))),
            )
            .child(
                Button::new("advanced-reload")
                    .outline()
                    .label(if self.draft.is_some() {
                        "放弃编辑并重新载入"
                    } else {
                        "重新载入"
                    })
                    .disabled(self.busy || self.auth.as_os_str().is_empty())
                    .on_click(cx.listener(|view, _, _, cx| view.load(cx))),
            );
        let body = v_flex().w_full().min_w_0().gap_6().child(
            v_flex()
                .gap_2()
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("保存后用于新建会话；已打开的会话保留当前设置。连接到远程服务时，客户端也会携带这份配置。"),
                )
                .child(actions)
                .when(self.saved, |view| {
                    view.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("已保存。新建会话时使用新设置。"),
                    )
                })
                .when_some(self.error.clone(), |view, error| {
                    view.child(div().text_sm().text_color(cx.theme().danger).child(error))
                }),
        );
        if self.draft.is_none() {
            return body;
        }
        body.child(
            section("运行预算", "达到上限后停止当前运行；0 表示不限。", cx)
                .children((0..2).map(|ix| self.field(ix, cx)))
                .child(
                    Checkbox::new("advanced-auto-compact")
                        .label("上下文接近上限时自动压缩")
                        .checked(self.auto_compact)
                        .disabled(unavailable)
                        .on_click(move |checked, _, cx| {
                            let _ = compact_owner.update(cx, |view, cx| {
                                view.auto_compact = *checked;
                                view.edited(cx);
                            });
                        }),
                ),
        )
        .child(
            section("命令与文件读取", "这些上限用于命令执行与文件读取工具。", cx)
                .children((2..5).map(|ix| self.field(ix, cx)))
                .child(
                    Checkbox::new("advanced-command-network")
                        .label("允许命令声明联网")
                        .checked(self.network_allowed)
                        .disabled(unavailable)
                        .on_click(move |checked, _, cx| {
                            let _ = network_owner.update(cx, |view, cx| {
                                view.network_allowed = *checked;
                                view.edited(cx);
                            });
                        }),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("联网声明用于命令审批，不限制原生进程实际访问网络。"),
                ),
        )
        .child(
            section("子 Agent", "探索与审查模型留空时继承父会话模型与思考档位。", cx)
                .child(
                    Checkbox::new("advanced-subagents-enabled")
                        .label("启用子 Agent")
                        .checked(self.subagents_enabled)
                        .disabled(unavailable)
                        .on_click(move |checked, _, cx| {
                            let _ = subagents_owner.update(cx, |view, cx| {
                                view.subagents_enabled = *checked;
                                view.edited(cx);
                            });
                        }),
                )
                .children((5..11).map(|ix| self.field(ix, cx))),
        )
        .child(
            section("禁用能力", "选中的能力不会提供给 Agent。它们会与会话自身的限制合并，能力禁用优先于上方功能开关。", cx)
                .child(v_flex()
                    .gap_3()
                    .children(CAPABILITIES
                    .into_iter()
                    .map(|(capability,
                    key,
                    label)| {
                    let owner = owner.clone();
                    let all = self.disabled.contains(&Capability::All);
                    Checkbox::new(SharedString::from(format!("advanced-disabled:{key}")))
                        .label(label)
                        .checked(self.disabled.contains(&capability) || all)
                        .disabled(unavailable || (all && capability != Capability::All))
                        .on_click(move |checked, _, cx| {
                            let _ = owner.update(cx, |view, cx| {
                                if *checked {
                                    view.disabled.insert(capability);
                                } else {
                                    view.disabled.remove(&capability);
                                }
                                view.edited(cx);
                            });
                        })
                }))),
        )
        .child(
            section("禁用指定工具", "每行填写一个完整工具名，按名称精确匹配；留空不增加限制。MCP 工具使用 mcp__服务器__工具 格式。", cx)
                .child(Textarea::new(&self.tools).disabled(unavailable).aria_label("禁用指定工具")),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{AgentSettings, Capability, same_form_values};

    fn saved_settings() -> AgentSettings {
        AgentSettings {
            max_turns: 0,
            max_tool_calls: 2_000,
            auto_compact: true,
            command: Default::default(),
            subagents: Default::default(),
            disabled_capabilities: vec![Capability::Write, Capability::Web],
            disabled_tools: vec!["read_file".into()],
        }
    }

    #[test]
    fn uncommitted_agent_edits_remain_protected_until_saved_or_reverted() {
        let mut saved = saved_settings();
        let mut form = saved.clone();
        form.disabled_capabilities.reverse();
        assert!(same_form_values(&form, &saved));

        form.max_tool_calls = 42;
        assert!(!same_form_values(&form, &saved));
        saved = form.clone();
        assert!(same_form_values(&form, &saved));

        form.subagents.explore_model = Some("local::model".into());
        assert!(!same_form_values(&form, &saved));
        form = saved.clone();
        form.auto_compact = !saved.auto_compact;
        assert!(!same_form_values(&form, &saved));
        form = saved.clone();
        form.disabled_tools.clear();
        assert!(!same_form_values(&form, &saved));
        form = saved.clone();
        assert!(same_form_values(&form, &saved));
    }
}
