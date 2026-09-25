use super::{ModelOption, app::UiState, commands::COMMANDS};
use crate::marketplaces::{Marketplace, PluginPackage};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use orbit_store::types::{ProjectPlugin, Session};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph, Wrap},
};

#[derive(Debug)]
pub struct SessionPicker {
    sessions: Vec<Session>,
    query: String,
    selected: usize,
    pub error: Option<String>,
}

#[derive(Debug)]
pub struct ModelPicker {
    models: Vec<ModelOption>,
    query: String,
    selected: usize,
    current: String,
    effort: Option<orbit_model::types::ReasoningEffort>,
}

#[derive(Debug)]
pub struct MarketplaceDialog {
    marketplaces: Vec<Marketplace>,
    source: String,
    adding: bool,
    viewing_plugins: bool,
    inspection: Option<Result<PluginPackage, String>>,
    selected_market: usize,
    selected_plugin: usize,
    project_id: String,
    enabled: std::collections::BTreeSet<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ProjectPluginChange {
    Enable(ProjectPlugin),
    Disable {
        project_id: String,
        marketplace_source: String,
        plugin_name: String,
    },
}

impl MarketplaceDialog {
    pub fn new(
        marketplaces: Vec<Marketplace>,
        project_id: String,
        enabled: Vec<ProjectPlugin>,
    ) -> Self {
        Self {
            marketplaces,
            source: String::new(),
            adding: false,
            viewing_plugins: false,
            inspection: None,
            selected_market: 0,
            selected_plugin: 0,
            project_id,
            enabled: enabled
                .into_iter()
                .map(|plugin| (plugin.marketplace_source, plugin.plugin_name))
                .collect(),
        }
    }

    fn move_market(&mut self, down: bool) {
        self.selected_market = move_selection(self.selected_market, self.marketplaces.len(), down);
        self.selected_plugin = 0;
    }

    fn move_plugin(&mut self, down: bool) {
        let count = self
            .marketplaces
            .get(self.selected_market)
            .map(|market| market.plugins.len())
            .unwrap_or_default();
        self.selected_plugin = move_selection(self.selected_plugin, count, down);
    }

    fn selected_market(&self) -> Option<&Marketplace> {
        self.marketplaces.get(self.selected_market)
    }

    fn selected_plugin(&self) -> Option<(&Marketplace, &crate::marketplaces::Plugin)> {
        let market = self.selected_market()?;
        market
            .plugins
            .get(self.selected_plugin)
            .map(|plugin| (market, plugin))
    }

    fn is_enabled(&self, market: &Marketplace, plugin: &crate::marketplaces::Plugin) -> bool {
        self.enabled
            .contains(&(market.source.clone(), plugin.name.clone()))
    }

    fn selected_change(&self) -> Option<ProjectPluginChange> {
        let (market, plugin) = self.selected_plugin()?;
        if self.is_enabled(market, plugin) {
            return Some(ProjectPluginChange::Disable {
                project_id: self.project_id.clone(),
                marketplace_source: market.source.clone(),
                plugin_name: plugin.name.clone(),
            });
        }
        let package = self.inspection.as_ref()?.as_ref().ok()?;
        let package_source = plugin.source.clone()?;
        Some(ProjectPluginChange::Enable(ProjectPlugin {
            project_id: self.project_id.clone(),
            marketplace_source: market.source.clone(),
            plugin_name: plugin.name.clone(),
            package_source,
            version: package.version.clone(),
            enabled_at: 0,
        }))
    }

    fn inspect_selected_plugin(&mut self) {
        let selected = self.selected_market().and_then(|market| {
            market
                .plugins
                .get(self.selected_plugin)
                .map(|plugin| (market.clone(), plugin.clone()))
        });
        self.inspection = Some(match selected {
            Some((market, plugin)) => crate::marketplaces::inspect_plugin(&market, &plugin)
                .map_err(|error| error.to_string()),
            None => Err("没有可检查的插件。".into()),
        });
    }
}

fn move_selection(selected: usize, count: usize, down: bool) -> usize {
    if count == 0 {
        0
    } else if down {
        (selected + 1).min(count - 1)
    } else {
        selected.saturating_sub(1)
    }
}

impl ModelPicker {
    pub fn new(
        models: Vec<ModelOption>,
        current: String,
        effort: Option<orbit_model::types::ReasoningEffort>,
    ) -> Self {
        let selected = models
            .iter()
            .position(|model| model.qualified_name() == current)
            .unwrap_or_default();
        Self {
            models,
            query: String::new(),
            selected,
            current,
            effort,
        }
    }
    fn matches(&self) -> Vec<&ModelOption> {
        let query = self.query.to_lowercase();
        self.models
            .iter()
            .filter(|model| {
                model.qualified_name().to_lowercase().contains(&query)
                    || model.model.to_lowercase().contains(&query)
                    || model.provider.to_lowercase().contains(&query)
            })
            .collect()
    }
    fn move_by(&mut self, down: bool) {
        let count = self.matches().len();
        self.selected = if down {
            self.selected.saturating_add(1).min(count.saturating_sub(1))
        } else {
            self.selected.saturating_sub(1)
        };
        self.normalize_effort();
    }
    fn selected_efforts(&self) -> Vec<orbit_model::types::ReasoningEffort> {
        let matches = self.matches();
        let Some(model) = matches.get(self.selected) else {
            return Vec::new();
        };
        model.reasoning_efforts.clone()
    }
    fn cycle_effort(&mut self, forward: bool) {
        let efforts = self.selected_efforts();
        if efforts.is_empty() {
            self.effort = None;
            return;
        }
        let index = efforts
            .iter()
            .position(|effort| Some(*effort) == self.effort)
            .unwrap_or(0);
        let next = if forward {
            (index + 1) % efforts.len()
        } else {
            (index + efforts.len() - 1) % efforts.len()
        };
        self.effort = Some(efforts[next]);
    }

    fn normalize_effort(&mut self) {
        let efforts = self.selected_efforts();
        if efforts.is_empty() {
            self.effort = None;
        } else if !efforts.iter().any(|effort| Some(*effort) == self.effort) {
            let matches = self.matches();
            let default = matches
                .get(self.selected)
                .and_then(|m| m.config.reasoning_effort)
                .filter(|d| efforts.contains(d));
            self.effort = default.or_else(|| Some(efforts[0]));
        }
    }
}
impl SessionPicker {
    pub fn new(sessions: Vec<Session>) -> Self {
        Self {
            sessions,
            query: String::new(),
            selected: 0,
            error: None,
        }
    }
    fn matches(&self) -> Vec<&Session> {
        let query = self.query.to_lowercase();
        self.sessions
            .iter()
            .filter(|s| s.title.to_lowercase().contains(&query) || s.id.contains(&query))
            .collect()
    }
    fn move_by(&mut self, down: bool) {
        let count = self.matches().len();
        self.selected = if down {
            self.selected.saturating_add(1).min(count.saturating_sub(1))
        } else {
            self.selected.saturating_sub(1)
        };
    }
}

pub fn handle(state: &mut UiState, event: Event) -> Option<String> {
    if let Event::Key(key) = &event {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        if key.code == KeyCode::Esc
            && let Some(dialog) = &mut state.marketplace_dialog
        {
            if dialog.inspection.take().is_some() {
                return None;
            }
            if dialog.viewing_plugins {
                dialog.viewing_plugins = false;
                dialog.selected_plugin = 0;
                return None;
            }
        }
        if key.code == KeyCode::Esc
            || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            state.show_help = false;
            state.picker = None;
            state.model_picker = None;
            state.marketplace_dialog = None;
            return None;
        }
    }
    if let Some(picker) = &mut state.picker {
        match event {
            Event::Key(key) => match key.code {
                KeyCode::Up => picker.move_by(false),
                KeyCode::Down => picker.move_by(true),
                KeyCode::Enter => {
                    return picker.matches().get(picker.selected).map(|s| s.id.clone());
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    picker.query.push(c);
                    picker.selected = 0;
                    picker.error = None;
                }
                KeyCode::Backspace => {
                    picker.query.pop();
                    picker.selected = 0;
                    picker.error = None;
                }
                _ => {}
            },
            Event::Paste(text) => {
                picker
                    .query
                    .extend(text.chars().filter(|c| !c.is_control()));
                picker.selected = 0;
            }
            Event::Mouse(mouse) if mouse.kind == MouseEventKind::ScrollUp => picker.move_by(false),
            Event::Mouse(mouse) if mouse.kind == MouseEventKind::ScrollDown => picker.move_by(true),
            _ => {}
        }
    } else if let Some(picker) = &mut state.model_picker {
        match event {
            Event::Key(key) => match key.code {
                KeyCode::Up => picker.move_by(false),
                KeyCode::Down => picker.move_by(true),
                KeyCode::Left => picker.cycle_effort(false),
                KeyCode::Right => picker.cycle_effort(true),
                KeyCode::Enter => {
                    state.chosen_model = picker
                        .matches()
                        .get(picker.selected)
                        .map(|model| model.qualified_name());
                    state.chosen_effort = picker.effort;
                    state.model_picker = None;
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    picker.query.push(c);
                    picker.selected = 0;
                    picker.normalize_effort();
                }
                KeyCode::Backspace => {
                    picker.query.pop();
                    picker.selected = 0;
                    picker.normalize_effort();
                }
                _ => {}
            },
            Event::Paste(text) => {
                picker
                    .query
                    .extend(text.chars().filter(|c| !c.is_control()));
                picker.selected = 0;
                picker.normalize_effort();
            }
            Event::Mouse(mouse) if mouse.kind == MouseEventKind::ScrollUp => picker.move_by(false),
            Event::Mouse(mouse) if mouse.kind == MouseEventKind::ScrollDown => picker.move_by(true),
            _ => {}
        }
    } else if let Some(dialog) = &mut state.marketplace_dialog {
        match event {
            Event::Key(key) => match key.code {
                KeyCode::Up if dialog.inspection.is_none() && dialog.viewing_plugins => {
                    dialog.move_plugin(false)
                }
                KeyCode::Down if dialog.inspection.is_none() && dialog.viewing_plugins => {
                    dialog.move_plugin(true)
                }
                KeyCode::Up if !dialog.adding => dialog.move_market(false),
                KeyCode::Down if !dialog.adding => dialog.move_market(true),
                KeyCode::Char('a')
                    if !dialog.adding
                        && !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    dialog.adding = true
                }
                KeyCode::Char('e')
                    if dialog.inspection.as_ref().is_some_and(Result::is_ok)
                        && !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    if let Some(change) = dialog.selected_change() {
                        state.pending_project_plugin_change = Some(change);
                        state.marketplace_dialog = None;
                    }
                }
                KeyCode::Enter if dialog.viewing_plugins && dialog.inspection.is_none() => {
                    dialog.inspect_selected_plugin()
                }
                KeyCode::Enter if !dialog.adding && dialog.selected_market().is_some() => {
                    dialog.viewing_plugins = true
                }
                KeyCode::Enter if !dialog.adding => dialog.adding = true,
                KeyCode::Enter => {
                    let source = dialog.source.trim();
                    if !source.is_empty() {
                        state.pending_marketplace_source = Some(source.into());
                        state.marketplace_dialog = None;
                    }
                }
                KeyCode::Backspace if dialog.adding => {
                    dialog.source.pop();
                }
                KeyCode::Char(c)
                    if dialog.adding
                        && !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    dialog.source.push(c)
                }
                _ => {}
            },
            Event::Paste(text) if dialog.adding => dialog
                .source
                .extend(text.chars().filter(|c| !c.is_control())),
            Event::Mouse(mouse)
                if mouse.kind == MouseEventKind::ScrollUp
                    && dialog.inspection.is_none()
                    && dialog.viewing_plugins =>
            {
                dialog.move_plugin(false)
            }
            Event::Mouse(mouse)
                if mouse.kind == MouseEventKind::ScrollDown
                    && dialog.inspection.is_none()
                    && dialog.viewing_plugins =>
            {
                dialog.move_plugin(true)
            }
            Event::Mouse(mouse) if mouse.kind == MouseEventKind::ScrollUp && !dialog.adding => {
                dialog.move_market(false)
            }
            Event::Mouse(mouse) if mouse.kind == MouseEventKind::ScrollDown && !dialog.adding => {
                dialog.move_market(true)
            }
            _ => {}
        }
    } else if state.show_help {
        match event {
            Event::Key(key) => match key.code {
                KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                    state.help_keys = !state.help_keys;
                    state.help_scroll = 0;
                }
                KeyCode::Up => state.help_scroll = state.help_scroll.saturating_sub(1),
                KeyCode::Down => state.help_scroll += 1,
                _ => {}
            },
            Event::Mouse(mouse) if mouse.kind == MouseEventKind::ScrollUp => {
                state.help_scroll = state.help_scroll.saturating_sub(super::wheel::LINES)
            }
            Event::Mouse(mouse) if mouse.kind == MouseEventKind::ScrollDown => {
                state.help_scroll += super::wheel::LINES
            }
            _ => {}
        }
    }
    None
}

fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

pub fn draw(frame: &mut Frame<'_>, state: &mut UiState) {
    if !state.show_help
        && state.picker.is_none()
        && state.model_picker.is_none()
        && state.marketplace_dialog.is_none()
    {
        return;
    }
    state.selection.clear();
    let full = frame.area();
    let width = full.width.saturating_sub(4).min(96);
    let height = full.height.saturating_sub(2).min(24);
    let area = Rect::new(
        (full.width - width) / 2,
        (full.height - height) / 2,
        width,
        height,
    );
    super::overlay::clear(frame, area);
    let title = if state.picker.is_some() {
        "恢复会话 · 当前项目"
    } else if state.model_picker.is_some() {
        "选择模型"
    } else if state.marketplace_dialog.is_some() {
        "插件市场"
    } else {
        "Orbit 帮助"
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(218, 139, 109)));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let header = Rect::new(
        inner.x + 1,
        inner.y,
        inner.width.saturating_sub(2),
        2.min(inner.height),
    );
    let body = Rect::new(
        header.x,
        inner.y + 2,
        header.width,
        inner.height.saturating_sub(4),
    );
    let footer = Rect::new(header.x, inner.bottom().saturating_sub(2), header.width, 2);
    if let Some(picker) = &mut state.picker {
        let feedback = picker.error.as_deref().or(state.resume_loading.as_deref());
        let feedback_rows = feedback.map_or(0, |text| {
            unicode_width::UnicodeWidthStr::width(clean(text).as_str())
                .div_ceil(usize::from(header.width).max(1))
                .clamp(1, 3) as u16
        });
        let header = Rect {
            height: (2 + feedback_rows).min(inner.height.saturating_sub(2)),
            ..header
        };
        let body = Rect::new(
            header.x,
            header.bottom(),
            header.width,
            footer.y.saturating_sub(header.bottom()),
        );
        let search = super::render::fit_label(
            &format!("搜索标题或 ID：{}", clean(&picker.query)),
            header.width.saturating_sub(1) as usize,
            true,
        );
        state.ime_cursor = Some(
            (
                header.x + unicode_width::UnicodeWidthStr::width(search.as_str()) as u16,
                header.y + 1,
            )
                .into(),
        );
        let mut heading = vec![
            Line::styled(
                title,
                Style::default()
                    .fg(Color::Rgb(218, 139, 109))
                    .add_modifier(Modifier::BOLD),
            ),
            Line::from(format!("{search}▏")),
        ];
        if let Some(feedback) = feedback {
            heading.push(Line::styled(
                clean(feedback),
                Style::default().fg(if picker.error.is_some() {
                    Color::Rgb(235, 135, 135)
                } else {
                    Color::Rgb(218, 139, 109)
                }),
            ));
        }
        frame.render_widget(Paragraph::new(heading).wrap(Wrap { trim: false }), header);
        let rows = picker.matches();
        let visible = (body.height as usize / 2).max(1);
        let first = picker.selected.saturating_sub(visible - 1);
        let mut lines = Vec::new();
        for (index, session) in rows.iter().enumerate().skip(first).take(visible) {
            let selected = index == picker.selected;
            let style = if selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Rgb(218, 139, 109))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            lines.push(Line::styled(
                format!(
                    "{} {}",
                    if selected { "›" } else { " " },
                    clean(&session.title)
                ),
                style,
            ));
            lines.push(Line::styled(
                format!("  {} · {}", session.id, clean(&session.model)),
                Style::default().fg(Color::Rgb(170, 170, 170)),
            ));
        }
        if lines.is_empty() {
            lines.push(Line::from(if picker.sessions.is_empty() {
                "暂无其他可恢复会话。/new 后可在这里找回之前的对话。"
            } else {
                "没有匹配的会话。"
            }));
        }
        frame.render_widget(Paragraph::new(lines), body);
        let detail = format!("{} 个会话 · 按最近更新排序", rows.len());
        frame.render_widget(
            Paragraph::new(format!("{detail}\n↑↓ 选择 · Enter 恢复 · Esc 返回")),
            footer,
        );
    } else if let Some(picker) = &mut state.model_picker {
        let search = super::render::fit_label(
            &format!("筛选模型：{}", clean(&picker.query)),
            header.width.saturating_sub(1) as usize,
            true,
        );
        state.ime_cursor = Some(
            (
                header.x + unicode_width::UnicodeWidthStr::width(search.as_str()) as u16,
                header.y + 1,
            )
                .into(),
        );
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(
                    title,
                    Style::default()
                        .fg(Color::Rgb(218, 139, 109))
                        .add_modifier(Modifier::BOLD),
                ),
                Line::from(format!("{search}▏")),
            ]),
            header,
        );
        let rows = picker.matches();
        let visible = (body.height.saturating_sub(2) as usize).max(1);
        let first = picker.selected.saturating_sub(visible - 1);
        let mut lines = Vec::new();
        let mut previous_provider = None;
        for (index, model) in rows.iter().enumerate().skip(first).take(visible) {
            if previous_provider != Some(model.provider.as_str()) {
                lines.push(Line::styled(
                    clean(&model.provider),
                    Style::default()
                        .fg(Color::Rgb(218, 139, 109))
                        .add_modifier(Modifier::BOLD),
                ));
                previous_provider = Some(model.provider.as_str());
            }
            let selected = index == picker.selected;
            let current = model.qualified_name() == picker.current;
            let ordinal = picker
                .models
                .iter()
                .filter(|candidate| candidate.provider == model.provider)
                .position(|candidate| candidate.model == model.model)
                .map_or(1, |position| position + 1);
            let style = if selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Rgb(218, 139, 109))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            lines.push(Line::styled(
                format!(
                    "{} {}. {}{}{}{}",
                    if selected { "›" } else { " " },
                    ordinal,
                    model.model,
                    if model.supports_vision {
                        "  (视觉)"
                    } else {
                        ""
                    },
                    if model.default_for_provider {
                        "  (默认)"
                    } else {
                        ""
                    },
                    if current { "  (当前)" } else { "" },
                ),
                style,
            ));
        }
        if lines.is_empty() {
            lines.push(Line::from("没有匹配的模型。"));
        }
        frame.render_widget(Paragraph::new(lines), body);
        let effort = picker
            .effort
            .map(orbit_model::types::ReasoningEffort::label)
            .unwrap_or("此模型未配置思考强度");
        frame.render_widget(
            Paragraph::new(format!(
                "{} 个模型 · ←/→ 思考强度：{effort} · ↑↓ 选择 · Enter 确认 · Esc 返回",
                rows.len()
            )),
            footer,
        );
    } else if let Some(dialog) = &mut state.marketplace_dialog {
        if dialog.adding {
            let source = super::render::fit_label(
                &format!("来源：{}", clean(&dialog.source)),
                header.width.saturating_sub(1) as usize,
                true,
            );
            state.ime_cursor = Some(
                (
                    header.x + unicode_width::UnicodeWidthStr::width(source.as_str()) as u16,
                    header.y + 1,
                )
                    .into(),
            );
            frame.render_widget(
                Paragraph::new(vec![
                    Line::styled(
                        "添加市场源",
                        Style::default()
                            .fg(Color::Rgb(218, 139, 109))
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::from(format!("{source}▏")),
                ]),
                header,
            );
            frame.render_widget(
                Paragraph::new(
                    "可输入本地目录、GitHub owner/repo 或 HTTPS Git URL。\n下载只保存到 Orbit 用户数据目录；安装插件前仍会展示其内容。",
                )
                .wrap(Wrap { trim: false }),
                body,
            );
            frame.render_widget(Paragraph::new("Enter 下载 · Esc 取消"), footer);
        } else if let Some(inspection) = &dialog.inspection {
            state.ime_cursor = None;
            match inspection {
                Ok(package) => {
                    let version = package
                        .version
                        .as_deref()
                        .map(|value| format!("版本 {value}"))
                        .unwrap_or_else(|| "未声明版本".into());
                    let description = package.description.as_deref().unwrap_or("未声明描述");
                    frame.render_widget(
                        Paragraph::new(vec![
                            Line::styled(
                                format!(
                                    "{} · {}",
                                    clean(&package.name),
                                    if dialog.selected_plugin().is_some_and(|(market, plugin)| {
                                        dialog.is_enabled(market, plugin)
                                    }) {
                                        "当前项目已启用"
                                    } else {
                                        "包声明"
                                    }
                                ),
                                Style::default()
                                    .fg(Color::Rgb(218, 139, 109))
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Line::styled(
                                super::render::fit_label(
                                    &format!("{version} · {description}"),
                                    header.width as usize,
                                    true,
                                ),
                                Style::default().fg(Color::Rgb(170, 170, 170)),
                            ),
                        ]),
                        header,
                    );
                    let list = |items: &[String]| {
                        if items.is_empty() {
                            "无".into()
                        } else {
                            items.join("、")
                        }
                    };
                    let lines = vec![
                        Line::from(format!("技能：{}", list(&package.skills))),
                        Line::from(format!("命令：{}", list(&package.commands))),
                        Line::from(format!("Agent：{}", list(&package.agents))),
                        Line::from(format!(
                            "Hook：{}",
                            if package.declares_hooks {
                                "已声明，未执行"
                            } else {
                                "未声明"
                            }
                        )),
                        Line::from(format!(
                            "MCP：{}",
                            if package.declares_mcp {
                                "已声明，未启动"
                            } else {
                                "未声明"
                            }
                        )),
                        Line::styled(
                            super::render::fit_label(
                                &format!("位置：{}", package.directory.display()),
                                body.width as usize,
                                true,
                            ),
                            Style::default().fg(Color::Rgb(170, 170, 170)),
                        ),
                        Line::from(""),
                        Line::styled(
                            if dialog
                                .selected_plugin()
                                .is_some_and(|(market, plugin)| dialog.is_enabled(market, plugin))
                            {
                                "此包已为当前项目启用；Hook、MCP 和命令仍不会自动运行。"
                            } else {
                                "启用只记录当前项目的选择；Hook、MCP 和命令不会自动运行。"
                            },
                            Style::default().fg(Color::Rgb(170, 170, 170)),
                        ),
                    ];
                    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);
                }
                Err(error) => {
                    frame.render_widget(
                        Paragraph::new(Line::styled(
                            "无法检查插件包",
                            Style::default()
                                .fg(Color::Rgb(218, 139, 109))
                                .add_modifier(Modifier::BOLD),
                        )),
                        header,
                    );
                    frame.render_widget(
                        Paragraph::new(clean(error)).wrap(Wrap { trim: false }),
                        body,
                    );
                }
            }
            frame.render_widget(
                Paragraph::new(
                    if dialog
                        .selected_plugin()
                        .is_some_and(|(market, plugin)| dialog.is_enabled(market, plugin))
                    {
                        "E 停用 · Esc 返回插件列表"
                    } else if inspection.is_ok() {
                        "E 启用 · Esc 返回插件列表"
                    } else {
                        "Esc 返回插件列表"
                    },
                ),
                footer,
            );
        } else if dialog.viewing_plugins {
            state.ime_cursor = None;
            let Some(market) = dialog.selected_market() else {
                return;
            };
            frame.render_widget(
                Paragraph::new(vec![
                    Line::styled(
                        format!("{} · 插件", clean(&market.name)),
                        Style::default()
                            .fg(Color::Rgb(218, 139, 109))
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::styled(
                        clean(&market.source),
                        Style::default().fg(Color::Rgb(170, 170, 170)),
                    ),
                ]),
                header,
            );
            let visible = (body.height as usize / 2).max(1);
            let first = dialog.selected_plugin.saturating_sub(visible - 1);
            let mut entries = Vec::new();
            for (index, plugin) in market.plugins.iter().enumerate().skip(first).take(visible) {
                let selected = index == dialog.selected_plugin;
                let style = if selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Rgb(218, 139, 109))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().add_modifier(Modifier::BOLD)
                };
                entries.push(Line::styled(
                    format!(
                        "{} {}{}",
                        if selected { "›" } else { " " },
                        if dialog.is_enabled(market, plugin) {
                            "✓ "
                        } else {
                            ""
                        },
                        clean(&plugin.name)
                    ),
                    style,
                ));
                entries.push(Line::styled(
                    format!(
                        "  {}",
                        super::render::fit_label(
                            &clean(&plugin.description),
                            body.width.saturating_sub(2) as usize,
                            true,
                        )
                    ),
                    Style::default().fg(Color::Rgb(170, 170, 170)),
                ));
            }
            if entries.is_empty() {
                entries.push(Line::from("这个市场没有声明插件。"));
            }
            frame.render_widget(Paragraph::new(entries), body);
            frame.render_widget(
                Paragraph::new(format!(
                    "{} 个插件 · ↑↓ 浏览 · Enter 检查包 · Esc 返回市场列表",
                    market.plugins.len()
                )),
                footer,
            );
        } else {
            state.ime_cursor = None;
            frame.render_widget(
                Paragraph::new(Line::styled(
                    "已添加的市场",
                    Style::default()
                        .fg(Color::Rgb(218, 139, 109))
                        .add_modifier(Modifier::BOLD),
                )),
                header,
            );
            let visible = (body.height as usize / 2).max(1);
            let first = dialog.selected_market.saturating_sub(visible - 1);
            let mut entries = Vec::new();
            for (index, market) in dialog
                .marketplaces
                .iter()
                .enumerate()
                .skip(first)
                .take(visible)
            {
                let selected = index == dialog.selected_market;
                let style = if selected {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Rgb(218, 139, 109))
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().add_modifier(Modifier::BOLD)
                };
                entries.push(Line::styled(
                    format!(
                        "{} {}",
                        if selected { "›" } else { " " },
                        clean(&market.name)
                    ),
                    style,
                ));
                entries.push(Line::styled(
                    format!(
                        "  {} · {} 个插件",
                        super::render::fit_label(
                            &clean(&market.source),
                            body.width.saturating_sub(12) as usize,
                            true,
                        ),
                        market.plugins.len()
                    ),
                    Style::default().fg(Color::Rgb(170, 170, 170)),
                ));
            }
            if entries.is_empty() {
                entries.push(Line::from("尚未添加市场。"));
            }
            frame.render_widget(Paragraph::new(entries), body);
            frame.render_widget(
                Paragraph::new(format!(
                    "{} 个市场 · ↑↓ 选择 · Enter 查看插件 · A 添加市场 · Esc 返回",
                    dialog.marketplaces.len()
                )),
                footer,
            );
        }
    } else {
        state.ime_cursor = None;
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(
                    title,
                    Style::default()
                        .fg(Color::Rgb(218, 139, 109))
                        .add_modifier(Modifier::BOLD),
                ),
                Line::styled(
                    if state.help_keys {
                        "命令     [快捷键]"
                    } else {
                        "[命令]     快捷键"
                    },
                    Style::default()
                        .fg(Color::Rgb(218, 139, 109))
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            header,
        );
        let lines: Vec<Line> = if state.help_keys {
            [
                "Enter          发送消息；运行中提交引导或本地命令",
                "Ctrl+J / Shift+Enter  换行",
                "Ctrl+V / Alt+V  粘贴剪贴板图片或文字",
                "←→ + Backspace/Delete  选择或删除图片",
                "↑ / ↓          移动输入光标，首尾切换历史",
                "滚轮           滚动对话，不移动输入光标",
                "拖选 + Ctrl+C  复制选区",
                "Ctrl+O         展开工具结果；再次按下收起当前展开项",
                "Shift+Tab      切换 ask / auto / YOLO",
                "Esc            关闭面板或命令菜单",
                "Ctrl+A / E     移到行首 / 行尾",
                "Ctrl+U / K     删除到行首 / 行尾",
                "Ctrl+C         取消响应 / 清空输入 / 空输入时退出",
                "Ctrl+D         空输入时退出",
                "?              打开快捷键帮助",
            ]
            .into_iter()
            .map(Line::from)
            .collect()
        } else {
            COMMANDS
                .iter()
                .map(|c| {
                    Line::from(format!(
                        "{:<width$} {}",
                        c.name,
                        c.description,
                        width = super::commands::COMMAND_NAME_WIDTH
                    ))
                })
                .collect()
        };
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        state.help_scroll = state.help_scroll.min(
            paragraph
                .line_count(body.width)
                .saturating_sub(body.height as usize),
        );
        frame.render_widget(paragraph.scroll((state.help_scroll as u16, 0)), body);
        frame.render_widget(
            Paragraph::new("←→ / Tab 切换 · ↑↓ / 滚轮浏览\nEsc 返回对话"),
            footer,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;
    use ratatui::{Terminal, backend::TestBackend};
    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }
    fn session(id: &str, title: &str) -> Session {
        Session {
            id: id.into(),
            title: title.into(),
            workspace: "/project".into(),
            project_id: "project-id".into(),
            model: "model".into(),
            reasoning_effort: None,
            demo: true,
            updated_at: 0,
        }
    }

    #[test]
    fn model_picker_draws_on_a_terminal_too_short_for_body_rows() {
        let mut state = UiState::new("model");
        state.model_picker = Some(ModelPicker::new(Vec::new(), "model".into(), None));
        let mut terminal = Terminal::new(TestBackend::new(12, 6)).unwrap();

        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    }

    #[test]
    fn picker_filters_selects_and_cancels_without_touching_draft() {
        let mut state = UiState::new("model");
        state.input.insert("draft");
        state.picker = Some(SessionPicker::new(vec![
            session("abc", "older task"),
            session("def", "目标任务"),
        ]));
        handle(&mut state, Event::Paste("目标".into()));
        assert_eq!(handle(&mut state, key(KeyCode::Enter)), Some("def".into()));
        handle(&mut state, key(KeyCode::Esc));
        assert!(state.picker.is_none());
        assert_eq!(state.input.text, "draft");
    }

    #[test]
    fn model_picker_filters_and_returns_a_qualified_model_name() {
        let mut state = UiState::new("deepseek::deepseek-chat");
        state.model_picker = Some(ModelPicker::new(
            vec![
                ModelOption {
                    provider: "deepseek".into(),
                    model: "deepseek-chat".into(),
                    context_tokens: 64_000,
                    input_context_tokens: 32_000,
                    default_for_provider: true,
                    reasoning_efforts: vec![orbit_model::types::ReasoningEffort::Low],
                    supports_vision: false,
                    config: orbit_model::types::ModelConfig::default(),
                },
                ModelOption {
                    provider: "openai".into(),
                    model: "gpt-6-astra".into(),
                    context_tokens: 128_000,
                    input_context_tokens: 96_000,
                    default_for_provider: true,
                    reasoning_efforts: vec![
                        orbit_model::types::ReasoningEffort::Medium,
                        orbit_model::types::ReasoningEffort::High,
                    ],
                    supports_vision: true,
                    config: orbit_model::types::ModelConfig::default(),
                },
            ],
            state.model.clone(),
            Some(orbit_model::types::ReasoningEffort::Low),
        ));
        handle(&mut state, Event::Paste("astra".into()));
        handle(&mut state, key(KeyCode::Right));
        handle(&mut state, key(KeyCode::Enter));
        assert!(state.model_picker.is_none());
        assert_eq!(state.chosen_model.as_deref(), Some("openai::gpt-6-astra"));
        assert_eq!(
            state.chosen_effort,
            Some(orbit_model::types::ReasoningEffort::High)
        );
    }

    #[test]
    fn model_picker_can_select_no_thinking_effort() {
        let mut state = UiState::new("deepseek::deepseek-chat");
        state.model_picker = Some(ModelPicker::new(
            vec![
                ModelOption {
                    provider: "deepseek".into(),
                    model: "deepseek-chat".into(),
                    context_tokens: 64_000,
                    input_context_tokens: 32_000,
                    default_for_provider: true,
                    reasoning_efforts: vec![
                        orbit_model::types::ReasoningEffort::None,
                        orbit_model::types::ReasoningEffort::Low,
                    ],
                    supports_vision: false,
                    config: orbit_model::types::ModelConfig::default(),
                },
                ModelOption {
                    provider: "deepseek".into(),
                    model: "strict-model".into(),
                    context_tokens: 64_000,
                    input_context_tokens: 32_000,
                    default_for_provider: false,
                    reasoning_efforts: vec![orbit_model::types::ReasoningEffort::Low],
                    supports_vision: false,
                    config: orbit_model::types::ModelConfig::default(),
                },
            ],
            state.model.clone(),
            Some(orbit_model::types::ReasoningEffort::Low),
        ));
        // Cycling left from Low (which is at index 1 of [None, Low]) goes to None ("不思考")
        handle(&mut state, key(KeyCode::Left));
        handle(&mut state, key(KeyCode::Enter));
        assert!(state.model_picker.is_none());
        assert_eq!(
            state.chosen_effort,
            Some(orbit_model::types::ReasoningEffort::None)
        );

        // Model without None in efforts cannot select None
        state.model = "deepseek::strict-model".into();
        state.model_picker = Some(ModelPicker::new(
            vec![ModelOption {
                provider: "deepseek".into(),
                model: "strict-model".into(),
                context_tokens: 64_000,
                input_context_tokens: 32_000,
                default_for_provider: false,
                reasoning_efforts: vec![orbit_model::types::ReasoningEffort::Low],
                supports_vision: false,
                config: orbit_model::types::ModelConfig::default(),
            }],
            state.model.clone(),
            Some(orbit_model::types::ReasoningEffort::Low),
        ));
        handle(&mut state, key(KeyCode::Left));
        handle(&mut state, key(KeyCode::Enter));
        assert_eq!(
            state.chosen_effort,
            Some(orbit_model::types::ReasoningEffort::Low)
        );
    }

    #[test]
    fn marketplace_dialog_browses_plugins_then_returns_to_market_list() {
        let mut state = UiState::new("model");
        state.marketplace_dialog = Some(MarketplaceDialog::new(
            vec![Marketplace {
                name: "official".into(),
                source: "owner/catalog".into(),
                directory: "/catalog".into(),
                plugins: vec![
                    crate::marketplaces::Plugin {
                        name: "review".into(),
                        description: "Review pull requests".into(),
                        source: None,
                    },
                    crate::marketplaces::Plugin {
                        name: "docs".into(),
                        description: "Write documentation".into(),
                        source: None,
                    },
                ],
            }],
            "project".into(),
            Vec::new(),
        ));

        handle(&mut state, key(KeyCode::Enter));
        assert!(
            state
                .marketplace_dialog
                .as_ref()
                .is_some_and(|dialog| dialog.viewing_plugins)
        );
        handle(&mut state, key(KeyCode::Down));
        assert_eq!(
            state
                .marketplace_dialog
                .as_ref()
                .map(|dialog| dialog.selected_plugin),
            Some(1)
        );
        handle(&mut state, key(KeyCode::Esc));
        assert!(
            state
                .marketplace_dialog
                .as_ref()
                .is_some_and(|dialog| !dialog.viewing_plugins && dialog.selected_plugin == 0)
        );
    }

    #[test]
    fn marketplace_dialog_collects_source_before_starting_download() {
        let mut state = UiState::new("model");
        state.marketplace_dialog = Some(MarketplaceDialog::new(
            Vec::new(),
            "project".into(),
            Vec::new(),
        ));

        handle(&mut state, key(KeyCode::Enter));
        handle(&mut state, Event::Paste("owner/catalog".into()));
        handle(&mut state, key(KeyCode::Enter));

        assert!(state.marketplace_dialog.is_none());
        assert_eq!(
            state.pending_marketplace_source.as_deref(),
            Some("owner/catalog")
        );
    }

    fn plugin_marketplace() -> Marketplace {
        Marketplace {
            name: "official".into(),
            source: "owner/catalog".into(),
            directory: "/catalog".into(),
            plugins: vec![crate::marketplaces::Plugin {
                name: "review".into(),
                description: "Review pull requests".into(),
                source: Some("plugins/review".into()),
            }],
        }
    }

    fn inspected_plugin() -> PluginPackage {
        PluginPackage {
            name: "review".into(),
            version: Some("1.2.3".into()),
            description: Some("Review pull requests".into()),
            directory: "/catalog/plugins/review".into(),
            skills: Vec::new(),
            commands: Vec::new(),
            agents: Vec::new(),
            declares_hooks: false,
            declares_mcp: false,
        }
    }

    #[test]
    fn marketplace_dialog_enables_inspected_plugin_for_current_project() {
        let mut state = UiState::new("model");
        state.marketplace_dialog = Some(MarketplaceDialog::new(
            vec![plugin_marketplace()],
            "project".into(),
            Vec::new(),
        ));
        let dialog = state.marketplace_dialog.as_mut().unwrap();
        dialog.viewing_plugins = true;
        dialog.inspection = Some(Ok(inspected_plugin()));

        handle(&mut state, key(KeyCode::Char('e')));

        assert!(state.marketplace_dialog.is_none());
        assert_eq!(
            state.pending_project_plugin_change,
            Some(ProjectPluginChange::Enable(ProjectPlugin {
                project_id: "project".into(),
                marketplace_source: "owner/catalog".into(),
                plugin_name: "review".into(),
                package_source: "plugins/review".into(),
                version: Some("1.2.3".into()),
                enabled_at: 0,
            }))
        );
    }

    #[test]
    fn marketplace_dialog_disables_already_enabled_plugin() {
        let enabled = ProjectPlugin {
            project_id: "project".into(),
            marketplace_source: "owner/catalog".into(),
            plugin_name: "review".into(),
            package_source: "plugins/review".into(),
            version: Some("1.2.3".into()),
            enabled_at: 1,
        };
        let mut state = UiState::new("model");
        state.marketplace_dialog = Some(MarketplaceDialog::new(
            vec![plugin_marketplace()],
            "project".into(),
            vec![enabled],
        ));
        let dialog = state.marketplace_dialog.as_mut().unwrap();
        dialog.viewing_plugins = true;
        dialog.inspection = Some(Ok(inspected_plugin()));

        handle(&mut state, key(KeyCode::Char('e')));

        assert_eq!(
            state.pending_project_plugin_change,
            Some(ProjectPluginChange::Disable {
                project_id: "project".into(),
                marketplace_source: "owner/catalog".into(),
                plugin_name: "review".into(),
            })
        );
    }

    #[test]
    fn help_has_its_own_panel_and_all_shortcuts_are_reachable() {
        let mut state = UiState::new("model");
        state.show_help = true;
        for (width, height) in [(80, 24), (40, 14), (24, 10)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &mut state)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(text.contains("Orbit"));
        }
        handle(&mut state, key(KeyCode::Right));
        assert!(state.help_keys);
        for _ in 0..100 {
            handle(&mut state, key(KeyCode::Down));
        }
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("?"));
        assert!(!text.contains("/help"));
        handle(&mut state, key(KeyCode::Esc));
        assert!(!state.show_help);
    }

    #[test]
    fn locked_session_error_is_visible_above_the_list_in_error_color() {
        let mut state = UiState::new("model");
        let mut picker = SessionPicker::new(vec![session("abc", "目标会话")]);
        picker.error = Some("该会话已在其他 Orbit 窗口或进程中打开，请先关闭后再恢复。".into());
        state.picker = Some(picker);
        for width in [40, 80, 120] {
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
            terminal.draw(|frame| draw(frame, &mut state)).unwrap();
            let buffer = terminal.backend().buffer();
            let error_cells: Vec<_> = buffer
                .content()
                .iter()
                .enumerate()
                .filter(|(_, cell)| {
                    cell.fg == Color::Rgb(235, 135, 135) && !cell.symbol().trim().is_empty()
                })
                .collect();
            assert!(!error_cells.is_empty());
            assert!(error_cells.iter().all(|(i, _)| i / usize::from(width) < 8));
            let text = error_cells
                .iter()
                .map(|(_, cell)| cell.symbol())
                .collect::<String>();
            assert!(text.contains("已在其他Orbit"), "{text}");
            assert!(state.picker.is_some());
        }
    }

    #[test]
    fn panel_title_uses_the_inner_header_not_the_border() {
        let mut state = UiState::new("model");
        state.picker = Some(SessionPicker::new(vec![session("abc", "a session")]));
        let width = 80;
        let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let buffer = terminal.backend().buffer();
        let text_at = |row: u16| {
            (0..width)
                .map(|column| {
                    buffer.content()[(row as usize * width as usize) + column as usize].symbol()
                })
                .collect::<String>()
        };
        assert!(!text_at(1).contains('恢'));
        assert!(text_at(2).contains('恢'), "{}", text_at(2));
        assert!(text_at(1).contains('─'));
    }

    #[test]
    fn dialog_borders_survive_wide_background_text_and_restore_on_close() {
        for width in [80, 81, 120, 121] {
            for glyph in ["中", "👩‍💻", "a\u{301}"] {
                for indent in ["", " "] {
                    let mut state = UiState::new("model");
                    let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
                    let background =
                        Paragraph::new(vec![
                            Line::from(format!("{indent}{}", glyph.repeat(150)));
                            30
                        ]);
                    terminal
                        .draw(|frame| frame.render_widget(background.clone(), frame.area()))
                        .unwrap();
                    let original = terminal.backend().buffer().clone();
                    let area = Rect::new(
                        (width - width.saturating_sub(4).min(96)) / 2,
                        3,
                        width.saturating_sub(4).min(96),
                        24,
                    );
                    for query in ["", "中文"] {
                        let mut picker = SessionPicker::new(vec![session("abc", "中文会话")]);
                        picker.query = query.into();
                        state.picker = Some(picker);
                        terminal
                            .draw(|frame| {
                                frame.render_widget(background.clone(), frame.area());
                                draw(frame, &mut state);
                            })
                            .unwrap();
                        let buffer = terminal.backend().buffer();
                        for y in area.y + 1..area.bottom() - 1 {
                            for x in [area.x, area.right() - 1] {
                                assert_eq!(
                                    buffer[(x, y)].symbol(),
                                    "│",
                                    "width={width}, glyph={glyph}, indent={indent:?}, point=({x},{y})"
                                );
                            }
                        }
                    }
                    state.picker = None;
                    terminal
                        .draw(|frame| {
                            frame.render_widget(background.clone(), frame.area());
                            draw(frame, &mut state);
                        })
                        .unwrap();
                    use ratatui::buffer::CellWidth;
                    for y in 0..30 {
                        let mut x = 0;
                        while x < width {
                            assert_eq!(
                                terminal.backend().buffer()[(x, y)],
                                original[(x, y)],
                                "restored visible cell at ({x},{y})"
                            );
                            x += original[(x, y)].cell_width().max(1);
                        }
                    }
                }
            }
        }
    }
}
