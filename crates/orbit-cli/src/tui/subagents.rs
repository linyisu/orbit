use std::collections::{HashMap, HashSet};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use orbit_agent::events::AgentEvent;
use orbit_model::types::StreamEvent;
use orbit_store::types::Snapshot;
use orbit_store::{store::Store, tui_sessions::SessionRevision};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use serde_json::{Value, json};
use unicode_width::UnicodeWidthStr;

use super::{
    app::RunStatus,
    app::{ToolRow, ToolStatus, TranscriptItem, UiState},
    markdown::{LinkTarget, LinkedLine},
    render::{fit_label, safe_output},
    theme::THEME,
};

pub(super) use super::transcript_dialog::Action as ViewerAction;

pub(super) struct Viewer {
    id: String,
    view: super::transcript_dialog::TranscriptDialog,
    snapshot: Option<Snapshot>,
    sync: super::session_sync::SessionSync,
    error: Option<String>,
}

impl std::fmt::Debug for Viewer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Viewer")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

pub(super) fn open(state: &mut UiState, id: String) {
    state.selection.clear();
    state.input_dragging = false;
    state.cursor_motion.clear();
    let mut view = super::transcript_dialog::TranscriptDialog::new("");
    view.enable_input();
    view.block_submission(true);
    view.ui.resume_loading = Some("加载子代理记录…".into());
    state.subagent_dialog = Some(Box::new(Viewer {
        id,
        view,
        snapshot: None,
        sync: Default::default(),
        error: None,
    }));
}

impl Viewer {
    /// Read saved events without acquiring a writer or changing the active parent session.
    pub async fn poll(&mut self, store: &Store, parent: &str) -> bool {
        if self.error.is_some() || self.view.ui.selection.active() {
            return false;
        }
        let result: anyhow::Result<bool> = async {
            if self.snapshot.is_none() {
                anyhow::ensure!(
                    store.subagent_parent(&self.id).await?.as_deref() == Some(parent),
                    "该记录不是当前会话的子代理。"
                );
                let snapshot = store.load(&self.id).await?;
                self.view.ui.model = snapshot.session.model.clone();
                self.view.ui.subagent_parent = Some(parent.into());
                self.view.ui.follow_bottom = snapshot
                    .runs
                    .last()
                    .is_some_and(|run| run.status == "running");
                self.view.ui.resume_loading = None;
                self.snapshot = Some(snapshot);
            }
            let mut history = Vec::new();
            self.sync
                .poll_read_only(
                    store,
                    self.snapshot.as_mut().unwrap(),
                    &mut self.view.ui,
                    &mut history,
                )
                .await
        }
        .await;
        match result {
            Ok(changed) => {
                self.view.block_submission(false);
                changed
            }
            Err(error) => {
                self.error = Some(format!("无法读取子代理记录：{error}"));
                self.view.ui.resume_loading = None;
                self.view.ui.status = RunStatus::Failed;
                true
            }
        }
    }

    pub fn tick(&mut self) -> bool {
        self.view.tick()
    }

    pub fn draw(&mut self, frame: &mut Frame<'_>, area: Rect, inner: Rect) {
        let status = if self.view.ui.resume_loading.is_some() {
            "加载中"
        } else {
            match self.view.ui.status {
                RunStatus::Running => "运行中",
                RunStatus::Cancelling => "取消中",
                RunStatus::Completed => "完成",
                RunStatus::Failed => "失败",
                RunStatus::Cancelled => "已取消",
                RunStatus::Idle => "等待运行",
            }
        };
        let metadata = self.view.ui.resume_loading.clone().unwrap_or_else(|| {
            if self.view.ui.model.is_empty() {
                "记录不可用".into()
            } else {
                format!(
                    "{} · {} 次调用",
                    self.view.ui.model, self.view.ui.tool_calls
                )
            }
        });
        self.view.draw(
            &format!("子代理 · {status}"),
            &metadata,
            self.error.as_deref(),
            frame,
            area,
            inner,
        );
    }

    pub fn handle(&mut self, event: &Event) -> std::io::Result<ViewerAction> {
        self.view.handle(event)
    }

    pub async fn send(&mut self, tools: &orbit_agent::tool::ToolRegistry, message: String) {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let call = orbit_model::types::ToolCall::function(
            format!("ui-followup-{}-{stamp}", std::process::id()),
            "send_agent_message",
            json!({"agent_id":self.id,"message":message}),
        );
        match tools
            .execute(call, tokio_util::sync::CancellationToken::new())
            .await
        {
            Ok(_) => self.view.submitted(),
            Err(error) => self.view.submit_error(error),
        }
    }

    pub fn cursor(&self) -> Option<ratatui::layout::Position> {
        self.view.ui.ime_cursor
    }

    pub fn link_error(&mut self, error: &anyhow::Error) {
        self.view
            .ui
            .report_local(TranscriptItem::CommandError(format!(
                "无法打开链接：{error}"
            )));
    }
}

pub(super) fn is_task(name: &str) -> bool {
    matches!(name, "spawn_agent" | "send_agent_message")
}

fn status(row: &ToolRow, value: &Value) -> &'static str {
    match value["status"].as_str() {
        Some("queued") => "已排队",
        Some("cancelled") => "已取消",
        Some("interrupted") => "已中断",
        Some("failed") => "失败",
        Some("completed") => "完成",
        _ if row.status == ToolStatus::Failed => "失败",
        _ => "运行中",
    }
}

fn task(args: &Value) -> &str {
    args["task"]
        .as_str()
        .or(args["message"].as_str())
        .unwrap_or("委派任务")
}

fn statistics(value: &Value) -> Vec<String> {
    let seconds = value["elapsed_seconds"]
        .as_u64()
        .or_else(|| value["elapsed_ms"].as_u64().map(|ms| ms / 1000));
    let calls = value["tool_calls"]
        .as_u64()
        .or(value["_activity"]["tool_calls"].as_u64());
    let mut parts = Vec::new();
    if let Some(calls) = calls {
        parts.push(format!("{calls} 次调用"));
    }
    if let Some(seconds) = seconds {
        parts.push(format!("{seconds}s"));
    }
    parts
}

fn activity(row: &ToolRow, value: &Value) -> String {
    let current = value["_activity"]["current"]
        .as_str()
        .filter(|text| !text.is_empty() && row.status == ToolStatus::Running);
    let mut parts = vec![safe_output(current.unwrap_or(status(row, value)))];
    parts.extend(statistics(value));
    parts.join(" · ")
}

pub(super) fn header(
    row: &ToolRow,
    color: ratatui::style::Color,
    width: u16,
) -> Option<LinkedLine> {
    if !is_task(&row.name) {
        return None;
    }
    let args: Value = serde_json::from_str(&row.arguments).unwrap_or_default();
    let value: Value = serde_json::from_str(&row.output).unwrap_or_default();
    let role = match value["role"].as_str().or(args["role"].as_str()) {
        Some("review") => "审查",
        _ => "探索",
    };
    let id = super::tool_display::agent_session_id(&row.output).filter(|_| width >= 6);
    let padding = if width >= 8 { "  " } else { " " };
    let button_width = if id.is_some() {
        "查看".width() + padding.width() * 2
    } else {
        0
    };
    let available = usize::from(width).saturating_sub(button_width);
    let name = fit_label(&row.name, available, false);
    let remaining = available.saturating_sub(name.width() + 1);
    let mut spans = vec![Span::styled(name, Style::default().fg(color))];
    let mut decoration = Vec::new();
    if remaining > 0 {
        let mut statistics = vec![status(row, &value).into()];
        statistics.extend(self::statistics(&value));
        let statistics = fit_label(
            &format!(" · {}", statistics.join(" · ")),
            remaining.saturating_sub(8),
            false,
        );
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            fit_label(
                &safe_output(&format!("{role} · {}", task(&args))),
                remaining.saturating_sub(statistics.width()),
                false,
            ),
            Style::default().fg(THEME.tool_summary),
        ));
        decoration.push(spans.len());
        spans.push(Span::styled(
            statistics,
            Style::default().fg(THEME.tool_detail),
        ));
    }
    let mut header = LinkedLine::plain(Line::from(spans));
    header.links.clear();
    if let Some(id) = id {
        header.line.spans.push(Span::raw(padding));
        let start = header.line.to_string().len();
        decoration.push(header.line.spans.len());
        header.line.spans.push(Span::styled(
            "查看",
            Style::default()
                .fg(THEME.accent)
                .add_modifier(Modifier::UNDERLINED),
        ));
        header.line.spans.push(Span::raw(padding));
        header.links.push(LinkTarget {
            start,
            end: start + "查看".len(),
            url: format!("orbit://session/{id}"),
        });
    }
    Some(header.decorate(&decoration))
}

pub(super) fn details(row: &ToolRow, width: u16) -> Option<Vec<LinkedLine>> {
    if !is_task(&row.name) {
        return None;
    }
    let args: Value = serde_json::from_str(&row.arguments).unwrap_or_default();
    let value: Value = serde_json::from_str(&row.output).unwrap_or_default();
    let mut lines = vec![LinkedLine::chrome(Line::styled(
        activity(row, &value),
        Style::default().fg(THEME.tool_detail),
    ))];
    if let Some(model) = value["model"].as_str() {
        lines.push(LinkedLine::chrome(Line::styled(
            safe_output(model),
            Style::default().fg(THEME.tool_detail),
        )));
    }
    lines.extend(
        task(&args)
            .lines()
            .map(|line| LinkedLine::plain(Line::from(safe_output(line)))),
    );
    for content in [
        value["summary"].as_str(),
        value["error"].as_str(),
        (row.status == ToolStatus::Failed && value.is_null()).then_some(row.output.as_str()),
    ]
    .into_iter()
    .flatten()
    .filter(|text| !text.is_empty())
    {
        lines.push(LinkedLine::plain(Line::default()));
        lines.extend(super::markdown::document(content, width));
    }
    Some(lines)
}

#[derive(Default)]
struct Activity {
    revision: Option<SessionRevision>,
    events: usize,
    tool_calls: usize,
    current: String,
}

impl Activity {
    fn apply(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TurnStarted { .. } => self.current = "准备请求".into(),
            AgentEvent::Model(StreamEvent::ReasoningDelta(_)) => self.current = "思考中".into(),
            AgentEvent::Model(StreamEvent::TextDelta(_)) => self.current = "生成回复".into(),
            AgentEvent::ToolStarted { call } => {
                self.tool_calls += 1;
                self.current = format!(
                    "{} {}",
                    call.function.name,
                    super::tool_display::tool_summary(
                        &call.function.name,
                        &call.function.arguments,
                        ""
                    )
                );
            }
            AgentEvent::CompactionStarted => self.current = "压缩上下文".into(),
            _ => {}
        }
    }
}

/// Read journal changes for child runs without retaining their message bodies.
#[derive(Default)]
pub(super) struct Progress {
    runs: HashMap<(String, i64), Activity>,
}

#[cfg(test)]
mod tests;

impl Progress {
    pub async fn poll(&mut self, store: &Store, state: &mut UiState) -> anyhow::Result<bool> {
        let mut retained = HashSet::new();
        let mut changed = false;
        for (index, item) in state.transcript.iter_mut().enumerate() {
            let TranscriptItem::Tool(row) = item else {
                continue;
            };
            if !is_task(&row.name) {
                continue;
            }
            let Some(id) = super::tool_display::agent_session_id(&row.output) else {
                continue;
            };
            let Ok(mut value) = serde_json::from_str::<Value>(&row.output) else {
                continue;
            };
            let Some(run) = value["run_id"].as_i64() else {
                continue;
            };
            let key = (id.clone(), run);
            retained.insert(key.clone());
            let activity = self.runs.entry(key).or_default();
            if row.status == ToolStatus::Running
                || activity
                    .revision
                    .as_ref()
                    .is_none_or(|r| r.status == "running")
            {
                let Some(revision) = store.session_revision(&id).await? else {
                    continue;
                };
                // A continuation belongs to a new card, never to the original invocation.
                if revision.run != run {
                    continue;
                }
                if activity.revision.as_ref() != Some(&revision) {
                    let events = store.run_events_after(&id, run, activity.events).await?;
                    activity.events += events.len();
                    for event in events {
                        activity.apply(event);
                    }
                    activity.revision = Some(revision);
                }
            }
            let next = json!({"tool_calls":activity.tool_calls,"current":activity.current});
            if value["_activity"] != next {
                value["_activity"] = next;
                row.output = value.to_string();
                state.transcript_layout.invalidate_from(index);
                changed = true;
            }
        }
        self.runs.retain(|key, _| retained.contains(key));
        Ok(changed)
    }
}

pub(super) fn draw_parent(frame: &mut Frame<'_>, state: &mut UiState, area: Rect) {
    state.subagent_parent_area = Rect::default();
    if state.subagent_parent.is_none() || area.height == 0 {
        return;
    }
    let text = fit_label(
        "返回父会话 · Alt+←",
        usize::from(area.width).saturating_sub(4),
        false,
    );
    let area = Rect::new(area.x + 2, area.y, text.width() as u16, 1);
    let mut style = Style::default().fg(THEME.accent);
    if state.subagent_parent_hovered {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    frame.render_widget(Paragraph::new(text).style(style), area);
    state.subagent_parent_area = area;
}

pub(super) fn handle_parent(state: &mut UiState, event: &Event) -> Option<bool> {
    let parent = state.subagent_parent.as_ref()?;
    if state.dialog_open()
        || state.approval.is_some()
        || state.restore_picker.is_some()
        || state.selection.dragging()
    {
        state.subagent_parent_pressed = false;
        return None;
    }
    match event {
        Event::Key(key)
            if key.kind != KeyEventKind::Release
                && key.code == KeyCode::Left
                && key.modifiers == KeyModifiers::ALT =>
        {
            state.selection.clear();
            state.subagent_parent_pressed = false;
            state.resume_request = Some(parent.clone());
            Some(true)
        }
        Event::Mouse(mouse)
            if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && state
                    .subagent_parent_area
                    .contains((mouse.column, mouse.row).into()) =>
        {
            state.subagent_parent_pressed = !state.selection.active();
            state.selection.clear();
            Some(true)
        }
        Event::Mouse(mouse)
            if mouse.kind == MouseEventKind::Up(MouseButton::Left)
                && state.subagent_parent_pressed =>
        {
            state.subagent_parent_pressed = false;
            if state
                .subagent_parent_area
                .contains((mouse.column, mouse.row).into())
            {
                state.resume_request = Some(parent.clone());
            }
            Some(true)
        }
        Event::Mouse(mouse)
            if matches!(
                mouse.kind,
                MouseEventKind::Drag(_) | MouseEventKind::Down(_)
            ) =>
        {
            state.subagent_parent_pressed = false;
            None
        }
        Event::Mouse(mouse) if mouse.kind == MouseEventKind::Moved => {
            let hovered = state
                .subagent_parent_area
                .contains((mouse.column, mouse.row).into());
            let changed = state.subagent_parent_hovered != hovered;
            state.subagent_parent_hovered = hovered;
            changed.then_some(true)
        }
        _ => None,
    }
}
