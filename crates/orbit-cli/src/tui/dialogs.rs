use super::{ModelOption, app::UiState, commands::COMMANDS, icons};
use crate::marketplaces::{Marketplace, PluginPackage};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use orbit_store::types::{ProjectPlugin, Session};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph, Wrap},
};

#[derive(Debug)]
pub struct SessionPicker {
    offset: usize,
    sessions: Vec<Session>,
    query: String,
    selected: usize,
    pub error: Option<String>,
    title_scroll: Option<(String, usize, std::time::Instant)>,
    title_scroll_tick: u128,
    /// ID of the session currently awaiting delete confirmation. Set on the
    /// first `Delete` keypress; cleared on any other key or Esc.
    pending_delete: Option<String>,
}

#[derive(Debug)]
pub struct ModelPicker {
    offset: usize,
    models: Vec<ModelOption>,
    query: String,
    selected: usize,
    current: String,
    effort: Option<orbit_model::types::ReasoningEffort>,
}

#[derive(Debug)]
pub struct MarketplaceDialog {
    market_offset: usize,
    plugin_offset: usize,
    marketplaces: Vec<Marketplace>,
    source: String,
    adding: bool,
    viewing_plugins: bool,
    inspection: Option<Result<PluginPackage, String>>,
    selected_market: usize,
    selected_plugin: usize,
    project_id: String,
    enabled: std::collections::BTreeMap<(String, String), ProjectPlugin>,
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
            market_offset: 0,
            plugin_offset: 0,
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
                .map(|plugin| {
                    (
                        (
                            plugin.marketplace_source.clone(),
                            plugin.plugin_name.clone(),
                        ),
                        plugin,
                    )
                })
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
            .get(&(market.source.clone(), plugin.name.clone()))
            .is_some_and(|configured| {
                configured.marketplace_revision == market.revision
                    && plugin.source.as_deref() == Some(configured.package_source.as_str())
            })
    }

    fn selected_change(&self) -> Option<ProjectPluginChange> {
        let (market, plugin) = self.selected_plugin()?;
        let package = self.inspection.as_ref()?.as_ref().ok()?;

        if self.is_enabled(market, plugin)
            && self
                .enabled
                .get(&(market.source.clone(), plugin.name.clone()))
                .is_some_and(|configured| configured.version == package.version)
        {
            return Some(ProjectPluginChange::Disable {
                project_id: self.project_id.clone(),
                marketplace_source: market.source.clone(),
                plugin_name: plugin.name.clone(),
            });
        }

        let package_source = plugin.source.clone()?;

        Some(ProjectPluginChange::Enable(ProjectPlugin {
            project_id: self.project_id.clone(),
            marketplace_source: market.source.clone(),
            plugin_name: plugin.name.clone(),
            package_source,
            version: package.version.clone(),
            marketplace_revision: market.revision.clone(),
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
            offset: 0,
            models,
            query: String::new(),
            selected,
            current,
            effort,
        }
    }

    fn first_visible(&mut self, height: usize) -> usize {
        let rows = self.matches();
        let selected = self.selected.min(rows.len().saturating_sub(1));
        let mut first = self.offset.min(selected);
        while first < selected {
            let required = selected - first
                + 2
                + rows[first..=selected]
                    .windows(2)
                    .filter(|pair| pair[0].provider != pair[1].provider)
                    .count();
            if required <= height {
                break;
            }
            first += 1;
        }
        self.offset = first;
        first
    }

    fn matches(&self) -> Vec<&ModelOption> {
        super::search::ranked(self.models.iter(), |model| {
            [
                model.qualified_name().as_str(),
                &model.model,
                &model.provider,
            ]
            .into_iter()
            .filter_map(|text| super::search::rank(text, &self.query))
            .min()
        })
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
            offset: 0,
            sessions,
            query: String::new(),
            selected: 0,
            error: None,
            title_scroll: None,
            title_scroll_tick: 0,
            pending_delete: None,
        }
    }

    fn prepare_title_scroll(&mut self, width: usize) -> Option<std::time::Duration> {
        let title = self
            .matches()
            .get(self.selected)
            .map(|session| (session.id.clone(), clean(&session.title)));
        let Some((id, title)) = title.filter(|(_, title)| {
            width > 0 && unicode_width::UnicodeWidthStr::width(title.as_str()) > width
        }) else {
            self.title_scroll = None;
            return None;
        };
        let key = format!("{id}\n{title}");

        if !self
            .title_scroll
            .as_ref()
            .is_some_and(|(previous, previous_width, _)| {
                previous == &key && *previous_width == width
            })
        {
            self.title_scroll = Some((key, width, std::time::Instant::now()));
            self.title_scroll_tick = 0;
        }

        self.title_scroll
            .as_ref()
            .map(|(_, _, since)| since.elapsed())
    }

    pub fn advance_title_scroll(&mut self) -> bool {
        let Some((_, _, since)) = &self.title_scroll else {
            return false;
        };
        let tick = since.elapsed().as_millis().saturating_sub(100) / 120;
        let changed = tick != self.title_scroll_tick;
        self.title_scroll_tick = tick;
        changed
    }

    fn matches(&self) -> Vec<&Session> {
        super::search::ranked(self.sessions.iter(), |session| {
            super::search::rank(&session.title, &self.query)
        })
    }

    fn move_by(&mut self, down: bool) {
        let count = self.matches().len();
        self.selected = if down {
            self.selected.saturating_add(1).min(count.saturating_sub(1))
        } else {
            self.selected.saturating_sub(1)
        };
    }

    /// Remove the session with `id` from the list and clamp the selection index.
    pub fn remove_session(&mut self, id: &str) {
        self.sessions.retain(|s| s.id != id);
        self.selected = self.selected.min(self.sessions.len().saturating_sub(1));
        self.pending_delete = None;
        self.error = None;
    }
}

/// Format a Unix timestamp using the current local UTC offset; nonpositive values show `—`.
fn format_session_time(updated_at: i64) -> String {
    if updated_at <= 0 {
        return "—".into();
    }
    let local_secs = (updated_at + local_utc_offset_secs()).max(0) as u64;
    let secs_per_day = 86400u64;
    let days = local_secs / secs_per_day;
    let tod = local_secs % secs_per_day;
    let hh = tod / 3600;
    let mm = (tod % 3600) / 60;
    let (year, month, day) = days_to_ymd(days);
    format!("{year:04}-{month:02}-{day:02} {hh:02}:{mm:02}")
}

/// Return the standard local UTC offset on Linux, or zero on other targets.
fn local_utc_offset_secs() -> i64 {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `tzset` and the `timezone` global are thread-safe POSIX
        // globals; reading after `tzset` is defined behaviour.
        unsafe extern "C" {
            fn tzset();
            static timezone: std::ffi::c_long;
        }
        unsafe {
            tzset();
            // POSIX `timezone` is seconds *west* of UTC; negate to get east.
            // c_long is 32-bit on 32-bit Linux, but i64 on 64-bit Linux.
            #[allow(clippy::unnecessary_cast)]
            let offset = timezone as i64;
            -offset
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        0
    }
}

/// Convert a count of days since the Unix epoch (1970-01-01) to (year, month, day).
/// Uses Howard Hinnant's "civil_from_days" algorithm — exact for all Gregorian dates.
const fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    let z = days + 719468;
    let era = z / 146097;
    let doe = z % 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

fn fit_hit_rows(rows: &mut [(Rect, usize)], lines: &[Line<'_>], body: Rect) {
    for (area, _) in rows {
        let row = area.y.saturating_sub(body.y) as usize;
        if let Some(line) = lines.get(row) {
            area.width = area.width.min(line.width().min(u16::MAX as usize) as u16);
            area.height = area.height.min(1);
        }
    }
}

#[derive(Debug)]
pub struct InfoDialog {
    pub title: String,
    pub content: String,
    scroll: usize,
    max_scroll: usize,
    page_rows: usize,
}

impl InfoDialog {
    pub fn new(title: &str, content: String) -> Self {
        Self {
            title: title.into(),
            content,
            scroll: 0,
            max_scroll: 0,
            page_rows: 1,
        }
    }

    fn handle(&mut self, event: &Event) -> bool {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Esc | KeyCode::Enter => return true,
                KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return true,
                KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
                KeyCode::Down => self.scroll = self.scroll.saturating_add(1),
                KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(self.page_rows),
                KeyCode::PageDown => self.scroll = self.scroll.saturating_add(self.page_rows),
                KeyCode::Home => self.scroll = 0,
                KeyCode::End => self.scroll = self.max_scroll,
                _ => {}
            },
            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::ScrollUp => {
                    self.scroll = self.scroll.saturating_sub(super::wheel::LINES)
                }
                MouseEventKind::ScrollDown => {
                    self.scroll = self.scroll.saturating_add(super::wheel::LINES)
                }
                _ => {}
            },
            _ => {}
        }
        self.scroll = self.scroll.min(self.max_scroll);
        false
    }
}

pub fn handle(state: &mut UiState, event: Event) -> Option<String> {
    if let Some(dialog) = &mut state.info_dialog {
        if dialog.handle(&event) {
            state.info_dialog = None;
        }
        return None;
    }
    if matches!(&event, Event::Key(_) | Event::Paste(_)) {
        state.dialog_manual_scroll = false;
    }
    if let Event::Mouse(mouse) = &event
        && matches!(
            mouse.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        )
    {
        let visible = state
            .dialog_rows
            .iter()
            .map(|(_, index)| *index)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            .max(1);
        let down = mouse.kind == MouseEventKind::ScrollDown;
        let scroll = |offset: &mut usize, count: usize| {
            *offset = if down {
                offset.saturating_add(3).min(count.saturating_sub(visible))
            } else {
                offset.saturating_sub(3)
            };
        };
        if let Some(picker) = &mut state.picker {
            let count = picker.matches().len();
            scroll(&mut picker.offset, count);
        } else if let Some(picker) = &mut state.model_picker {
            let count = picker.matches().len();
            scroll(&mut picker.offset, count);
        } else if let Some(dialog) = &mut state.marketplace_dialog {
            if dialog.viewing_plugins {
                let count = dialog
                    .selected_market()
                    .map_or(0, |market| market.plugins.len());
                scroll(&mut dialog.plugin_offset, count);
            } else {
                scroll(&mut dialog.market_offset, dialog.marketplaces.len());
            }
        }
        state.dialog_manual_scroll = true;
        state.pending_list_hover = Some(*mouse);
        return None;
    }
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

    if let Event::Mouse(mouse) = &event
        && matches!(
            mouse.kind,
            MouseEventKind::Moved | MouseEventKind::Down(crossterm::event::MouseButton::Left)
        )
        && let Some((_, index)) = state
            .dialog_rows
            .iter()
            .find(|(area, _)| area.contains((mouse.column, mouse.row).into()))
    {
        let index = *index;
        if let Some(picker) = &mut state.picker {
            // Moving to a different row cancels any pending delete confirmation.
            if picker.selected != index {
                picker.pending_delete = None;
            }
            picker.selected = index;
            picker.error = None;
        } else if let Some(picker) = &mut state.model_picker {
            picker.selected = index;
            picker.normalize_effort();
        } else if let Some(dialog) = &mut state.marketplace_dialog {
            if dialog.viewing_plugins {
                dialog.selected_plugin = index;
            } else {
                dialog.selected_market = index;
                dialog.selected_plugin = 0;
            }
        }
        if mouse.kind == MouseEventKind::Moved {
            return None;
        }
        state.dialog_manual_scroll = false;
        return handle(
            state,
            Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )),
        );
    }

    if let Some(picker) = &mut state.picker {
        match event {
            Event::Key(key) => match key.code {
                KeyCode::Up => {
                    picker.pending_delete = None;
                    picker.move_by(false);
                }
                KeyCode::Down => {
                    picker.pending_delete = None;
                    picker.move_by(true);
                }
                KeyCode::Enter => {
                    // If a delete is pending, confirm it; otherwise resume.
                    if let Some(id) = picker.pending_delete.take() {
                        state.delete_request = Some(id);
                    } else {
                        return picker.matches().get(picker.selected).map(|s| s.id.clone());
                    }
                }
                KeyCode::Delete => {
                    let selected_id = picker.matches().get(picker.selected).map(|s| s.id.clone());
                    if let Some(id) = selected_id {
                        if picker.pending_delete.as_deref() == Some(&id) {
                            // Second Delete on the same session — confirm.
                            picker.pending_delete = None;
                            state.delete_request = Some(id);
                        } else {
                            // First Delete — enter confirmation state.
                            picker.pending_delete = Some(id);
                        }
                    }
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    picker.pending_delete = None;
                    picker.query.push(c);
                    picker.selected = 0;
                    picker.error = None;
                }
                KeyCode::Backspace => {
                    picker.pending_delete = None;
                    picker.query.pop();
                    picker.selected = 0;
                    picker.error = None;
                }
                _ => {
                    picker.pending_delete = None;
                }
            },
            Event::Paste(text) => {
                picker.pending_delete = None;
                picker
                    .query
                    .extend(text.chars().filter(|c| !c.is_control()));
                picker.selected = 0;
            }
            Event::Mouse(mouse) if mouse.kind == MouseEventKind::ScrollUp => {
                picker.pending_delete = None;
                picker.move_by(false);
            }
            Event::Mouse(mouse) if mouse.kind == MouseEventKind::ScrollDown => {
                picker.pending_delete = None;
                picker.move_by(true);
            }
            _ => {}
        }
    } else if let Some(picker) = &mut state.model_picker {
        match event {
            Event::Key(key) => match key.code {
                KeyCode::Up => picker.move_by(false),
                KeyCode::Down => picker.move_by(true),
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

pub(super) fn centered_area(full: Rect) -> Rect {
    centered_size(full, 96, 24)
}

fn transcript_area(full: Rect) -> Rect {
    let width = full.width.saturating_sub(full.width / 5).max(96);
    // Keep the half-screen proportions, then grow with the width for long records.
    let height = (width / 4)
        .min(full.height.saturating_sub(full.height / 5))
        .max(24);
    centered_size(full, width, height)
}

fn centered_size(full: Rect, width: u16, height: u16) -> Rect {
    let width = full.width.saturating_sub(4).min(width);
    let height = full.height.saturating_sub(2).min(height);
    Rect::new(
        full.x + (full.width - width) / 2,
        full.y + (full.height - height) / 2,
        width,
        height,
    )
}

pub fn draw(frame: &mut Frame<'_>, state: &mut UiState) {
    state.dialog_rows.clear();
    if !state.dialog_open() {
        return;
    }

    state.selection.clear();
    let area = if state.subagent_dialog.is_some() {
        transcript_area(frame.area())
    } else {
        centered_area(frame.area())
    };
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
        .border_style(Style::default().fg(crate::tui::theme::THEME.accent));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if let Some(viewer) = &mut state.subagent_dialog {
        state.ime_cursor = None;
        viewer.draw(frame, area, inner);
        return;
    }
    if let Some(dialog) = &mut state.info_dialog {
        state.ime_cursor = None;
        let title = Rect::new(
            inner.x + 1,
            inner.y,
            inner.width.saturating_sub(2),
            1.min(inner.height),
        );
        let body = Rect::new(
            title.x,
            title.bottom().saturating_add(1).min(inner.bottom()),
            title.width,
            inner.height.saturating_sub(2),
        );
        frame.render_widget(
            Paragraph::new(Line::styled(
                super::render::fit_label(&clean(&dialog.title), title.width as usize, false),
                Style::default()
                    .fg(crate::tui::theme::THEME.accent)
                    .add_modifier(Modifier::BOLD),
            )),
            title,
        );
        let lines = dialog
            .content
            .lines()
            .map(|line| Line::from(super::render::safe_output(line)))
            .collect::<Vec<_>>();
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        dialog.page_rows = usize::from(body.height).max(1);
        dialog.max_scroll = paragraph
            .line_count(body.width)
            .saturating_sub(body.height as usize)
            .min(u16::MAX as usize);
        dialog.scroll = dialog.scroll.min(dialog.max_scroll);
        frame.render_widget(paragraph.scroll((dialog.scroll as u16, 0)), body);
        return;
    }
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
            &format!("搜索标题：{}", clean(&picker.query)),
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
                    .fg(crate::tui::theme::THEME.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Line::from(search.clone()),
        ];

        if let Some(feedback) = feedback {
            heading.push(Line::styled(
                clean(feedback),
                Style::default().fg(if picker.error.is_some() {
                    crate::tui::theme::THEME.danger
                } else {
                    crate::tui::theme::THEME.accent
                }),
            ));
        }

        frame.render_widget(Paragraph::new(heading).wrap(Wrap { trim: false }), header);
        let title_width = body.width.saturating_sub(4) as usize;
        let title_elapsed = picker.prepare_title_scroll(title_width);
        let visible = (body.height as usize / 2).max(1);
        if !state.dialog_manual_scroll {
            picker.offset = picker
                .offset
                .min(picker.selected)
                .max(picker.selected.saturating_sub(visible - 1));
        }
        let first = picker.offset;
        let rows = picker.matches();
        let mut lines = Vec::new();
        let pending_delete_id = picker.pending_delete.clone();

        for (index, session) in rows.iter().enumerate().skip(first).take(visible) {
            state.dialog_rows.push((
                Rect::new(body.x, body.y + ((index - first) * 2) as u16, body.width, 2)
                    .intersection(body),
                index,
            ));
            let selected = index == picker.selected;
            let confirming = selected && pending_delete_id.as_deref() == Some(session.id.as_str());

            let (title_style, subtitle_style) = if confirming {
                (
                    Style::default()
                        .fg(crate::tui::theme::THEME.white)
                        .bg(crate::tui::theme::THEME.danger_background)
                        .add_modifier(Modifier::BOLD),
                    Style::default()
                        .fg(crate::tui::theme::THEME.danger_foreground)
                        .bg(crate::tui::theme::THEME.danger_background),
                )
            } else if selected {
                (
                    Style::default()
                        .fg(crate::tui::theme::THEME.on_accent)
                        .bg(crate::tui::theme::THEME.accent)
                        .add_modifier(Modifier::BOLD),
                    Style::default().fg(crate::tui::theme::THEME.secondary),
                )
            } else {
                (
                    Style::default(),
                    Style::default().fg(crate::tui::theme::THEME.secondary),
                )
            };

            let title_text = if confirming {
                let label = format!(
                    "{} 确认删除？再按 Del 或 Enter 确认，其他键取消",
                    icons::ALERT_OUTLINE
                );
                super::render::scrolling_title(&label, title_width, None)
            } else {
                super::render::scrolling_title(
                    &clean(&session.title),
                    title_width,
                    if selected { title_elapsed } else { None },
                )
            };

            lines.push(Line::styled(
                super::render::padded_label(&title_text, body.width as usize, 2),
                title_style,
            ));
            lines.push(Line::styled(
                super::render::padded_label(
                    &format_session_time(session.updated_at),
                    body.width as usize,
                    2,
                ),
                subtitle_style,
            ));
        }

        if lines.is_empty() {
            lines.push(Line::from(if picker.sessions.is_empty() {
                "暂无其他可恢复会话。/new 后可在这里找回之前的对话。"
            } else {
                "没有匹配的会话。"
            }));
        }

        fit_hit_rows(&mut state.dialog_rows, &lines, body);
        frame.render_widget(Paragraph::new(lines), body);
        let detail = format!("{} 个会话 · 按最近更新排序", rows.len());
        frame.render_widget(
            Paragraph::new(format!(
                "{detail}\n↑↓ 选择 · Enter 恢复 · Del 删除 · Esc 返回"
            )),
            footer,
        );
    } else if let Some(picker) = &mut state.model_picker {
        let body = Rect {
            height: body.height.saturating_sub(1),
            ..body
        };
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
                        .fg(crate::tui::theme::THEME.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Line::from(search.clone()),
            ]),
            header,
        );
        let visible = body.height as usize;
        let first = if state.dialog_manual_scroll {
            picker.offset
        } else {
            picker.first_visible(visible)
        };
        let rows = picker.matches();
        let mut lines = Vec::new();
        let mut previous_provider = None;

        for (index, model) in rows.iter().enumerate().skip(first) {
            let needed = 1 + usize::from(previous_provider != Some(model.provider.as_str()));
            if lines.len() + needed > visible {
                break;
            }
            if previous_provider != Some(model.provider.as_str()) {
                lines.push(Line::styled(
                    clean(&model.provider),
                    Style::default()
                        .fg(crate::tui::theme::THEME.accent)
                        .add_modifier(Modifier::BOLD),
                ));
                previous_provider = Some(model.provider.as_str());
            }

            state.dialog_rows.push((
                Rect::new(body.x, body.y + lines.len() as u16, body.width, 1).intersection(body),
                index,
            ));
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
                    .fg(crate::tui::theme::THEME.on_accent)
                    .bg(crate::tui::theme::THEME.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            let label = format!(
                "{}. {}{}{}{}",
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
            );
            lines.push(Line::styled(
                super::render::padded_label(&label, body.width as usize, 2),
                style,
            ));
        }

        if lines.is_empty() {
            lines.push(Line::from("没有匹配的模型。"));
        }

        fit_hit_rows(&mut state.dialog_rows, &lines, body);
        frame.render_widget(Paragraph::new(lines), body);
        frame.render_widget(
            Paragraph::new(format!("共 {} 个模型 · Esc 返回", rows.len())),
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
                            .fg(crate::tui::theme::THEME.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::from(source.clone()),
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
                                    .fg(crate::tui::theme::THEME.accent)
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Line::styled(
                                super::render::fit_label(
                                    &format!("{version} · {description}"),
                                    header.width as usize,
                                    true,
                                ),
                                Style::default().fg(crate::tui::theme::THEME.secondary),
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
                            Style::default().fg(crate::tui::theme::THEME.secondary),
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
                            Style::default().fg(crate::tui::theme::THEME.secondary),
                        ),
                    ];
                    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);
                }

                Err(error) => {
                    frame.render_widget(
                        Paragraph::new(Line::styled(
                            "无法检查插件包",
                            Style::default()
                                .fg(crate::tui::theme::THEME.accent)
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
            let visible = (body.height as usize / 2).max(1);
            if !state.dialog_manual_scroll {
                dialog.plugin_offset = dialog
                    .plugin_offset
                    .min(dialog.selected_plugin)
                    .max(dialog.selected_plugin.saturating_sub(visible - 1));
            }
            let Some(market) = dialog.selected_market() else {
                return;
            };

            frame.render_widget(
                Paragraph::new(vec![
                    Line::styled(
                        format!("{} · 插件", clean(&market.name)),
                        Style::default()
                            .fg(crate::tui::theme::THEME.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::styled(
                        clean(&market.source),
                        Style::default().fg(crate::tui::theme::THEME.secondary),
                    ),
                ]),
                header,
            );
            let visible = (body.height as usize / 2).max(1);
            let first = dialog.plugin_offset;
            let mut entries = Vec::new();

            for (index, plugin) in market.plugins.iter().enumerate().skip(first).take(visible) {
                state.dialog_rows.push((
                    Rect::new(body.x, body.y + ((index - first) * 2) as u16, body.width, 2)
                        .intersection(body),
                    index,
                ));
                let selected = index == dialog.selected_plugin;
                let style = if selected {
                    Style::default()
                        .fg(crate::tui::theme::THEME.on_accent)
                        .bg(crate::tui::theme::THEME.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().add_modifier(Modifier::BOLD)
                };

                let label = format!(
                    "{}{}",
                    if dialog.is_enabled(market, plugin) {
                        format!("{} ", icons::CHECK)
                    } else {
                        String::new()
                    },
                    clean(&plugin.name)
                );
                entries.push(Line::styled(
                    super::render::padded_label(&label, body.width as usize, 2),
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
                    Style::default().fg(crate::tui::theme::THEME.secondary),
                ));
            }

            if entries.is_empty() {
                entries.push(Line::from("这个市场没有声明插件。"));
            }

            fit_hit_rows(&mut state.dialog_rows, &entries, body);
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
                        .fg(crate::tui::theme::THEME.accent)
                        .add_modifier(Modifier::BOLD),
                )),
                header,
            );
            let visible = (body.height as usize / 2).max(1);
            if !state.dialog_manual_scroll {
                dialog.market_offset = dialog
                    .market_offset
                    .min(dialog.selected_market)
                    .max(dialog.selected_market.saturating_sub(visible - 1));
            }
            let first = dialog.market_offset;
            let mut entries = Vec::new();

            for (index, market) in dialog
                .marketplaces
                .iter()
                .enumerate()
                .skip(first)
                .take(visible)
            {
                state.dialog_rows.push((
                    Rect::new(body.x, body.y + ((index - first) * 2) as u16, body.width, 2)
                        .intersection(body),
                    index,
                ));
                let selected = index == dialog.selected_market;
                let style = if selected {
                    Style::default()
                        .fg(crate::tui::theme::THEME.on_accent)
                        .bg(crate::tui::theme::THEME.accent)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().add_modifier(Modifier::BOLD)
                };

                entries.push(Line::styled(
                    super::render::padded_label(&clean(&market.name), body.width as usize, 2),
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
                    Style::default().fg(crate::tui::theme::THEME.secondary),
                ));
            }

            if entries.is_empty() {
                entries.push(Line::from("尚未添加市场。"));
            }

            fit_hit_rows(&mut state.dialog_rows, &entries, body);
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
                        .fg(crate::tui::theme::THEME.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Line::styled(
                    if state.help_keys {
                        "命令     [快捷键]"
                    } else {
                        "[命令]     快捷键"
                    },
                    Style::default()
                        .fg(crate::tui::theme::THEME.accent)
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
                "↑ / ↓          移动光标；单行输入可切换历史",
                "Tab            打开提示 / 补全当前选项",
                "滚轮           滚动对话，不移动输入光标",
                "拖选 + Ctrl+C  复制选区",
                "Ctrl+O         展开工具结果；再次按下收起当前展开项",
                "Shift+Tab      切换 ask / auto / YOLO",
                "Esc            关闭面板或命令菜单",
                "Ctrl+A         全选输入；Shift+方向键扩展选区",
                "Ctrl+↑ / ↓     切换历史并保留草稿光标",
                "Home / End     移到行首 / 行尾",
                "Ctrl+← / →     按词移动；Ctrl+Home/End 到输入首尾",
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

    #[test]
    fn transcript_dialog_grows_on_wide_screens_and_keeps_half_screen_size() {
        for (full, expected) in [
            (Rect::new(0, 0, 105, 47), (96, 24)),
            (Rect::new(0, 0, 120, 50), (96, 24)),
            (Rect::new(0, 0, 150, 45), (120, 30)),
            (Rect::new(0, 0, 200, 50), (160, 40)),
            (Rect::new(0, 0, 200, 12), (160, 10)),
        ] {
            let area = transcript_area(full);
            assert_eq!((area.width, area.height), expected);
        }
        let widths = (100..=240)
            .map(|width| transcript_area(Rect::new(0, 0, width, 50)))
            .collect::<Vec<_>>();
        assert!(
            widths.windows(2).all(|pair| {
                pair[1].width >= pair[0].width && pair[1].width - pair[0].width <= 1
            })
        );
    }

    #[test]
    fn transcript_dialog_preserves_centering_and_bounds_at_all_sizes() {
        for (width, height) in [(0, 0), (1, 1), (8, 3), (100, 30), (201, 51)] {
            let full = Rect::new(7, 11, width, height);
            let area = transcript_area(full);
            assert_eq!(area.intersection(full), area);
            let left = area.left() - full.left();
            let right = full.right() - area.right();
            let top = area.top() - full.top();
            let bottom = full.bottom() - area.bottom();
            assert!(left.abs_diff(right) <= 1);
            assert!(top.abs_diff(bottom) <= 1);
        }
    }

    #[test]
    fn home_output_scrolls_in_a_temporary_dialog_without_changing_the_draft() {
        let mut state = UiState::new("orbit::model");
        state.input.set("Orbit 草稿".into());
        state.show_output(
            "Orbit 状态",
            format!("首行 Orbit\n{}\n末行 Orbit", "中文 👩‍💻 状态\n".repeat(80)),
        );
        let mut terminal = Terminal::new(TestBackend::new(70, 30)).unwrap();
        terminal
            .draw(|frame| super::super::render::draw(frame, &mut state))
            .unwrap();
        assert!(state.ime_cursor.is_none());
        assert!(state.info_dialog.as_ref().unwrap().max_scroll > 0);
        let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
        handle(&mut state, key(KeyCode::PageDown));
        let dialog = state.info_dialog.as_ref().unwrap();
        assert_eq!(dialog.scroll, dialog.page_rows);
        handle(&mut state, key(KeyCode::End));
        terminal
            .draw(|frame| super::super::render::draw(frame, &mut state))
            .unwrap();
        let screen = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        let text = screen.replace(' ', "");
        assert!(text.contains("末行Orbit"));
        assert!(!text.contains("首行Orbit"));
        handle(
            &mut state,
            Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
        );
        assert!(state.info_dialog.is_none());
        assert_eq!(state.input.text, "Orbit 草稿");
        assert!(state.transcript.is_empty());
        terminal
            .draw(|frame| super::super::render::draw(frame, &mut state))
            .unwrap();
        assert!(state.ime_cursor.is_some());
    }

    #[test]
    fn home_errors_leave_no_transcript_and_keep_dialogs_safe_at_small_sizes() {
        let mut state = UiState::new("orbit::model");
        for (width, height) in [(141, 41), (28, 18), (10, 3), (1, 1)] {
            state.report_local(super::super::app::TranscriptItem::CommandError(
                "Orbit 操作失败：\u{1b}[31m".into(),
            ));
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| super::super::render::draw(frame, &mut state))
                .unwrap();
            assert!(state.transcript.is_empty());
            assert!(state.ime_cursor.is_none());
            let buffer = terminal.backend().buffer();
            assert!(
                buffer
                    .content
                    .iter()
                    .all(|cell| !cell.symbol().contains('\u{1b}'))
            );
            if width > 20 {
                let border = buffer
                    .content
                    .iter()
                    .position(|cell| cell.symbol() == "┌")
                    .unwrap();
                let x = border as u16 % width;
                let panel_width = width.saturating_sub(4).min(96);
                assert!(width - x - panel_width - x <= 1);
            }
            handle(
                &mut state,
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            );
            assert!(state.info_dialog.is_none());
        }
    }

    #[test]
    fn delete_requires_two_presses_to_confirm_and_any_other_key_cancels() {
        let mut picker = SessionPicker::new(vec![
            session("keep", "保留的会话"),
            session("del", "要删除的会话"),
        ]);
        picker.selected = 1;

        // First Delete: enters pending state, no delete_request yet.
        assert!(picker.pending_delete.is_none());
        let del_key = Event::Key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));

        let mut state = UiState::new("model");
        state.picker = Some(picker);

        handle(&mut state, del_key.clone());
        assert_eq!(
            state.picker.as_ref().unwrap().pending_delete.as_deref(),
            Some("del")
        );
        assert!(state.delete_request.is_none());

        // Pressing Up cancels pending delete.
        handle(
            &mut state,
            Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
        );
        assert!(state.picker.as_ref().unwrap().pending_delete.is_none());

        // Navigate back and press Delete twice → delete_request is set.
        handle(
            &mut state,
            Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
        );
        handle(&mut state, del_key.clone()); // first press
        handle(&mut state, del_key.clone()); // second press confirms
        assert_eq!(state.delete_request.as_deref(), Some("del"));
        assert!(state.picker.as_ref().unwrap().pending_delete.is_none());
    }

    #[test]
    fn delete_confirmed_with_enter_after_first_del_press() {
        let mut state = UiState::new("model");
        state.picker = Some(SessionPicker::new(vec![session("abc", "会话")]));

        // First Delete → pending.
        handle(
            &mut state,
            Event::Key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE)),
        );
        assert!(state.picker.as_ref().unwrap().pending_delete.is_some());

        // Enter confirms.
        handle(
            &mut state,
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        );
        assert_eq!(state.delete_request.as_deref(), Some("abc"));
        assert!(state.picker.as_ref().unwrap().pending_delete.is_none());
    }

    #[test]
    fn calendar_year_changes_only_once_for_january_and_february() {
        assert_eq!(days_to_ymd(19722), (2023, 12, 31));
        assert_eq!(days_to_ymd(19723), (2024, 1, 1));
        assert_eq!(days_to_ymd(19782), (2024, 2, 29));
        assert_eq!(days_to_ymd(19783), (2024, 3, 1));
    }

    #[test]
    fn format_session_time_produces_iso_date_and_dash_for_zero() {
        // Zero / negative → em dash placeholder.
        assert_eq!(format_session_time(0), "—");
        assert_eq!(format_session_time(-1), "—");

        // 2024-06-15 12:00:00 UTC. On any system we get HH:MM in local time, so
        // only the date portion is safe to assert without knowing the TZ offset.
        let s = format_session_time(1_718_445_600);
        assert!(
            s.starts_with("2024-06-"),
            "expected 2024-06-xx date, got {s}"
        );
        // Must match YYYY-MM-DD HH:MM
        let re_like = s.len() == 16
            && s.as_bytes()[4] == b'-'
            && s.as_bytes()[7] == b'-'
            && s.as_bytes()[10] == b' '
            && s.as_bytes()[13] == b':';
        assert!(re_like, "unexpected format: {s}");
    }

    #[test]
    fn resume_search_ranks_titles_with_the_shared_matching_rules() {
        let mut picker = SessionPicker::new(vec![
            session("one", "a-long-b-long-c"),
            session("two", "项目 abc"),
            session("three", "ABC 开头"),
            session("aabbcc", "按编号查找"),
        ]);
        picker.query = "abc".into();

        let ids = picker
            .matches()
            .iter()
            .map(|session| session.id.as_str())
            .collect::<Vec<_>>();

        // Only title-matching sessions are returned; "aabbcc" (ID only) is excluded.
        assert_eq!(ids, ["three", "two", "one"]);
        picker.query = "项c".into();
        assert_eq!(picker.matches()[0].id, "two");
    }

    #[test]
    fn mouse_confirms_session_rows_and_wheel_preserves_selection() {
        use crossterm::event::{MouseButton, MouseEvent};
        let mut state = UiState::new("model");
        state.picker = Some(SessionPicker::new(vec![
            session("a", "first"),
            session("b", "second"),
        ]));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| super::super::render::draw(frame, &mut state))
            .unwrap();
        let area = state
            .dialog_rows
            .iter()
            .find(|(_, index)| *index == 1)
            .unwrap()
            .0;
        let mut mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        };

        assert_eq!(handle(&mut state, Event::Mouse(mouse)), Some("b".into()));
        assert_eq!(state.picker.as_ref().unwrap().selected, 1);
        mouse.kind = MouseEventKind::ScrollUp;
        handle(&mut state, Event::Mouse(mouse));
        assert_eq!(state.picker.as_ref().unwrap().selected, 1);
    }

    #[test]
    fn selected_session_title_scrolls_and_resets_for_selection_search_and_width() {
        use std::time::{Duration, Instant};

        let mut picker = SessionPicker::new(vec![
            session("first", "中文标题 abcdefghijklmnopqrstuvwxyz"),
            session("second", "另一段很长的会话标题 abcdefghijklmnopqrstuvwxyz"),
            session("short", "短标题"),
        ]);
        picker.prepare_title_scroll(12);
        picker.title_scroll.as_mut().unwrap().2 = Instant::now() - Duration::from_secs(1);

        assert!(picker.advance_title_scroll());
        let elapsed = picker.prepare_title_scroll(12);
        let title = &picker.sessions[0].title;
        assert_ne!(
            super::super::render::scrolling_title(title, 12, elapsed),
            super::super::render::scrolling_title(title, 12, Some(Duration::ZERO)),
        );

        picker.move_by(true);
        picker.prepare_title_scroll(12);
        assert_eq!(picker.title_scroll_tick, 0);
        picker.title_scroll.as_mut().unwrap().2 = Instant::now() - Duration::from_secs(1);
        picker.prepare_title_scroll(14);
        assert_eq!(picker.title_scroll_tick, 0);
        assert!(picker.title_scroll.as_ref().unwrap().2.elapsed() < Duration::from_secs(1));

        picker.query = "短标题".into();
        picker.selected = 0;
        assert!(picker.prepare_title_scroll(12).is_none());
        assert!(!picker.advance_title_scroll());
    }

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
            // 2024-06-15 12:00:00 UTC — a stable, non-zero timestamp so that
            // the rendered subtitle shows a date rather than "—".
            updated_at: 1_718_445_600,
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
    fn grouped_models_keep_last_row_visible_and_support_mouse_selection() {
        let models = (0..30)
            .map(|index| ModelOption {
                max_output_tokens: 32_000,
                provider: format!("provider-{}", index / 3),
                model: format!("model-{index}"),
                context_tokens: 128_000,
                input_context_tokens: 96_000,
                default_for_provider: false,
                reasoning_efforts: Vec::new(),
                supports_vision: false,
                config: orbit_model::types::ModelConfig::default(),
            })
            .collect();
        let mut state = UiState::new("provider-9::model-29");
        state.model_picker = Some(ModelPicker::new(models, state.model.clone(), None));
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();

        terminal.draw(|frame| draw(frame, &mut state)).unwrap();

        let (last, _) = state
            .dialog_rows
            .iter()
            .find(|(_, index)| *index == 29)
            .unwrap();
        assert!(last.height > 0 && last.width > 0);
        let (area, index) = state.dialog_rows[0];
        let mouse = |kind| {
            Event::Mouse(crossterm::event::MouseEvent {
                kind,
                column: area.x,
                row: area.y,
                modifiers: KeyModifiers::NONE,
            })
        };

        assert!(super::super::handle_list_hover(
            &mut state,
            &mouse(MouseEventKind::Moved)
        ));
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();

        assert_eq!(state.model_picker.as_ref().unwrap().selected, index);
        assert_eq!(state.dialog_rows[0], (area, index));
        assert_eq!(
            terminal.backend().buffer()[(area.x, area.y)].bg,
            crate::tui::theme::THEME.accent
        );
        assert!(state.chosen_model.is_none());

        handle(&mut state, mouse(MouseEventKind::ScrollUp));
        assert_eq!(state.model_picker.as_ref().unwrap().selected, index);
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        assert!(state.dialog_rows[0].1 < index);
        // Restore the original viewport for the click check below.
        state.model_picker.as_mut().unwrap().offset = index;
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let blank = Event::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Moved,
            column: area.right() + 1,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        });
        assert!(!super::super::handle_list_hover(&mut state, &blank));
        assert_eq!(state.model_picker.as_ref().unwrap().selected, index);

        handle(
            &mut state,
            mouse(MouseEventKind::Down(crossterm::event::MouseButton::Left)),
        );
        assert!(state.model_picker.is_none());
        assert_eq!(
            state.chosen_model,
            Some(format!("provider-{}::model-{index}", index / 3))
        );
    }

    #[test]
    fn scrolling_recomputes_hover_from_the_new_rows_without_pointer_motion() {
        let models = (0..30)
            .map(|index| ModelOption {
                max_output_tokens: 32_000,
                provider: "provider".into(),
                model: format!("model-{index}"),
                context_tokens: 128_000,
                input_context_tokens: 96_000,
                default_for_provider: false,
                reasoning_efforts: Vec::new(),
                supports_vision: false,
                config: orbit_model::types::ModelConfig::default(),
            })
            .collect();
        let mut state = UiState::new("provider::model-0");
        state.model_picker = Some(ModelPicker::new(models, state.model.clone(), None));
        let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
        terminal
            .draw(|frame| super::super::render::draw(frame, &mut state))
            .unwrap();
        let (area, before) = state.dialog_rows[2];

        handle(
            &mut state,
            Event::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: area.x + 2,
                row: area.y,
                modifiers: KeyModifiers::NONE,
            }),
        );
        terminal
            .draw(|frame| super::super::render::draw(frame, &mut state))
            .unwrap();

        let after = state
            .dialog_rows
            .iter()
            .find(|(rect, _)| rect.contains((area.x + 2, area.y).into()))
            .unwrap()
            .1;
        assert!(after > before);
        assert_eq!(state.model_picker.as_ref().unwrap().selected, after);
        assert!(state.pending_list_hover.is_none());
        assert!(state.chosen_model.is_none());
        assert_eq!(
            terminal.backend().buffer()[(area.x + 2, area.y)].bg,
            crate::tui::theme::THEME.accent
        );
        let buffer = terminal.backend().buffer();
        let footer_y = (0..20)
            .find(|y| {
                (0..100)
                    .map(|x| buffer[(x, *y)].symbol())
                    .collect::<String>()
                    .replace(' ', "")
                    .contains("共30个模型·Esc返回")
            })
            .unwrap();
        assert!((3..97).all(|x| buffer[(x, footer_y - 1)].symbol() == " "));
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
                    max_output_tokens: 32_000,
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
                    max_output_tokens: 32_000,
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
            Some(orbit_model::types::ReasoningEffort::Medium)
        );
    }

    #[test]
    fn model_picker_arrows_do_not_change_thinking_effort() {
        let mut state = UiState::new("deepseek::deepseek-chat");
        state.model_picker = Some(ModelPicker::new(
            vec![
                ModelOption {
                    max_output_tokens: 32_000,
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
                    max_output_tokens: 32_000,
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
        // Horizontal arrows no longer change reasoning in the model picker.
        handle(&mut state, key(KeyCode::Left));
        handle(&mut state, key(KeyCode::Enter));

        assert!(state.model_picker.is_none());
        assert_eq!(
            state.chosen_effort,
            Some(orbit_model::types::ReasoningEffort::Low)
        );

        // Model without None in efforts cannot select None
        state.model = "deepseek::strict-model".into();
        state.model_picker = Some(ModelPicker::new(
            vec![ModelOption {
                max_output_tokens: 32_000,
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
                revision: None,
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
            revision: None,
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
                marketplace_revision: None,
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
            marketplace_revision: None,
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
                    cell.fg == crate::tui::theme::THEME.danger && !cell.symbol().trim().is_empty()
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
