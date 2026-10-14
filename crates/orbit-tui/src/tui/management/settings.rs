//! Client-owned configuration. Remote sessions never turn these writes into remote file edits.
use super::resources::{editor_area, field, row};
use super::{Action, Panel};
use crate::{
    tui::{components, connect_custom::form, input::InputState, render::safe_output, theme::THEME},
    workspace::protocol::Response,
};
use anyhow::{Result, ensure};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use orbit_config::config::{
    advanced::{AdvancedDraft, AgentSettings},
    connections::mcp::{McpDraft, environment},
};
use orbit_protocol::Capability;
use orbit_tools::mcp::McpServerConfig;
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::Style,
    widgets::Paragraph,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

const LABELS: [&str; 11] = [
    "会话轮数上限（0 不限）",
    "工具调用上限（0 不限）",
    "默认命令超时（秒）",
    "最长命令超时（秒）",
    "文件扫描上限（字节，0 不限）",
    "并行子 Agent 上限（1～128）",
    "子 Agent 轮数上限（0 不限）",
    "子 Agent 工具调用上限",
    "子 Agent 进度提醒（秒，0 关闭）",
    "探索子 Agent 模型（空白继承）",
    "审查子 Agent 模型（空白继承）",
];
const CAPABILITIES: [(Capability, &str); 14] = [
    (Capability::All, "全部可选工具"),
    (Capability::Subagents, "子 Agent"),
    (Capability::Web, "网页搜索与抓取"),
    (Capability::Mcp, "MCP"),
    (Capability::Skills, "技能"),
    (Capability::Image, "图片"),
    (Capability::Document, "文档"),
    (Capability::Files, "文件读取与搜索"),
    (Capability::Git, "Git"),
    (Capability::Write, "文件写入"),
    (Capability::Capability, "工作区外路径授权"),
    (Capability::Command, "命令执行"),
    (Capability::History, "归档上下文"),
    (Capability::ProjectState, "任务与记忆"),
];
const MCP_LABELS: [&str; 7] = [
    "名称",
    "启动命令",
    "参数（每行一个）",
    "环境变量（KEY=value）",
    "超时（秒）",
    "允许的工具（空白全部）",
    "启用服务器",
];
const AGENT_FIELDS: usize = 29;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Agent,
    Mcp,
}

#[derive(Clone, Copy, Debug)]
enum Hit {
    Agent,
    Mcp,
    Save,
    Reload,
    ServerList,
    Resume,
    New,
    Edit,
    Remove,
    Toggle,
    Server(usize),
    AgentField(usize),
    McpField(usize),
    ConfirmRemove,
    CancelRemove,
}

struct AgentForm {
    fields: [InputState; 11],
    tools: InputState,
    compact: bool,
    network: bool,
    subagents: bool,
    disabled: BTreeSet<Capability>,
    focus: usize,
    offset: usize,
    reveal: bool,
    tools_offset: usize,
    tools_follow: bool,
}

impl AgentForm {
    fn new(settings: &AgentSettings) -> Self {
        let values = [
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
        ];
        let fields = values.map(input);
        Self {
            fields,
            tools: input(settings.disabled_tools.join("\n")),
            compact: settings.auto_compact,
            network: settings.command.network.is_allowed(),
            subagents: settings.subagents.enabled,
            disabled: settings
                .disabled_capabilities
                .iter()
                .copied()
                .collect(),
            focus: 0,
            offset: 0,
            reveal: true,
            tools_offset: 0,
            tools_follow: true,
        }
    }

    fn settings(&self, base: &AgentSettings) -> Result<AgentSettings> {
        let mut settings = base.clone();
        settings.max_turns = number(&self.fields[0].text, LABELS[0])?;
        settings.max_tool_calls = number(&self.fields[1].text, LABELS[1])?;
        settings.command.default_timeout_seconds = number(&self.fields[2].text, LABELS[2])?;
        settings.command.max_timeout_seconds = number(&self.fields[3].text, LABELS[3])?;
        settings.command.read_file_scan_bytes = number(&self.fields[4].text, LABELS[4])?;
        settings.subagents.max_concurrent = number(&self.fields[5].text, LABELS[5])?;
        settings.subagents.max_turns = number(&self.fields[6].text, LABELS[6])?;
        settings.subagents.max_tool_calls = number(&self.fields[7].text, LABELS[7])?;
        settings.subagents.timeout_seconds = number(&self.fields[8].text, LABELS[8])?;
        settings.subagents.explore_model = optional(&self.fields[9].text);
        settings.subagents.review_model = optional(&self.fields[10].text);
        settings.auto_compact = self.compact;
        settings.set_network_allowed(self.network);
        settings.subagents.enabled = self.subagents;
        settings.disabled_capabilities = self.disabled.iter().copied().collect();
        settings.disabled_tools = self
            .tools
            .text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect();
        Ok(settings)
    }

    fn toggle(&mut self) {
        match self.focus {
            11 => self.compact = !self.compact,
            12 => self.network = !self.network,
            13 => self.subagents = !self.subagents,
            14..=27 => {
                let capability = CAPABILITIES[self.focus - 14].0;
                if !self.disabled.remove(&capability) {
                    self.disabled.insert(capability);
                }
            }
            _ => {}
        }
    }
}

struct McpForm {
    existing: Option<String>,
    fields: [InputState; 6],
    enabled: bool,
    focus: usize,
    offset: usize,
    follow: bool,
}

impl McpForm {
    fn new(name: Option<&str>, server: Option<&McpServerConfig>) -> Self {
        let values = [
            name.unwrap_or_default().into(),
            server.map_or_else(String::new, |s| s.command.clone()),
            server.map_or_else(String::new, |s| s.args.join("\n")),
            server.map_or_else(String::new, |server| environment::format(&server.env)),
            server.map_or_else(|| "120".into(), |s| s.timeout_seconds.to_string()),
            server.map_or_else(String::new, |s| s.tools.join("\n")),
        ];
        Self {
            existing: name.map(str::to_owned),
            fields: values.map(input),
            enabled: server.is_some_and(|s| s.enabled),
            focus: usize::from(name.is_some()),
            offset: 0,
            follow: true,
        }
    }

    fn server(&self) -> Result<(String, McpServerConfig)> {
        let name = self.fields[0].text.trim().to_owned();
        let env = environment::parse(&self.fields[3].text)?;
        let server = McpServerConfig {
            transport: "stdio".into(),
            command: self.fields[1].text.trim().into(),
            args: self.fields[2]
                .text
                .lines()
                .map(str::to_owned)
                .collect(),
            env,
            timeout_seconds: number(&self.fields[4].text, "MCP 超时")?,
            tools: self.fields[5]
                .text
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(str::to_owned)
                .collect(),
            enabled: self.enabled,
        };
        Ok((name, server))
    }
}

pub(super) struct State {
    path: PathBuf,
    auth: PathBuf,
    page: Page,
    agent: Option<AdvancedDraft>,
    agent_form: Option<AgentForm>,
    mcp: Option<McpDraft>,
    mcp_form: Option<McpForm>,
    mcp_drafts: BTreeMap<Option<String>, McpForm>,
    editing_mcp: bool,
    selected_server: usize,
    server_offset: usize,
    removing: Option<String>,
    error: Option<String>,
    message: Option<String>,
    hits: Vec<(Rect, Hit)>,
    body: Rect,
    editor: Rect,
}

impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Settings")
            .field("path", &self.path)
            .field("page", &self.page)
            .field("editing_mcp", &self.editing_mcp)
            .finish_non_exhaustive()
    }
}

impl State {
    pub(super) fn new(path: PathBuf, auth: PathBuf, mcp: bool) -> Self {
        let mut state = Self {
            path,
            auth,
            page: if mcp { Page::Mcp } else { Page::Agent },
            agent: None,
            agent_form: None,
            mcp: None,
            mcp_form: None,
            mcp_drafts: BTreeMap::new(),
            editing_mcp: false,
            selected_server: 0,
            server_offset: 0,
            removing: None,
            error: None,
            message: None,
            hits: Vec::new(),
            body: Rect::default(),
            editor: Rect::default(),
        };
        state.reload();
        state
    }

    /// Refresh snapshots explicitly, retaining all form values. This is also the recovery path for
    /// CAS failures.
    fn reload(&mut self) {
        let result: Result<()> = match self.page {
            Page::Agent => AdvancedDraft::load(&self.path, &self.auth).map(|draft| {
                if self.agent_form.is_none() {
                    self.agent_form = Some(AgentForm::new(draft.settings()));
                }
                self.agent = Some(draft);
            }),
            Page::Mcp => McpDraft::load(&self.path).map(|draft| self.mcp = Some(draft)),
        };
        match result {
            Ok(()) => {
                self.error = None;
                self.message = Some("已刷新配置原稿；编辑草稿保留。核对后可保存。".into());
            }
            Err(error) => self.error = Some(format!("{error:#}")),
        }
        self.invalidate_geometry();
    }

    fn switch(&mut self, page: Page) {
        self.page = page;
        self.error = None;
        self.message = None;
        self.removing = None;
        match page {
            Page::Agent if self.agent.is_none() => self.reload(),
            Page::Mcp if self.mcp.is_none() => self.reload(),
            _ => {}
        }
        self.invalidate_geometry();
    }

    fn server_names(&self) -> Vec<String> {
        self.mcp
            .as_ref()
            .map(|draft| {
                draft
                    .servers()
                    .keys()
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn edit_mcp(&mut self, create: bool) {
        let name = if create {
            None
        } else {
            self.server_names()
                .get(self.selected_server)
                .cloned()
        };
        if !create && name.is_none() {
            return;
        }
        if !self
            .mcp_form
            .as_ref()
            .is_some_and(|form| form.existing == name)
        {
            if let Some(previous) = self.mcp_form.take() {
                self.mcp_drafts
                    .insert(previous.existing.clone(), previous);
            }
            if let Some(draft) = self.mcp_drafts.remove(&name) {
                self.mcp_form = Some(draft);
            } else {
                let server = self.mcp.as_ref().and_then(|draft| {
                    name.as_ref()
                        .and_then(|name| draft.servers().get(name))
                });
                self.mcp_form = Some(McpForm::new(name.as_deref(), server));
            }
        }
        self.editing_mcp = true;
        self.error = None;
        self.message = None;
        self.invalidate_geometry();
    }

    fn save(&mut self) {
        let result: Result<()> = match self.page {
            Page::Agent => (|| {
                let draft = self
                    .agent
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("请先刷新配置。"))?;
                let form = self
                    .agent_form
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("请先载入设置。"))?;
                self.agent =
                    Some(draft.save(&self.path, &self.auth, form.settings(draft.settings())?)?);
                Ok(())
            })(),
            Page::Mcp => (|| {
                let draft = self
                    .mcp
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("请先刷新配置。"))?;
                let form = self
                    .mcp_form
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("请选择或新建服务器。"))?;
                let (name, server) = form.server()?;
                ensure!(
                    form.existing.is_some() || !draft.servers().contains_key(&name),
                    "服务器名称已存在，请使用其他名称。"
                );
                self.mcp = Some(draft.save(&self.path, &name, Some(server))?);
                self.editing_mcp = false;
                self.mcp_form = None;
                Ok(())
            })(),
        };
        match result {
            Ok(()) => {
                self.error = None;
                self.message =
                    Some("已保存。新建会话时使用新设置；已打开的会话保留原设置。".into());
            }
            Err(error) => {
                self.error = Some(format!(
                    "{error:#} · 草稿已保留，可刷新原稿后核对并重新保存。"
                ));
                self.message = None;
            }
        }
        self.invalidate_geometry();
    }

    fn remove(&mut self, name: String) {
        let result = self
            .mcp
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("请先刷新配置。"))
            .and_then(|draft| draft.save(&self.path, &name, None));
        match result {
            Ok(draft) => {
                self.mcp = Some(draft);
                self.removing = None;
                self.error = None;
                self.message = Some("服务器配置已移除，新建会话时生效。".into());
            }
            Err(error) => self.error = Some(format!("{error:#}")),
        }
        self.invalidate_geometry();
    }

    fn toggle_server(&mut self) {
        let names = self.server_names();
        let Some(name) = names.get(self.selected_server) else {
            return;
        };
        let Some(draft) = &self.mcp else {
            return;
        };
        let Some(mut server) = draft.servers().get(name).cloned() else {
            return;
        };
        server.enabled = !server.enabled;
        match draft.save(&self.path, name, Some(server)) {
            Ok(draft) => {
                self.mcp = Some(draft);
                self.error = None;
                self.message = Some("启用状态已保存，新建会话时生效。".into());
            }
            Err(error) => self.error = Some(format!("{error:#}")),
        }
        self.invalidate_geometry();
    }

    fn activate(&mut self, hit: Hit) -> Action {
        match hit {
            Hit::Agent => self.switch(Page::Agent),
            Hit::Mcp => self.switch(Page::Mcp),
            Hit::Save => self.save(),
            Hit::Reload => self.reload(),
            Hit::ServerList => self.editing_mcp = false,
            Hit::Resume => self.editing_mcp = self.mcp_form.is_some(),
            Hit::New => self.edit_mcp(true),
            Hit::Edit => self.edit_mcp(false),
            Hit::Toggle => self.toggle_server(),
            Hit::Remove => {
                self.removing = self
                    .server_names()
                    .get(self.selected_server)
                    .cloned()
            }
            Hit::Server(index) => self.selected_server = index,
            Hit::AgentField(index) => {
                if let Some(form) = &mut self.agent_form {
                    form.focus = index;
                    form.reveal = true;
                    form.tools_follow = true;
                    if (11..28).contains(&index) {
                        form.toggle();
                    }
                }
            }
            Hit::McpField(index) => {
                if let Some(form) = &mut self.mcp_form {
                    form.focus = index;
                    form.offset = 0;
                    form.follow = true;
                    if index == 6 {
                        form.enabled = !form.enabled;
                    }
                }
            }
            Hit::ConfirmRemove => {
                if let Some(name) = self.removing.clone() {
                    self.remove(name);
                }
            }
            Hit::CancelRemove => self.removing = None,
        }
        Action::None
    }

    fn agent_input(&mut self, event: &Event) {
        let Some(form) = &mut self.agent_form else {
            return;
        };
        if let Event::Key(key) = event {
            if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
                form.focus = if key.code == KeyCode::BackTab
                    || key
                        .modifiers
                        .contains(KeyModifiers::SHIFT)
                {
                    (form.focus + AGENT_FIELDS - 1) % AGENT_FIELDS
                } else {
                    (form.focus + 1) % AGENT_FIELDS
                };
                form.reveal = true;
                form.tools_follow = true;
                return;
            }
            if form.focus < 11 {
                match key.code {
                    KeyCode::Up => form.focus = form.focus.saturating_sub(1),
                    KeyCode::Down | KeyCode::Enter => {
                        form.focus = (form.focus + 1).min(AGENT_FIELDS - 1)
                    }
                    _ => form::edit(&mut form.fields[form.focus], key.code, key.modifiers),
                }
                form.reveal = true;
            } else if form.focus == 28 {
                form.tools_follow = true;
                if key.code == KeyCode::Enter {
                    form.tools.insert("\n");
                } else {
                    form.tools
                        .edit(event, self.body.width.max(1) as usize);
                }
                form.reveal = true;
            } else {
                match key.code {
                    KeyCode::Char(' ') | KeyCode::Enter => form.toggle(),
                    KeyCode::Up => {
                        form.focus = form.focus.saturating_sub(1);
                        form.reveal = true;
                    }
                    KeyCode::Down => {
                        form.focus = (form.focus + 1).min(AGENT_FIELDS - 1);
                        form.reveal = true;
                    }
                    _ => {}
                }
            }
        } else if let Event::Paste(text) = event {
            if form.focus < 11 {
                form::paste(&mut form.fields[form.focus], text);
            } else if form.focus == 28 {
                form.tools.insert(text);
                form.tools_follow = true;
            }
        }
    }

    fn mcp_input(&mut self, event: &Event) {
        let Some(form) = &mut self.mcp_form else {
            return;
        };
        if let Event::Key(key) = event {
            if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
                let first = usize::from(form.existing.is_some());
                let count = 7 - first;
                let relative = form.focus.saturating_sub(first);
                form.focus = first
                    + if key.code == KeyCode::BackTab
                        || key
                            .modifiers
                            .contains(KeyModifiers::SHIFT)
                    {
                        (relative + count - 1) % count
                    } else {
                        (relative + 1) % count
                    };
                form.offset = 0;
                form.follow = true;
                return;
            }
            if form.focus == 6 {
                if matches!(key.code, KeyCode::Enter | KeyCode::Char(' ')) {
                    form.enabled = !form.enabled;
                }
                return;
            }
            if form.focus == 0 && form.existing.is_some() {
                return;
            }
            if matches!(form.focus, 2 | 3 | 5) {
                form.follow = true;
                if key.code == KeyCode::Enter {
                    form.fields[form.focus].insert("\n");
                } else {
                    form.fields[form.focus].edit(event, self.editor.width.max(1) as usize);
                }
            } else if key.code == KeyCode::Enter {
                form.focus = (form.focus + 1).min(6);
                form.offset = 0;
            } else {
                form::edit(&mut form.fields[form.focus], key.code, key.modifiers);
            }
        } else if let Event::Paste(text) = event
            && form.focus < 6
            && !(form.focus == 0 && form.existing.is_some())
        {
            if matches!(form.focus, 2 | 3 | 5) {
                form.fields[form.focus].insert(text);
                form.follow = true;
            } else {
                form::paste(&mut form.fields[form.focus], text);
            }
        }
    }
}

impl Panel for State {
    fn handle(&mut self, event: &Event) -> Action {
        if let Event::Key(key) = event {
            if key.kind == KeyEventKind::Release {
                return Action::None;
            }
            if self.removing.is_some() {
                return match key.code {
                    KeyCode::Esc => self.activate(Hit::CancelRemove),
                    KeyCode::Enter => self.activate(Hit::ConfirmRemove),
                    _ => Action::None,
                };
            }
            if key.code == KeyCode::Esc {
                return Action::Close;
            }
            if key
                .modifiers
                .contains(KeyModifiers::CONTROL)
            {
                match key.code {
                    KeyCode::Char('s') => return self.activate(Hit::Save),
                    KeyCode::Char('r') => return self.activate(Hit::Reload),
                    KeyCode::Char('b') if self.page == Page::Mcp => {
                        return self.activate(Hit::ServerList);
                    }
                    _ => {}
                }
            }
            if matches!(key.code, KeyCode::F(1)) {
                return self.activate(Hit::Agent);
            }
            if matches!(key.code, KeyCode::F(2)) {
                return self.activate(Hit::Mcp);
            }
            if self.page == Page::Mcp && !self.editing_mcp {
                match key.code {
                    KeyCode::Up => self.selected_server = self.selected_server.saturating_sub(1),
                    KeyCode::Down => {
                        self.selected_server = (self.selected_server + 1).min(
                            self.server_names()
                                .len()
                                .saturating_sub(1),
                        )
                    }
                    KeyCode::Char('n') => return self.activate(Hit::New),
                    KeyCode::Char('e') | KeyCode::Enter => return self.activate(Hit::Edit),
                    KeyCode::Char('d') | KeyCode::Delete => return self.activate(Hit::Remove),
                    KeyCode::Char(' ') => return self.activate(Hit::Toggle),
                    KeyCode::Char('r') => return self.activate(Hit::Reload),
                    KeyCode::Char('c') => return self.activate(Hit::Resume),
                    _ => {}
                }
            } else if self.page == Page::Agent {
                self.agent_input(event);
            } else {
                self.mcp_input(event);
            }
        } else if let Event::Paste(_) = event {
            if self.page == Page::Agent {
                self.agent_input(event);
            } else if self.editing_mcp {
                self.mcp_input(event);
            }
        } else if let Event::Mouse(mouse) = event {
            let point = Position::new(mouse.column, mouse.row);
            if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
                if let Some((_, hit)) = self
                    .hits
                    .iter()
                    .find(|(area, _)| area.contains(point))
                    .copied()
                {
                    return self.activate(hit);
                }
            } else if self.body.contains(point)
                && matches!(
                    mouse.kind,
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                )
            {
                let down = mouse.kind == MouseEventKind::ScrollDown;
                if self.page == Page::Agent {
                    if let Some(form) = &mut self.agent_form {
                        if self.editor.contains(point) && form.focus == 28 {
                            form.tools_offset = if down {
                                form.tools_offset.saturating_add(3)
                            } else {
                                form.tools_offset.saturating_sub(3)
                            };
                            form.tools_follow = false;
                            return Action::None;
                        }
                        form.offset = if down {
                            form.offset.saturating_add(3)
                        } else {
                            form.offset.saturating_sub(3)
                        };
                        form.reveal = false;
                    }
                } else if self.editing_mcp {
                    if let Some(form) = &mut self.mcp_form {
                        form.offset = if down {
                            form.offset.saturating_add(3)
                        } else {
                            form.offset.saturating_sub(3)
                        };
                        form.follow = false;
                    }
                } else {
                    self.selected_server = if down {
                        (self.selected_server + 3).min(
                            self.server_names()
                                .len()
                                .saturating_sub(1),
                        )
                    } else {
                        self.selected_server.saturating_sub(3)
                    };
                }
            }
        }
        Action::None
    }

    fn receive(&mut self, _: Response) -> Action {
        self.error("本地设置收到意外的服务响应，草稿已保留。".into());
        Action::None
    }

    fn error(&mut self, error: String) {
        self.error = Some(error);
        self.invalidate_geometry();
    }

    fn draw(&mut self, frame: &mut Frame<'_>, area: Rect) {
        self.invalidate_geometry();
        if area.width < 28 || area.height < 10 {
            frame.render_widget(
                Paragraph::new("请扩大窗口 · Esc 收起").style(components::muted()),
                area,
            );
            return;
        }
        frame.render_widget(
            Paragraph::new("客户端设置 · 新建会话时生效").style(components::accent()),
            row(area, 0),
        );
        let mut buttons = vec![
            ("Agent F1", Hit::Agent),
            ("MCP F2", Hit::Mcp),
            ("刷新原稿 Ctrl+R", Hit::Reload),
        ];
        if self.page == Page::Agent || self.editing_mcp {
            buttons.push(("保存 Ctrl+S", Hit::Save));
            if self.page == Page::Mcp {
                buttons.push(("服务器列表 Ctrl+B", Hit::ServerList));
            }
        } else {
            buttons.extend([
                ("新增 N", Hit::New),
                ("编辑 E", Hit::Edit),
                ("删除 D", Hit::Remove),
            ]);
            if self.mcp_form.is_some() {
                buttons.push(("继续草稿 C", Hit::Resume));
            }
        }
        toolbar(frame, row(area, 1), &buttons, &mut self.hits);
        let status = self
            .error
            .as_deref()
            .or(self.message.as_deref())
            .unwrap_or_default();
        frame.render_widget(
            Paragraph::new(safe_output(status)).style(Style::default().fg(
                if self.error.is_some() {
                    THEME.error
                } else {
                    THEME.muted
                },
            )),
            row(area, area.height - 2),
        );
        frame.render_widget(
            Paragraph::new(if self.page == Page::Mcp && !self.editing_mcp {
                "↑↓ 选择 · Enter 编辑 · Space 启停 · D 删除 · Esc 收起"
            } else {
                "Tab 切换字段 · Space 切换开关 · Ctrl+S 保存 · Esc 保留草稿"
            })
            .style(components::muted()),
            row(area, area.height - 1),
        );
        self.body = Rect::new(area.x, area.y + 3, area.width, area.height - 5);
        if let Some(name) = &self.removing {
            frame.render_widget(
                Paragraph::new(format!(
                    "移除 MCP 服务器 {}？\n当前已打开的会话不受影响。",
                    safe_output(name)
                ))
                .style(components::normal()),
                self.body,
            );
            let y = self.body.height.saturating_sub(2);
            toolbar(
                frame,
                row(self.body, y),
                &[
                    ("确认移除 Enter", Hit::ConfirmRemove),
                    ("取消 Esc", Hit::CancelRemove),
                ],
                &mut self.hits,
            );
        } else if self.page == Page::Agent {
            self.draw_agent(frame);
        } else if self.editing_mcp {
            self.draw_mcp_editor(frame);
        } else {
            self.draw_mcp_list(frame);
        }
    }

    fn invalidate_geometry(&mut self) {
        self.hits.clear();
        self.body = Rect::default();
        self.editor = Rect::default();
    }
}

impl State {
    fn draw_agent(&mut self, frame: &mut Frame<'_>) {
        let area = self.body;
        let Some(form) = &mut self.agent_form else {
            frame.render_widget(
                Paragraph::new("设置暂不可用 · Ctrl+R 重试").style(components::muted()),
                area,
            );
            return;
        };
        let tools_start = 11 * 2 + 3 + 14;
        let total = tools_start + 7;
        let focus_row = match form.focus {
            0..=10 => form.focus * 2 + 1,
            11..=13 => 22 + form.focus - 11,
            14..=27 => 25 + form.focus - 14,
            _ => tools_start + 6,
        };
        if form.reveal {
            form.offset = form
                .offset
                .min(focus_row)
                .max(focus_row.saturating_sub(area.height as usize - 1));
            form.reveal = false;
        }
        form.offset = form
            .offset
            .min(total.saturating_sub(area.height as usize));
        for (index, label) in LABELS.iter().enumerate() {
            if let Some(rect) = virtual_row(area, form.offset, index * 2) {
                frame.render_widget(Paragraph::new(*label).style(components::muted()), rect);
            }
            if let Some(rect) = virtual_row(area, form.offset, index * 2 + 1) {
                field(
                    frame,
                    rect,
                    &form.fields[index],
                    form.focus == index,
                    if index >= 9 {
                        "继承父会话"
                    } else {
                        "整数"
                    },
                );
                self.hits
                    .push((rect, Hit::AgentField(index)));
            }
        }
        for (index, (label, checked)) in [
            ("自动压缩上下文", form.compact),
            ("允许命令声明联网（用于审批）", form.network),
            ("启用子 Agent", form.subagents),
        ]
        .iter()
        .enumerate()
        {
            if let Some(rect) = virtual_row(area, form.offset, 22 + index) {
                frame.render_widget(
                    Paragraph::new(format!("{} {label}", check(*checked))).style(
                        if form.focus == index + 11 {
                            components::highlighted()
                        } else {
                            components::normal()
                        },
                    ),
                    rect,
                );
                self.hits
                    .push((rect, Hit::AgentField(index + 11)));
            }
        }
        for (index, (capability, label)) in CAPABILITIES.iter().enumerate() {
            if let Some(rect) = virtual_row(area, form.offset, 25 + index) {
                let checked =
                    form.disabled.contains(capability) || form.disabled.contains(&Capability::All);
                frame.render_widget(
                    Paragraph::new(format!("{} 禁用 {label}", check(checked))).style(
                        if form.focus == index + 14 {
                            components::highlighted()
                        } else {
                            components::normal()
                        },
                    ),
                    rect,
                );
                self.hits
                    .push((rect, Hit::AgentField(index + 14)));
            }
        }
        if let Some(rect) = virtual_row(area, form.offset, tools_start) {
            frame.render_widget(
                Paragraph::new("禁用指定工具（每行一个完整名称）").style(components::muted()),
                rect,
            );
        }
        let start = tools_start + 1;
        let visible_start = start.max(form.offset);
        let visible_end = (start + 6).min(form.offset + area.height as usize);
        if visible_start < visible_end {
            let rect = Rect::new(
                area.x,
                area.y + (visible_start - form.offset) as u16,
                area.width,
                (visible_end - visible_start) as u16,
            );
            editor_area(
                frame,
                rect,
                &form.tools,
                &mut form.tools_offset,
                form.focus == 28,
                std::mem::take(&mut form.tools_follow),
            );
            self.editor = rect;
            self.hits
                .push((rect, Hit::AgentField(28)));
        }
    }

    fn draw_mcp_list(&mut self, frame: &mut Frame<'_>) {
        let area = self.body;
        frame.render_widget(
            Paragraph::new(format!(
                "配置文件：{}",
                safe_output(&self.path.display().to_string())
            ))
            .style(components::muted()),
            row(area, 0),
        );
        let names = self.server_names();
        let height = (area.height.saturating_sub(2) as usize / 2).max(1);
        self.selected_server = self
            .selected_server
            .min(names.len().saturating_sub(1));
        self.server_offset = self
            .server_offset
            .min(self.selected_server)
            .max(
                self.selected_server
                    .saturating_sub(height - 1),
            );
        for (index, name) in names
            .iter()
            .enumerate()
            .skip(self.server_offset)
            .take(height)
        {
            let server = &self.mcp.as_ref().unwrap().servers()[name];
            let offset = ((index - self.server_offset) * 2) as u16 + 2;
            if offset >= area.height {
                break;
            }
            let rect = Rect::new(
                area.x,
                area.y + offset,
                area.width,
                2.min(area.height - offset),
            );
            frame.render_widget(
                Paragraph::new(format!("{} {}", check(server.enabled), safe_output(name))).style(
                    if index == self.selected_server {
                        components::highlighted()
                    } else {
                        components::normal()
                    },
                ),
                row(rect, 0),
            );
            if rect.height > 1 {
                frame.render_widget(
                    Paragraph::new(format!(
                        "{} · 超时 {} 秒 · {}",
                        safe_output(&server.command),
                        server.timeout_seconds,
                        if server.tools.is_empty() {
                            "全部工具".into()
                        } else {
                            format!("{} 个允许工具", server.tools.len())
                        }
                    ))
                    .style(components::muted()),
                    row(rect, 1),
                );
            }
            self.hits
                .push((rect, Hit::Server(index)));
        }
        if names.is_empty() {
            frame.render_widget(
                Paragraph::new("还没有 MCP 服务器 · N 新增（默认不启用）")
                    .style(components::muted()),
                row(area, 2),
            );
        }
    }

    fn draw_mcp_editor(&mut self, frame: &mut Frame<'_>) {
        let area = self.body;
        let Some(form) = &mut self.mcp_form else {
            return;
        };
        let left_width = if area.width >= 64 { 25 } else { 0 };
        if left_width > 0 {
            for (index, label) in MCP_LABELS.iter().enumerate() {
                let offset = index as u16 * 2;
                if offset >= area.height {
                    break;
                }
                let rect = Rect::new(area.x, area.y + offset, left_width, 1);
                frame.render_widget(
                    Paragraph::new(*label).style(if form.focus == index {
                        components::highlighted()
                    } else {
                        components::muted()
                    }),
                    rect,
                );
                self.hits
                    .push((rect, Hit::McpField(index)));
            }
        }
        let right = Rect::new(
            area.x + left_width,
            area.y,
            area.width - left_width,
            area.height,
        );
        frame.render_widget(
            Paragraph::new(format!(
                "{}  ({}/7)",
                MCP_LABELS[form.focus],
                form.focus + 1
            ))
            .style(components::accent()),
            row(right, 0),
        );
        let hint = match form.focus {
            0 => "ASCII 字母、数字、_ 或 -；已有服务器名称不可更改",
            1 => "例如 npx 或 /usr/bin/server",
            2 => "每行一个参数，不需要 shell 引号",
            3 => "每行一个 KEY=value；不允许重复或受保护变量",
            4 => "正整数（秒）",
            5 => "每行一个工具名；留空允许服务器的全部工具",
            _ => "Space 切换。启用后会在新建会话中启动该 MCP 进程。",
        };
        frame.render_widget(
            Paragraph::new(hint).style(components::muted()),
            row(right, 1),
        );
        self.editor = Rect::new(
            right.x,
            right.y + 3,
            right.width,
            right.height.saturating_sub(3),
        );
        if form.focus == 6 {
            frame.render_widget(
                Paragraph::new(format!("{} 启用服务器", check(form.enabled)))
                    .style(components::highlighted()),
                self.editor,
            );
        } else if matches!(form.focus, 2 | 3 | 5) {
            editor_area(
                frame,
                self.editor,
                &form.fields[form.focus],
                &mut form.offset,
                true,
                std::mem::take(&mut form.follow),
            );
        } else {
            field(
                frame,
                row(self.editor, 0),
                &form.fields[form.focus],
                !(form.focus == 0 && form.existing.is_some()),
                "填写配置值…",
            );
        }
    }
}

fn input(value: String) -> InputState {
    let mut input = InputState::default();
    input.set(value);
    input
}

fn optional(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.into())
}

fn number<T: std::str::FromStr>(value: &str, label: &str) -> Result<T> {
    value
        .trim()
        .parse()
        .map_err(|_| anyhow::anyhow!("{label} 应为非负整数。"))
}

fn check(checked: bool) -> &'static str {
    if checked { "[x]" } else { "[ ]" }
}

fn virtual_row(area: Rect, offset: usize, index: usize) -> Option<Rect> {
    index
        .checked_sub(offset)
        .filter(|row| *row < area.height as usize)
        .map(|row| Rect::new(area.x, area.y + row as u16, area.width, 1))
}

fn toolbar(
    frame: &mut Frame<'_>,
    area: Rect,
    buttons: &[(&str, Hit)],
    hits: &mut Vec<(Rect, Hit)>,
) {
    use unicode_width::UnicodeWidthStr;
    let mut x = area.x;
    for (label, hit) in buttons {
        let width = (label.width() as u16 + 2).min(area.right().saturating_sub(x));
        if width == 0 {
            break;
        }
        let rect = Rect::new(x, area.y, width, area.height);
        components::button(frame, rect, label, components::Control::default());
        hits.push((rect, *hit));
        x = x.saturating_add(width + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state(mcp: bool) -> (tempfile::TempDir, State) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        std::fs::write(
            &path,
            "# retain comment\nmax_turns=0\n[providers]\n[command]\nmax_scan_entr\
            ies=1234\n[command.verification_commands]\ncheck='cargo check'\n",
        )
        .unwrap();
        let state = State::new(path, directory.path().join("auth"), mcp);
        (directory, state)
    }

    #[test]
    fn agent_saves_all_controls_and_preserves_unexposed_settings_and_comments() {
        let (_directory, mut state) = state(false);
        let form = state.agent_form.as_mut().unwrap();
        form.fields[0].set("17".into());
        form.fields[2].set("5".into());
        form.fields[5].set("3".into());
        form.network = false;
        form.disabled.insert(Capability::Write);
        form.tools
            .set("read_file\nmcp__demo__tool".into());
        state.save();
        assert!(state.error.is_none(), "{:?}", state.error);
        let content = std::fs::read_to_string(&state.path).unwrap();
        assert!(content.contains("# retain comment"));
        assert!(content.contains("max_scan_entries=1234"));
        assert!(content.contains("check='cargo check'"));
        let draft = AdvancedDraft::load(&state.path, &state.auth).unwrap();
        assert_eq!(draft.settings().max_turns, 17);
        assert_eq!(
            draft
                .settings()
                .subagents
                .max_concurrent,
            3
        );
        assert!(
            !draft
                .settings()
                .command
                .network
                .is_allowed()
        );
        assert_eq!(draft.settings().disabled_capabilities, [Capability::Write]);
    }

    #[test]
    fn stale_agent_config_preserves_form_and_requires_explicit_snapshot_refresh() {
        let (_directory, mut state) = state(false);
        state
            .agent_form
            .as_mut()
            .unwrap()
            .fields[0]
            .set("42".into());
        let external = std::fs::read_to_string(&state.path).unwrap() + "\n# external change\n";
        std::fs::write(&state.path, &external).unwrap();
        state.save();
        assert!(state.error.is_some());
        assert_eq!(std::fs::read_to_string(&state.path).unwrap(), external);
        assert_eq!(
            state
                .agent_form
                .as_ref()
                .unwrap()
                .fields[0]
                .text,
            "42"
        );
        state.reload();
        assert_eq!(
            state
                .agent_form
                .as_ref()
                .unwrap()
                .fields[0]
                .text,
            "42"
        );
        state.save();
        assert!(state.error.is_none());
    }

    #[test]
    fn mcp_add_edit_remove_retains_exact_arguments_and_env_and_rejects_duplicates() {
        let (_directory, mut state) = state(true);
        state.edit_mcp(true);
        let form = state.mcp_form.as_mut().unwrap();
        form.fields[0].set("demo".into());
        form.fields[1].set("server".into());
        form.fields[2].set("--path\npath with spaces".into());
        form.fields[3].set("TOKEN=a=b\nMODE=readonly\nDEMO_TOOL_HOME= '/opt/sample-tool'".into());
        state.save();
        assert!(state.error.is_none(), "{:?}", state.error);
        let server = &state.mcp.as_ref().unwrap().servers()["demo"];
        assert_eq!(server.args, ["--path", "path with spaces"]);
        assert_eq!(server.env["TOKEN"], "a=b");
        assert_eq!(server.env["DEMO_TOOL_HOME"], "/opt/sample-tool");
        assert!(!server.enabled);
        state.edit_mcp(false);
        assert!(
            state.mcp_form.as_ref().unwrap().fields[3]
                .text
                .contains("DEMO_TOOL_HOME=/opt/sample-tool")
        );
        state.mcp_form.as_mut().unwrap().fields[3].set("TOKEN=first\nTOKEN=second".into());
        state.save();
        assert!(state.error.is_some());
        assert!(state.mcp_form.is_some());
        state.remove("demo".into());
        assert!(
            state
                .mcp
                .as_ref()
                .unwrap()
                .servers()
                .is_empty()
        );
    }

    #[test]
    fn stale_mcp_save_keeps_unsaved_editor_and_does_not_write_to_workspace() {
        let (_directory, mut state) = state(true);
        state.edit_mcp(true);
        let form = state.mcp_form.as_mut().unwrap();
        form.fields[0].set("draft".into());
        form.fields[1].set("server".into());
        let external = std::fs::read_to_string(&state.path).unwrap() + "\n# external\n";
        std::fs::write(&state.path, &external).unwrap();
        state.save();
        assert!(state.error.is_some());
        assert_eq!(std::fs::read_to_string(&state.path).unwrap(), external);
        assert_eq!(state.mcp_form.as_ref().unwrap().fields[0].text, "draft");
        state.reload();
        state.save();
        assert!(state.error.is_none());
        assert!(
            state
                .mcp
                .as_ref()
                .unwrap()
                .servers()
                .contains_key("draft")
        );
    }

    #[test]
    fn all_agent_controls_are_keyboard_reachable_and_render_at_terminal_sizes() {
        use ratatui::{Terminal, backend::TestBackend};
        let (_directory, mut state) = state(false);
        for (width, height) in [(120, 32), (80, 24), (40, 12)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            state.agent_form.as_mut().unwrap().focus = 0;
            for focus in 0..AGENT_FIELDS {
                assert_eq!(state.agent_form.as_ref().unwrap().focus, focus);
                terminal
                    .draw(|frame| state.draw(frame, frame.area()))
                    .unwrap();
                state.handle(&Event::Key(crossterm::event::KeyEvent::new(
                    KeyCode::Tab,
                    KeyModifiers::NONE,
                )));
            }
        }
        state.switch(Page::Mcp);
        state.edit_mcp(true);
        state.mcp_form.as_mut().unwrap().fields[0].set("retained".into());
        state.activate(Hit::ServerList);
        state.activate(Hit::Resume);
        assert_eq!(state.mcp_form.as_ref().unwrap().fields[0].text, "retained");
        assert!(state.editing_mcp);
    }

    #[test]
    fn mcp_server_switching_and_new_form_never_replace_unsaved_drafts() {
        let (_directory, mut state) = state(true);
        for name in ["first", "second"] {
            state.edit_mcp(true);
            let form = state.mcp_form.as_mut().unwrap();
            form.fields[0].set(name.into());
            form.fields[1].set("server".into());
            state.save();
            assert!(state.error.is_none());
        }
        state.selected_server = 0;
        state.edit_mcp(false);
        state.mcp_form.as_mut().unwrap().fields[1].set("first unsaved".into());
        state.selected_server = 1;
        state.edit_mcp(false);
        state.mcp_form.as_mut().unwrap().fields[1].set("second unsaved".into());
        state.edit_mcp(true);
        state.mcp_form.as_mut().unwrap().fields[0].set("new unsaved".into());
        state.selected_server = 0;
        state.edit_mcp(false);
        assert_eq!(
            state.mcp_form.as_ref().unwrap().fields[1].text,
            "first unsaved"
        );
        state.selected_server = 1;
        state.edit_mcp(false);
        assert_eq!(
            state.mcp_form.as_ref().unwrap().fields[1].text,
            "second unsaved"
        );
        state.edit_mcp(true);
        assert_eq!(
            state.mcp_form.as_ref().unwrap().fields[0].text,
            "new unsaved"
        );
    }
}
