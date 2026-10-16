use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use orbit_protocol::{Session, plugins::Selection};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap},
};

use super::{ModelOption, app::UiState, commands::COMMANDS, icons};
use crate::marketplaces::{Marketplace, PluginPackage};

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

#[derive(Clone, Debug)]
pub struct ModelChoice {
    provider: String,
    model: String,
    supports_vision: bool,
    reasoning_effort: Option<orbit_client::protocol::model::ReasoningEffort>,
    reasoning_efforts: Vec<orbit_client::protocol::model::ReasoningEffort>,
}

impl ModelChoice {
    fn qualified_name(&self) -> String {
        format!("{}::{}", self.provider, self.model)
    }
}

impl From<ModelOption> for ModelChoice {
    fn from(model: ModelOption) -> Self {
        Self {
            provider: model.provider,
            model: model.model,
            supports_vision: model.supports_vision,
            reasoning_effort: model.config.reasoning_effort,
            reasoning_efforts: model.reasoning_efforts,
        }
    }
}

impl From<orbit_protocol::ModelSummary> for ModelChoice {
    fn from(model: orbit_protocol::ModelSummary) -> Self {
        Self {
            provider: model.provider,
            model: model.model,
            supports_vision: model.supports_vision,
            reasoning_effort: model
                .reasoning_effort
                .and_then(|value| value.parse().ok()),
            reasoning_efforts: model
                .reasoning_efforts
                .into_iter()
                .filter_map(|value| value.parse().ok())
                .collect(),
        }
    }
}

#[derive(Debug)]
pub struct ModelPicker {
    offset: usize,
    models: Vec<ModelChoice>,
    provider: Option<String>,
    back_area: Option<Rect>,
    back_hover: bool,
    query: String,
    selected: usize,
    current: String,
    effort: Option<orbit_client::protocol::model::ReasoningEffort>,
}

#[derive(Debug)]
pub struct MarketplaceDialog {
    request_identity: String,
    inspections: Option<Vec<orbit_protocol::plugins::Inspection>>,
    market_offset: usize,
    plugin_offset: usize,
    marketplaces: Vec<Marketplace>,
    source: String,
    adding: bool,
    viewing_plugins: bool,
    inspection: Option<Result<PluginPackage, String>>,
    selected_market: usize,
    selected_plugin: usize,
    session_id: String,
    enabled: std::collections::BTreeMap<(String, String), Selection>,
    query: String,
    searching: bool,
    enabled_only: bool,
    removing: Option<String>,
    pending: Option<String>,
    error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SessionPluginChange {
    Enable(Selection),
    Disable {
        session_id: String,
        marketplace_source: String,
        plugin_name: String,
    },
}

impl MarketplaceDialog {
    pub fn from_catalog(catalog: orbit_protocol::plugins::Catalog) -> Self {
        let mut dialog = Self::new(catalog.marketplaces, catalog.session_id, catalog.applied);
        dialog.inspections = Some(catalog.inspections);
        dialog
    }

    pub fn new(
        marketplaces: Vec<Marketplace>,
        session_id: String,
        enabled: Vec<Selection>,
    ) -> Self {
        Self {
            request_identity: crate::workspace::request_id(),
            inspections: None,
            market_offset: 0,
            plugin_offset: 0,
            marketplaces,
            source: String::new(),
            adding: false,
            viewing_plugins: false,
            inspection: None,
            selected_market: 0,
            selected_plugin: 0,
            session_id,
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
            query: String::new(),
            searching: false,
            enabled_only: false,
            removing: None,
            pending: None,
            error: None,
        }
    }

    /// A catalog is authoritative only for the session that opened this dialog.
    /// Preserve navigation by full source identity across refresh/reordering.
    pub fn update_catalog(&mut self, catalog: orbit_protocol::plugins::Catalog) -> bool {
        if catalog.session_id != self.session_id {
            return false;
        }
        let source = self
            .selected_market()
            .map(|market| market.source.clone());
        let plugin = self
            .selected_plugin()
            .map(|(_, plugin)| plugin.name.clone());
        let mut next = Self::from_catalog(catalog);
        next.request_identity = self.request_identity.clone();
        let selected = source.and_then(|source| {
            next.marketplaces
                .iter()
                .position(|market| market.source == source)
        });
        next.selected_market = selected.unwrap_or(
            self.selected_market.min(
                next.marketplaces
                    .len()
                    .saturating_sub(1),
            ),
        );
        next.viewing_plugins = self.viewing_plugins && selected.is_some();
        next.query = self.query.clone();
        next.enabled_only = self.enabled_only;
        if let Some(plugin) = plugin {
            next.selected_plugin = next
                .selected_market()
                .and_then(|market| {
                    market
                        .plugins
                        .iter()
                        .position(|candidate| candidate.name == plugin)
                })
                .unwrap_or(0);
        }
        next.normalize_plugin();
        *self = next;
        true
    }

    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }

    pub(super) fn identity(&self) -> &str {
        &self.request_identity
    }

    pub fn failed(&mut self, error: String) {
        self.pending = None;
        self.error = Some(error);
    }

    fn start(&mut self, operation: &str, source: &str) {
        self.pending = Some(format!("{operation}\n{source}"));
        self.error = None;
        self.removing = None;
    }

    fn plugin_indices(&self) -> Vec<usize> {
        let query = self.query.to_lowercase();
        self.selected_market()
            .map(|market| {
                market
                    .plugins
                    .iter()
                    .enumerate()
                    .filter(|(_, plugin)| {
                        (!self.enabled_only
                            || self
                                .enabled
                                .contains_key(&(market.source.clone(), plugin.name.clone())))
                            && (query.is_empty()
                                || plugin
                                    .name
                                    .to_lowercase()
                                    .contains(&query)
                                || plugin
                                    .description
                                    .to_lowercase()
                                    .contains(&query))
                    })
                    .map(|(index, _)| index)
                    .collect()
            })
            .unwrap_or_default()
    }

    fn normalize_plugin(&mut self) {
        let indices = self.plugin_indices();
        if !indices.contains(&self.selected_plugin) {
            self.selected_plugin = indices.first().copied().unwrap_or(0);
        }
        self.plugin_offset = 0;
    }

    fn move_market(&mut self, down: bool) {
        self.selected_market = move_selection(self.selected_market, self.marketplaces.len(), down);
        self.selected_plugin = 0;
        self.query.clear();
        self.normalize_plugin();
    }

    fn move_plugin(&mut self, down: bool) {
        let indices = self.plugin_indices();
        let selected = indices
            .iter()
            .position(|index| *index == self.selected_plugin)
            .unwrap_or(0);
        self.selected_plugin = indices
            .get(move_selection(selected, indices.len(), down))
            .copied()
            .unwrap_or(0);
    }

    fn selected_market(&self) -> Option<&Marketplace> {
        self.marketplaces
            .get(self.selected_market)
    }

    fn selected_plugin(&self) -> Option<(&Marketplace, &crate::marketplaces::Plugin)> {
        if !self
            .plugin_indices()
            .contains(&self.selected_plugin)
        {
            return None;
        }
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

    fn selected_change(&self) -> Option<SessionPluginChange> {
        let (market, plugin) = self.selected_plugin()?;
        let package = self
            .inspection
            .as_ref()?
            .as_ref()
            .ok()?;

        if self.is_enabled(market, plugin)
            && self
                .enabled
                .get(&(market.source.clone(), plugin.name.clone()))
                .is_some_and(|configured| configured.version == package.version)
        {
            return Some(SessionPluginChange::Disable {
                session_id: self.session_id.clone(),
                marketplace_source: market.source.clone(),
                plugin_name: plugin.name.clone(),
            });
        }

        let package_source = plugin.source.clone()?;

        Some(SessionPluginChange::Enable(Selection {
            marketplace_source: market.source.clone(),
            plugin_name: plugin.name.clone(),
            package_source,
            version: package.version.clone(),
            marketplace_revision: market.revision.clone(),
        }))
    }

    fn inspect_selected_plugin(&mut self) {
        let selected = self
            .selected_plugin()
            .map(|(market, plugin)| (market.clone(), plugin.clone()));

        if selected.is_none() {
            return;
        }

        self.inspection = Some(match selected {
            Some((market, plugin)) => match &self.inspections {
                Some(inspections) => inspections
                    .iter()
                    .find(|entry| entry.source == market.source && entry.name == plugin.name)
                    .map(|entry| entry.package.clone())
                    .unwrap_or_else(|| Err("插件检查结果缺失，请刷新市场".into())),
                None => Err("请刷新插件目录后重试".into()),
            },
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
        models: impl IntoIterator<Item = impl Into<ModelChoice>>,
        current: String,
        effort: Option<orbit_client::protocol::model::ReasoningEffort>,
    ) -> Self {
        let mut models: Vec<ModelChoice> = models
            .into_iter()
            .map(Into::into)
            .collect();
        models.sort_by(|a, b| {
            a.model
                .cmp(&b.model)
                .then(a.provider.cmp(&b.provider))
        });
        let current_provider = current
            .split_once("::")
            .map(|(provider, _)| provider);
        let providers: std::collections::BTreeSet<_> = models
            .iter()
            .map(|model| model.provider.as_str())
            .collect();
        let selected = providers
            .iter()
            .position(|provider| Some(*provider) == current_provider)
            .unwrap_or_default();

        Self {
            offset: 0,
            models,
            provider: None,
            back_area: None,
            back_hover: false,
            query: String::new(),
            selected,
            current,
            effort,
        }
    }

    fn providers(&self) -> Vec<(String, usize)> {
        let mut groups = std::collections::BTreeMap::<String, usize>::new();
        for model in self.matches() {
            *groups
                .entry(model.provider.clone())
                .or_default() += 1;
        }
        groups.into_iter().collect()
    }

    fn row_count(&self) -> usize {
        if self.provider.is_some() {
            self.matches().len()
        } else {
            self.providers().len()
        }
    }

    fn enter_provider(&mut self) {
        if self.provider.is_some() {
            return;
        }
        let Some((provider, _)) = self
            .providers()
            .get(self.selected)
            .cloned()
        else {
            return;
        };
        self.provider = Some(provider);
        self.selected = self
            .matches()
            .iter()
            .position(|model| model.qualified_name() == self.current)
            .unwrap_or_default();
        self.offset = 0;
        self.normalize_effort();
    }

    fn back(&mut self) -> bool {
        let Some(provider) = self.provider.take() else {
            return false;
        };
        self.selected = self
            .providers()
            .iter()
            .position(|(name, _)| *name == provider)
            .unwrap_or_default();
        self.offset = 0;
        true
    }

    fn first_visible(&mut self, height: usize) -> usize {
        let selected = self
            .selected
            .min(self.row_count().saturating_sub(1));
        let mut first = self.offset.min(selected);
        while first < selected {
            let required = selected - first + 1;
            if required <= height {
                break;
            }
            first += 1;
        }

        self.offset = first;
        first
    }

    fn matches(&self) -> Vec<&ModelChoice> {
        super::search::ranked(
            self.models.iter().filter(|model| {
                self.provider
                    .as_ref()
                    .is_none_or(|provider| model.provider == *provider)
            }),
            |model| {
                [
                    model.qualified_name().as_str(),
                    &model.model,
                    &model.provider,
                ]
                .into_iter()
                .filter_map(|text| super::search::rank(text, &self.query))
                .min()
            },
        )
    }

    fn move_by(&mut self, down: bool) {
        let count = self.row_count();
        self.selected = if down {
            self.selected
                .saturating_add(1)
                .min(count.saturating_sub(1))
        } else {
            self.selected.saturating_sub(1)
        };
        self.normalize_effort();
    }

    fn selected_efforts(&self) -> Vec<orbit_client::protocol::model::ReasoningEffort> {
        let matches = self.matches();
        let Some(model) = matches.get(self.selected) else {
            return Vec::new();
        };

        model.reasoning_efforts.clone()
    }

    fn normalize_effort(&mut self) {
        if self.provider.is_none() {
            return;
        }
        let efforts = self.selected_efforts();

        if efforts.is_empty() {
            self.effort = None;
        } else if !efforts
            .iter()
            .any(|effort| Some(*effort) == self.effort)
        {
            let matches = self.matches();
            let default = matches
                .get(self.selected)
                .and_then(|m| m.reasoning_effort)
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
        let tick = since
            .elapsed()
            .as_millis()
            .saturating_sub(100)
            / 120;
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
            self.selected
                .saturating_add(1)
                .min(count.saturating_sub(1))
        } else {
            self.selected.saturating_sub(1)
        };
    }
}

/// Format a Unix timestamp using the current local UTC offset; nonpositive values show `—`.
fn format_session_time(updated_at: i64) -> String {
    if updated_at <= 0 {
        return "—".into();
    }
    let local_secs = (updated_at + local_utc_offset_secs()).max(0) as u64;
    let secs_per_day = 86_400u64;
    let days = local_secs / secs_per_day;
    let tod = local_secs % secs_per_day;
    let hh = tod / 3_600;
    let mm = (tod % 3_600) / 60;
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
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
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
            area.width = area
                .width
                .min(line.width().min(u16::MAX as usize) as u16);
            area.height = area.height.min(1);
        }
    }
}

#[derive(Debug)]
pub(super) enum ReportSource {
    Usage,
    Tool(String),
    Process,
}

#[derive(Debug)]
pub(super) struct ReportPage {
    pub label: String,
    pub content: Text<'static>,
}

#[derive(Debug)]
pub struct InfoDialog {
    pub title: String,
    pub content: String,
    scroll: usize,
    max_scroll: usize,
    page_rows: usize,
    pub(super) source: Option<ReportSource>,
    pub(super) refresh_requested: bool,
    pages: Vec<ReportPage>,
    selected_page: usize,
    page_areas: Vec<(Rect, usize)>,
    target: Option<orbit_client::execution::details::Target>,
}

impl InfoDialog {
    pub(super) fn tool(record: &orbit_client::execution::ToolRun) -> Self {
        let mut dialog = Self::new("工具详情", String::new());
        dialog.source = Some(ReportSource::Tool(record.id.clone()));
        dialog.update_tool(record);
        let initial = orbit_client::execution::details::default_stream(record);
        if let Some(index) = orbit_client::execution::details::streams(record)
            .iter()
            .position(|stream| *stream == initial)
        {
            dialog.select_page(index);
        }
        dialog
    }

    pub(super) fn update_tool(&mut self, record: &orbit_client::execution::ToolRun) {
        use orbit_client::execution::details;
        self.title = format!("{} · {}", record.name, details::status(record));
        self.target = details::target(record);
        self.pages(
            details::streams(record)
                .iter()
                .map(|stream| ReportPage {
                    label: stream.label().into(),
                    content: Text::from(format!(
                        "{}\n\n{}\n\n{}",
                        record.summary(),
                        details::content(record, *stream),
                        details::note(record, *stream)
                    )),
                })
                .collect(),
        );
    }

    /// Embedded transcripts expose evidence without borrowing the outer session's actions.
    pub(super) fn disable_navigation(&mut self) {
        self.target = None;
    }

    pub fn new(title: &str, content: String) -> Self {
        Self {
            title: title.into(),
            content,
            scroll: 0,
            max_scroll: 0,
            page_rows: 1,
            source: None,
            refresh_requested: false,
            pages: Vec::new(),
            selected_page: 0,
            page_areas: Vec::new(),
            target: None,
        }
    }

    pub(super) fn pages(&mut self, pages: Vec<ReportPage>) {
        self.selected_page = self
            .selected_page
            .min(pages.len().saturating_sub(1));
        self.content = pages
            .get(self.selected_page)
            .map(|page| page.content.to_string())
            .unwrap_or_default();
        self.pages = pages;
    }

    fn select_page(&mut self, index: usize) {
        if let Some(page) = self.pages.get(index) {
            self.selected_page = index;
            self.content = page.content.to_string();
            self.scroll = 0;
            self.max_scroll = 0;
        }
    }

    fn handle(&mut self, event: &Event) -> bool {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => match key.code {
                KeyCode::Esc | KeyCode::Enter => return true,
                KeyCode::Char('c')
                    if key
                        .modifiers
                        .contains(KeyModifiers::CONTROL) =>
                {
                    return true;
                }
                KeyCode::Up => self.scroll = self.scroll.saturating_sub(1),
                KeyCode::Down => self.scroll = self.scroll.saturating_add(1),
                KeyCode::PageUp => {
                    self.scroll = self
                        .scroll
                        .saturating_sub(self.page_rows)
                }
                KeyCode::PageDown => {
                    self.scroll = self
                        .scroll
                        .saturating_add(self.page_rows)
                }
                KeyCode::Home => self.scroll = 0,
                KeyCode::End => self.scroll = self.max_scroll,
                KeyCode::Tab | KeyCode::Right if !self.pages.is_empty() => {
                    self.select_page((self.selected_page + 1) % self.pages.len())
                }
                KeyCode::BackTab | KeyCode::Left if !self.pages.is_empty() => {
                    self.select_page((self.selected_page + self.pages.len() - 1) % self.pages.len())
                }
                KeyCode::Char('r' | 'R') | KeyCode::F(5) if self.source.is_some() => {
                    self.refresh_requested = true
                }
                _ => {}
            },

            Event::Mouse(mouse) => match mouse.kind {
                MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                    if let Some((_, index)) = self
                        .page_areas
                        .iter()
                        .find(|(area, _)| area.contains((mouse.column, mouse.row).into()))
                    {
                        self.select_page(*index);
                    }
                }
                MouseEventKind::ScrollUp => {
                    self.scroll = self
                        .scroll
                        .saturating_sub(super::wheel::LINES)
                }
                MouseEventKind::ScrollDown => {
                    self.scroll = self
                        .scroll
                        .saturating_add(super::wheel::LINES)
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
    let current_tool = state
        .info_dialog
        .as_ref()
        .and_then(|dialog| match &dialog.source {
            Some(ReportSource::Tool(id)) => Some(id.clone()),
            _ => None,
        });
    if let Some(id) = current_tool {
        if let Event::Key(key) = &event
            && key.kind != KeyEventKind::Release
            && key.code == KeyCode::Char('a')
            && let Some(target) = state
                .info_dialog
                .as_ref()
                .and_then(|dialog| dialog.target.clone())
        {
            use orbit_client::execution::details::Target;
            match target {
                Target::Agent(agent) => {
                    state.info_dialog = None;
                    super::subagents::open(state, agent);
                }
                Target::Image(path) => {
                    let mut context = state.media.clone();
                    context.workspace = state.workspace.clone().into();
                    state.info_dialog = None;
                    state.media_preview = Some(super::media::Preview::new(
                        context,
                        super::media::Source::Image(path),
                    ));
                }
                Target::Question => {
                    if let Some(row) = state
                        .transcript
                        .iter()
                        .find_map(|item| match item {
                            super::app::TranscriptItem::Tool(row) if row.id == id => Some(row),
                            _ => None,
                        })
                        && state
                            .user_input
                            .open_tool(&row.id, &row.output)
                    {
                        state.info_dialog = None;
                    }
                }
            }
            return None;
        }
        if let Event::Key(key) = &event
            && key.kind != KeyEventKind::Release
            && key
                .modifiers
                .contains(KeyModifiers::ALT)
            && matches!(key.code, KeyCode::Left | KeyCode::Right)
        {
            let records = state
                .transcript
                .iter()
                .filter_map(|item| match item {
                    super::app::TranscriptItem::Tool(row) => Some(row),
                    _ => None,
                })
                .collect::<Vec<_>>();
            if let Some(index) = records
                .iter()
                .position(|row| row.id == id)
            {
                let next = if key.code == KeyCode::Left {
                    index.saturating_sub(1)
                } else {
                    (index + 1).min(records.len() - 1)
                };
                state.info_dialog = Some(InfoDialog::tool(&records[next].record()));
            }
            return None;
        }
    }
    if let Some(dialog) = &mut state.info_dialog {
        if let Event::Key(key) = &event
            && key.kind != KeyEventKind::Release
            && key.code == KeyCode::Char('y')
            && matches!(
                dialog.source,
                Some(ReportSource::Tool(_) | ReportSource::Process)
            )
        {
            if let Err(error) = super::selection::copy_text(&dialog.content) {
                state.feedback.error(error.to_string());
            }
            return None;
        }
        if dialog.handle(&event) {
            state.info_dialog = None;
        }
        return None;
    }
    if state
        .marketplace_dialog
        .as_ref()
        .is_some_and(MarketplaceDialog::pending)
    {
        if matches!(
            &event,
            Event::Key(key)
                if key.kind != KeyEventKind::Release
                    && (key.code == KeyCode::Esc
                        || key.code == KeyCode::Char('c')
                            && key
                                .modifiers
                                .contains(KeyModifiers::CONTROL))
        ) {
            state.marketplace_dialog = None;
        }
        return None;
    }
    if let Some(picker) = &mut state.model_picker {
        match &event {
            Event::Mouse(mouse) => {
                picker.back_hover = picker
                    .back_area
                    .is_some_and(|area| area.contains((mouse.column, mouse.row).into()));
                if picker.back_hover {
                    if mouse.kind == MouseEventKind::Down(crossterm::event::MouseButton::Left) {
                        picker.back();
                        state.dialog_manual_scroll = false;
                    }
                    return None;
                }
                let hit = state
                    .model_update_area
                    .is_some_and(|area| area.contains((mouse.column, mouse.row).into()));
                state.model_update_hover = hit;
                if hit {
                    if mouse.kind == MouseEventKind::Down(crossterm::event::MouseButton::Left) {
                        state.model_update_requested = true;
                    }
                    return None;
                }
            }
            Event::Key(key) if key.code == KeyCode::F(5) && key.kind != KeyEventKind::Release => {
                state.model_update_requested = true;
                return None;
            }
            _ => {}
        }
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
                offset
                    .saturating_add(3)
                    .min(count.saturating_sub(visible))
            } else {
                offset.saturating_sub(3)
            };
        };

        if let Some(picker) = &mut state.picker {
            let count = picker.matches().len();
            scroll(&mut picker.offset, count);
        } else if let Some(picker) = &mut state.model_picker {
            let count = picker.row_count();
            scroll(&mut picker.offset, count);
        } else if let Some(dialog) = &mut state.marketplace_dialog {
            if dialog.viewing_plugins {
                let count = dialog.plugin_indices().len();
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
            if dialog.removing.take().is_some() {
                return None;
            }
            if dialog.searching {
                dialog.searching = false;
                return None;
            }
            if dialog.adding {
                dialog.adding = false;
                dialog.error = None;
                return None;
            }
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
            && let Some(picker) = &mut state.model_picker
            && picker.back()
        {
            return None;
        }

        if key.code == KeyCode::Esc
            || (key.code == KeyCode::Char('c')
                && key
                    .modifiers
                    .contains(KeyModifiers::CONTROL))
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
            if picker.selected != index {
                picker.pending_delete = None;
            }
            picker.selected = index;
            picker.error = None;
        } else if let Some(picker) = &mut state.model_picker {
            picker.selected = index;
            picker.normalize_effort();
        } else if let Some(dialog) = &mut state.marketplace_dialog {
            if dialog.adding || dialog.inspection.is_some() || dialog.removing.is_some() {
                return None;
            }
            if dialog.viewing_plugins {
                if dialog.plugin_indices().contains(&index) {
                    dialog.selected_plugin = index;
                }
            } else {
                if index >= dialog.marketplaces.len() {
                    return None;
                }
                dialog.selected_market = index;
                dialog.selected_plugin = 0;
                dialog.query.clear();
                dialog.normalize_plugin();
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
                    if let Some(id) = picker.pending_delete.take() {
                        state.delete_request = Some(id);
                    } else {
                        return picker
                            .matches()
                            .get(picker.selected)
                            .map(|s| s.id.clone());
                    }
                }

                KeyCode::Delete => {
                    let selected_id = picker
                        .matches()
                        .get(picker.selected)
                        .map(|s| s.id.clone());
                    if let Some(id) = selected_id {
                        if picker.pending_delete.as_deref() == Some(&id) {
                            picker.pending_delete = None;
                            state.delete_request = Some(id);
                        } else {
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
                KeyCode::Right => picker.enter_provider(),
                KeyCode::Left => {
                    picker.back();
                }
                KeyCode::Enter if picker.provider.is_none() => picker.enter_provider(),
                KeyCode::Enter => {
                    if let Some(model) = picker.matches().get(picker.selected) {
                        state.chosen_model = Some(model.qualified_name());
                        state.chosen_effort = picker.effort;
                        state.model_picker = None;
                    }
                }

                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    picker.query.push(c);
                    picker.selected = 0;
                    picker.offset = 0;
                    picker.normalize_effort();
                }

                KeyCode::Backspace if picker.query.is_empty() => {
                    picker.back();
                }
                KeyCode::Backspace => {
                    picker.query.pop();
                    picker.selected = 0;
                    picker.offset = 0;
                    picker.normalize_effort();
                }

                _ => {}
            },

            Event::Paste(text) => {
                picker
                    .query
                    .extend(text.chars().filter(|c| !c.is_control()));
                picker.selected = 0;
                picker.offset = 0;
                picker.normalize_effort();
            }

            Event::Mouse(mouse) if mouse.kind == MouseEventKind::ScrollUp => picker.move_by(false),
            Event::Mouse(mouse) if mouse.kind == MouseEventKind::ScrollDown => picker.move_by(true),
            _ => {}
        }
    } else if let Some(dialog) = &mut state.marketplace_dialog {
        if let Some(source) = dialog.removing.clone() {
            if matches!(&event, Event::Key(key) if key.code == KeyCode::Enter) {
                dialog.start("正在移除市场源", &source);
                state.pending_plugin_action =
                    Some(orbit_protocol::plugins::Action::Remove { source });
            }
            return None;
        }
        if dialog.searching {
            match event {
                Event::Key(key) => match key.code {
                    KeyCode::Enter => dialog.searching = false,
                    KeyCode::Up => dialog.move_plugin(false),
                    KeyCode::Down => dialog.move_plugin(true),
                    KeyCode::Backspace => {
                        dialog.query.pop();
                        dialog.normalize_plugin();
                    }
                    KeyCode::Char('u')
                        if key
                            .modifiers
                            .contains(KeyModifiers::CONTROL) =>
                    {
                        dialog.query.clear();
                        dialog.normalize_plugin();
                    }
                    KeyCode::Char(c)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        dialog.query.push(c);
                        dialog.normalize_plugin();
                    }
                    _ => {}
                },
                Event::Paste(text) => {
                    dialog
                        .query
                        .extend(text.chars().filter(|c| !c.is_control()));
                    dialog.normalize_plugin();
                }
                _ => {}
            }
            return None;
        }
        match event {
            Event::Key(key) => match key.code {
                KeyCode::Up
                    if !dialog.adding && dialog.inspection.is_none() && dialog.viewing_plugins =>
                {
                    dialog.move_plugin(false)
                }
                KeyCode::Down
                    if !dialog.adding && dialog.inspection.is_none() && dialog.viewing_plugins =>
                {
                    dialog.move_plugin(true)
                }
                KeyCode::Up if !dialog.adding && dialog.inspection.is_none() => {
                    dialog.move_market(false)
                }
                KeyCode::Down if !dialog.adding && dialog.inspection.is_none() => {
                    dialog.move_market(true)
                }

                KeyCode::Char('/')
                    if !dialog.adding && dialog.inspection.is_none() && dialog.viewing_plugins =>
                {
                    dialog.searching = true
                }
                KeyCode::F(2)
                    if !dialog.adding && dialog.inspection.is_none() && dialog.viewing_plugins =>
                {
                    dialog.enabled_only = !dialog.enabled_only;
                    dialog.normalize_plugin();
                }
                KeyCode::Char('r' | 'R') | KeyCode::F(5)
                    if !dialog.adding
                        && dialog.inspection.is_none()
                        && !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    if let Some(source) = dialog
                        .selected_market()
                        .map(|market| market.source.clone())
                    {
                        dialog.start("正在刷新市场源", &source);
                        state.pending_plugin_action =
                            Some(orbit_protocol::plugins::Action::Refresh { source });
                    }
                }
                KeyCode::Delete
                    if !dialog.adding && dialog.inspection.is_none() && !dialog.viewing_plugins =>
                {
                    dialog.removing = dialog
                        .selected_market()
                        .map(|market| market.source.clone());
                    dialog.error = None;
                }

                KeyCode::Char('a')
                    if !dialog.adding
                        && dialog.inspection.is_none()
                        && !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    dialog.adding = true
                }

                KeyCode::Char('e')
                    if dialog
                        .inspection
                        .as_ref()
                        .is_some_and(Result::is_ok)
                        && !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    if let Some(change) = dialog.selected_change() {
                        let (operation, source, action) = match change {
                            SessionPluginChange::Enable(plugin) => (
                                "正在应用插件",
                                plugin.marketplace_source.clone(),
                                orbit_protocol::plugins::Action::Enable { plugin },
                            ),
                            SessionPluginChange::Disable {
                                marketplace_source,
                                plugin_name,
                                ..
                            } => (
                                "正在移除会话插件",
                                marketplace_source.clone(),
                                orbit_protocol::plugins::Action::Disable {
                                    marketplace_source,
                                    plugin_name,
                                },
                            ),
                        };
                        dialog.start(operation, &source);
                        state.pending_plugin_action = Some(action);
                    }
                }

                KeyCode::Enter
                    if !dialog.adding && dialog.viewing_plugins && dialog.inspection.is_none() =>
                {
                    dialog.inspect_selected_plugin()
                }
                KeyCode::Enter
                    if !dialog.adding
                        && dialog.inspection.is_none()
                        && dialog.selected_market().is_some() =>
                {
                    dialog.viewing_plugins = true;
                    dialog.normalize_plugin();
                }
                KeyCode::Enter if !dialog.adding && dialog.inspection.is_none() => {
                    dialog.adding = true
                }

                KeyCode::Enter if dialog.adding => {
                    let source = dialog.source.trim().to_string();

                    if !source.is_empty() {
                        dialog.start("正在添加市场源", &source);
                        state.pending_plugin_action =
                            Some(orbit_protocol::plugins::Action::Add { source });
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
                state.help_scroll = state
                    .help_scroll
                    .saturating_sub(super::wheel::LINES)
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
    let width = full
        .width
        .saturating_sub(full.width / 5)
        .max(96);
    // Keep the half-screen proportions, then grow with the width for long records.
    let height = (width / 4)
        .min(
            full.height
                .saturating_sub(full.height / 5),
        )
        .max(24);
    centered_size(full, width, height)
}

pub(super) fn centered_size(full: Rect, width: u16, height: u16) -> Rect {
    super::components::centered(full, width, height)
}

fn model_row(
    model: &str,
    provider: &str,
    marker: &str,
    width: usize,
    selected: bool,
) -> Line<'static> {
    use unicode_width::UnicodeWidthStr;
    let provider_width = if provider.is_empty() {
        0
    } else {
        (width / 3).min(28)
    };
    let name_width = width.saturating_sub(provider_width + 6);
    let name = super::render::fit_label(&clean(model), name_width, false);
    let provider = super::render::fit_label(&clean(provider), provider_width, false);
    let lead = super::render::fit_label(
        &format!(
            " {marker} {name}{}  ",
            " ".repeat(name_width.saturating_sub(name.width()))
        ),
        width,
        false,
    );
    let tail = super::render::fit_label(&provider, width.saturating_sub(lead.width()), false);
    let padding = " ".repeat(width.saturating_sub(lead.width() + tail.width()));
    Line::from(vec![
        Span::raw(lead),
        Span::styled(
            format!("{tail}{padding}"),
            if selected {
                Style::default()
            } else {
                Style::default().fg(crate::tui::theme::THEME.description)
            },
        ),
    ])
}

pub fn draw(frame: &mut Frame<'_>, state: &mut UiState) {
    state.dialog_rows.clear();
    state.model_update_area = None;
    if !state.dialog_open() {
        return;
    }

    if state.info_dialog.is_none()
        && let Some(picker) = &mut state.restore_picker
    {
        state.ime_cursor = picker.draw(frame, frame.area());
        return;
    }
    let area = if state.info_dialog.is_some() {
        centered_area(frame.area())
    } else if state.btw.visible() || state.subagent_dialog.is_some() {
        transcript_area(frame.area())
    } else if let Some(picker) = &state.model_picker {
        centered_size(
            frame.area(),
            96,
            picker
                .row_count()
                .saturating_add(8)
                .min(28) as u16,
        )
    } else {
        centered_area(frame.area())
    };

    let title = if state.picker.is_some() {
        "恢复会话 · 当前项目"
    } else if state.model_picker.is_some() {
        "选择模型"
    } else if state.marketplace_dialog.is_some() {
        "插件市场"
    } else {
        "Orbit 帮助"
    };

    let inner = super::components::surface(frame, area, super::components::panel());
    if state.info_dialog.is_none() && state.btw.visible() {
        state.ime_cursor = None;
        let mut media = state.media.clone();
        media.workspace = state.workspace.clone().into();
        state
            .btw
            .draw(frame, area, inner, media);
        state.ime_cursor = state.btw.cursor();
        return;
    }

    if state.info_dialog.is_none()
        && let Some(viewer) = &mut state.subagent_dialog
    {
        state.ime_cursor = None;
        let mut media = state.media.clone();
        media.workspace = state.workspace.clone().into();
        viewer.draw(frame, area, inner, media);
        state.ime_cursor = viewer.cursor();
        return;
    }

    if let Some(dialog) = &mut state.info_dialog {
        state.ime_cursor = None;
        dialog.page_areas.clear();
        let page_header_rows = u16::from(!dialog.pages.is_empty());
        let title = Rect::new(
            inner.x + 1,
            inner.y,
            inner.width.saturating_sub(2),
            1.min(inner.height),
        );
        let body = Rect::new(
            title.x,
            title
                .bottom()
                .saturating_add(1 + page_header_rows)
                .min(inner.bottom()),
            title.width,
            inner
                .height
                .saturating_sub(4 + page_header_rows),
        );
        let footer = Rect::new(
            title.x,
            inner.bottom().saturating_sub(1),
            title.width,
            1.min(inner.height),
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
        if page_header_rows > 0 && title.bottom() < inner.bottom() {
            let mut x = title.x;
            for (index, page) in dialog.pages.iter().enumerate() {
                let label = format!(" {} ", clean(&page.label));
                let width = (unicode_width::UnicodeWidthStr::width(label.as_str()) as u16)
                    .min(title.right().saturating_sub(x));
                let area = Rect::new(x, title.bottom(), width, 1);
                let style = if index == dialog.selected_page {
                    super::components::highlighted()
                } else {
                    super::components::muted()
                };
                frame.render_widget(Paragraph::new(label).style(style), area);
                dialog.page_areas.push((area, index));
                x = x
                    .saturating_add(width + 2)
                    .min(title.right());
            }
        }
        let mut text = dialog
            .pages
            .get(dialog.selected_page)
            .map(|page| page.content.clone())
            .unwrap_or_else(|| Text::from(dialog.content.clone()));
        for line in &mut text.lines {
            for span in &mut line.spans {
                span.content = super::render::safe_output(&span.content).into();
            }
        }
        let paragraph = Paragraph::new(text).wrap(Wrap { trim: false });
        dialog.page_rows = usize::from(body.height).max(1);
        let rows = paragraph.line_count(body.width);
        dialog.max_scroll = rows
            .saturating_sub(body.height as usize)
            .min(u16::MAX as usize);
        dialog.scroll = dialog.scroll.min(dialog.max_scroll);
        frame.render_widget(paragraph.scroll((dialog.scroll as u16, 0)), body);
        if dialog.max_scroll > 0 && !body.is_empty() {
            // Ratatui uses content_length - 1 as the maximum position, then
            // adds the viewport length when sizing the thumb. Supply the
            // number of valid offsets, not the total number of wrapped rows.
            let mut scrollbar = ScrollbarState::new(dialog.max_scroll + 1)
                .position(dialog.scroll)
                .viewport_content_length(body.height as usize);
            frame.render_stateful_widget(
                Scrollbar::new(ScrollbarOrientation::VerticalRight)
                    .begin_symbol(None)
                    .end_symbol(None)
                    .thumb_style(super::components::accent())
                    .track_style(super::components::muted()),
                Rect::new(inner.right().saturating_sub(1), body.y, 1, body.height),
                &mut scrollbar,
            );
        }
        let actions = if matches!(dialog.source, Some(ReportSource::Tool(_))) {
            "Tab 切页 · Alt+←/→ 切换调用 · Y 复制 · Esc 关闭"
        } else if matches!(dialog.source, Some(ReportSource::Process)) {
            "PgUp/PgDn 翻页 · Y 复制 · Esc 关闭"
        } else if dialog.source.is_some() {
            "Tab 切页 · R 刷新 · Esc 关闭"
        } else if dialog.max_scroll > 0 {
            "PgUp/PgDn 翻页 · Esc 关闭"
        } else {
            "Esc 关闭"
        };
        let actions = if let Some(target) = &dialog.target {
            format!("A {} · {actions}", target.label())
        } else {
            actions.to_owned()
        };
        let hint = if dialog.max_scroll > 0 {
            format!(
                "{}–{} / {} · 滚轮/↑↓ 滚动 · {actions}",
                if rows == 0 || body.is_empty() {
                    0
                } else {
                    dialog.scroll + 1
                },
                dialog
                    .scroll
                    .saturating_add(body.height as usize)
                    .min(rows),
                rows,
            )
        } else {
            actions.to_owned()
        };
        frame.render_widget(
            Paragraph::new(super::render::fit_label(
                &hint,
                footer.width as usize,
                false,
            ))
            .style(super::components::muted()),
            footer,
        );
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
        let feedback = picker
            .error
            .as_deref()
            .or(state.resume_loading.as_deref());
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
            picker.offset = picker.offset.min(picker.selected).max(
                picker
                    .selected
                    .saturating_sub(visible - 1),
            );
        }

        let first = picker.offset;
        let rows = picker.matches();
        let mut lines = Vec::new();
        let pending_delete_id = picker.pending_delete.clone();

        for (index, session) in rows
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
                    super::components::highlighted(),
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
        picker.back_area = None;
        let column_header = Rect {
            height: 1.min(body.height),
            ..body
        };
        let body = Rect {
            y: body.y + column_header.height,
            height: body
                .height
                .saturating_sub(column_header.height),
            ..body
        };
        frame.render_widget(
            Paragraph::new(model_row(
                if picker.provider.is_some() {
                    "模型"
                } else {
                    "供应商 / 连接"
                },
                if picker.provider.is_some() {
                    ""
                } else {
                    "模型数"
                },
                " ",
                column_header.width as usize,
                false,
            )),
            column_header,
        );
        let search = super::render::fit_label(
            &format!(
                "筛选{}：{}",
                if picker.provider.is_some() {
                    "模型"
                } else {
                    "供应商或模型"
                },
                clean(&picker.query)
            ),
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
        let update_width = 18.min(header.width);
        state.model_update_area = super::components::button(
            frame,
            Rect::new(
                header
                    .right()
                    .saturating_sub(update_width),
                header.y,
                update_width,
                header.height.min(1),
            ),
            "更新模型 F5",
            super::components::Control {
                highlighted: state.model_update_hover,
                ..Default::default()
            },
        );
        if let Some(provider) = &picker.provider {
            picker.back_area = super::components::button(
                frame,
                Rect::new(
                    header.x,
                    header.y,
                    header
                        .width
                        .saturating_sub(update_width + 1),
                    header.height.min(1),
                ),
                &format!("‹ 供应商 · {}", clean(provider)),
                super::components::Control {
                    highlighted: picker.back_hover,
                    ..Default::default()
                },
            );
        }
        let visible = body.height as usize;
        let first = if state.dialog_manual_scroll {
            picker.offset
        } else {
            picker.first_visible(visible)
        };
        let models = picker.matches();
        let rows: Vec<(String, String, bool)> = if picker.provider.is_some() {
            models
                .iter()
                .map(|model| {
                    (
                        model.model.clone(),
                        String::new(),
                        model.qualified_name() == picker.current,
                    )
                })
                .collect()
        } else {
            picker
                .providers()
                .into_iter()
                .map(|(provider, count)| {
                    let current = picker
                        .current
                        .split_once("::")
                        .is_some_and(|(name, _)| name == provider);
                    (provider, format!("{count} 个  ›"), current)
                })
                .collect()
        };
        let mut lines = Vec::new();
        for (index, model) in rows
            .iter()
            .enumerate()
            .skip(first)
            .take(visible)
        {
            state.dialog_rows.push((
                Rect::new(body.x, body.y + lines.len() as u16, body.width, 1).intersection(body),
                index,
            ));
            let selected = index == picker.selected;
            let current = model.2;
            let style = if selected {
                super::components::highlighted()
            } else {
                Style::default()
            };

            lines.push(
                model_row(
                    &model.0,
                    &model.1,
                    if current { "✓" } else { " " },
                    body.width as usize,
                    selected,
                )
                .style(style),
            );
        }

        if lines.is_empty() {
            lines.push(Line::from("没有匹配的模型。"));
        }

        fit_hit_rows(&mut state.dialog_rows, &lines, body);
        frame.render_widget(Paragraph::new(lines), body);
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(if picker.provider.is_none() {
                    "先选择供应商 / 连接，再选择模型".into()
                } else {
                    models
                        .get(picker.selected)
                        .map_or_else(String::new, |model| {
                            format!(
                                "{}{}",
                                clean(&model.qualified_name()),
                                if model.supports_vision {
                                    " · 支持图片"
                                } else {
                                    ""
                                }
                            )
                        })
                }),
                Line::from(format!(
                    "{} 个{} · ↑↓ 选择 · Enter {} · Esc 返回",
                    rows.len(),
                    if picker.provider.is_some() {
                        "模型"
                    } else {
                        "连接"
                    },
                    if picker.provider.is_some() {
                        "使用"
                    } else {
                        "展开"
                    }
                )),
            ]),
            footer,
        );
    } else if let Some(dialog) = &mut state.marketplace_dialog {
        if let Some(operation) = &dialog.pending {
            state.ime_cursor = None;
            frame.render_widget(
                Paragraph::new(Line::styled(
                    "插件市场",
                    Style::default()
                        .fg(crate::tui::theme::THEME.accent)
                        .add_modifier(Modifier::BOLD),
                )),
                header,
            );
            frame.render_widget(
                Paragraph::new(
                    operation
                        .lines()
                        .map(|line| Line::from(clean(line)))
                        .collect::<Vec<_>>(),
                )
                .wrap(Wrap { trim: false }),
                body,
            );
            frame.render_widget(Paragraph::new("操作中… · Esc 关闭，后台继续"), footer);
        } else if let Some(source) = &dialog.removing {
            state.ime_cursor = None;
            frame.render_widget(
                Paragraph::new(Line::styled(
                    "移除市场源？",
                    Style::default()
                        .fg(crate::tui::theme::THEME.accent)
                        .add_modifier(Modifier::BOLD),
                )),
                header,
            );
            frame.render_widget(
                Paragraph::new(format!(
                    "{}\n\n只移除市场登记，保留本地缓存与目录。\n已有会话保留应用时的快照。",
                    clean(source)
                ))
                .wrap(Wrap { trim: false }),
                body,
            );
            frame.render_widget(Paragraph::new("Enter 确认移除 · Esc 返回"), footer);
        } else if dialog.adding {
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
                Paragraph::new(format!(
                    "可输入本地目录、GitHub owner/repo 或 HTTPS Git URL。\n下载只保存到 Orbit \
                    用户数据目录；安装插件前仍会展示其内容。{}",
                    dialog
                        .error
                        .as_ref()
                        .map(|error| format!("\n\n{}", clean(error)))
                        .unwrap_or_default()
                ))
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
                    let description = package
                        .description
                        .as_deref()
                        .unwrap_or("未声明描述");
                    frame.render_widget(
                        Paragraph::new(vec![
                            Line::styled(
                                format!(
                                    "{} · {}",
                                    clean(&package.name),
                                    if dialog
                                        .selected_plugin()
                                        .is_some_and(|(market, plugin)| {
                                            dialog.is_enabled(market, plugin)
                                        })
                                    {
                                        "当前会话已应用"
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
                    let mut lines = Vec::new();
                    if let Some(error) = &dialog.error {
                        lines.push(Line::from(clean(error)));
                        lines.push(Line::from(""));
                    }
                    lines.extend([
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
                                "此包已应用到当前会话；Hook、MCP 和命令仍不会自动运行。"
                            } else {
                                "应用只影响当前会话；Hook、MCP 和命令不会自动运行。"
                            },
                            Style::default().fg(crate::tui::theme::THEME.secondary),
                        ),
                    ]);

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
            let indices = dialog.plugin_indices();
            let selected = indices
                .iter()
                .position(|index| *index == dialog.selected_plugin)
                .unwrap_or(0);
            if !state.dialog_manual_scroll {
                dialog.plugin_offset = dialog
                    .plugin_offset
                    .min(selected)
                    .max(selected.saturating_sub(visible - 1));
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
                        dialog
                            .error
                            .as_ref()
                            .map(|error| clean(error))
                            .unwrap_or_else(|| {
                                format!(
                                    "{} · {}{}",
                                    if dialog.enabled_only {
                                        "已启用"
                                    } else {
                                        "全部"
                                    },
                                    if dialog.searching {
                                        "搜索："
                                    } else {
                                        "筛选："
                                    },
                                    clean(&dialog.query)
                                )
                            }),
                        Style::default().fg(crate::tui::theme::THEME.secondary),
                    ),
                ]),
                header,
            );
            if dialog.searching {
                state.ime_cursor = Some(
                    (
                        header.x
                            + unicode_width::UnicodeWidthStr::width(
                                format!(
                                    "{} · 搜索：{}",
                                    if dialog.enabled_only {
                                        "已启用"
                                    } else {
                                        "全部"
                                    },
                                    clean(&dialog.query)
                                )
                                .as_str(),
                            )
                            .min(header.width.saturating_sub(1) as usize)
                                as u16,
                        header.y + 1,
                    )
                        .into(),
                );
            }
            let visible = (body.height as usize / 2).max(1);
            let first = dialog.plugin_offset;
            let mut entries = Vec::new();

            for (row, index) in indices
                .iter()
                .copied()
                .enumerate()
                .skip(first)
                .take(visible)
            {
                let plugin = &market.plugins[index];
                state.dialog_rows.push((
                    Rect::new(body.x, body.y + ((row - first) * 2) as u16, body.width, 2)
                        .intersection(body),
                    index,
                ));
                let selected = index == dialog.selected_plugin;
                let style = if selected {
                    super::components::highlighted()
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
                entries.push(Line::from(
                    if dialog.query.is_empty() && !dialog.enabled_only {
                        "这个市场没有声明插件。"
                    } else {
                        "没有符合筛选的插件。"
                    },
                ));
            }

            fit_hit_rows(&mut state.dialog_rows, &entries, body);
            frame.render_widget(Paragraph::new(entries), body);
            frame.render_widget(
                Paragraph::new(format!(
                    "{} 个插件 · ↑↓ 浏览 · Enter 检查 · / 搜索 · F2 {} · R 刷新 · Esc 返回",
                    indices.len(),
                    if dialog.enabled_only {
                        "显示全部"
                    } else {
                        "只看已启用"
                    }
                )),
                footer,
            );
        } else {
            state.ime_cursor = None;
            frame.render_widget(
                Paragraph::new(vec![
                    Line::styled(
                        "已添加的市场",
                        Style::default()
                            .fg(crate::tui::theme::THEME.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Line::styled(
                        dialog
                            .error
                            .as_ref()
                            .map(|error| clean(error))
                            .unwrap_or_default(),
                        Style::default().fg(crate::tui::theme::THEME.secondary),
                    ),
                ]),
                header,
            );
            let visible = (body.height as usize / 2).max(1);
            if !state.dialog_manual_scroll {
                dialog.market_offset = dialog
                    .market_offset
                    .min(dialog.selected_market)
                    .max(
                        dialog
                            .selected_market
                            .saturating_sub(visible - 1),
                    );
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
                    super::components::highlighted()
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
                    "{} 个市场 · ↑↓ 选择 · Enter 插件 · A 添加 · R 刷新 · Delete 移除 · Esc 返回",
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
                "Shift+Tab      切换 ask / auto / yolo",
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
