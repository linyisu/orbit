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

pub(super) fn transcript_area(full: Rect) -> Rect {
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
    let area = if state.btw.visible() || state.subagent_dialog.is_some() {
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
    if state.btw.visible() {
        state.ime_cursor = None;
        let mut media = state.media.clone();
        media.workspace = state.workspace.clone().into();
        state.btw.draw(frame, area, inner, media);
        state.ime_cursor = state.btw.cursor();
        return;
    }
    if let Some(viewer) = &mut state.subagent_dialog {
        state.ime_cursor = None;
        let mut media = state.media.clone();
        media.workspace = state.workspace.clone().into();
        viewer.draw(frame, area, inner, media);
        state.ime_cursor = viewer.cursor();
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
                super::render::fit_label(
                    &format!("{title}（{} 个会话）", picker.matches().len()),
                    header.width as usize,
                    false,
                ),
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
        frame.render_widget(
            Paragraph::new("\n↑↓ 选择 · Enter 恢复 · Del 删除 · Esc 返回"),
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
mod tests;
