use std::collections::{HashMap, HashSet};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use orbit_agent::events::AgentEvent;
use orbit_model::types::StreamEvent;
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
    app::{ToolRow, ToolStatus, TranscriptItem, UiState},
    icons,
    markdown::{LinkTarget, LinkedLine},
    render::{fit_label, safe_output},
    theme::THEME,
};

pub(super) fn is_task(name: &str) -> bool {
    matches!(name, "spawn_agent" | "send_agent_message")
}

pub(super) fn card(row: &ToolRow, expanded: bool, width: u16) -> Option<Vec<LinkedLine>> {
    if !is_task(&row.name) || width < 10 {
        return None;
    }
    let args: Value = serde_json::from_str(&row.arguments).unwrap_or_default();
    let value: Value = serde_json::from_str(&row.output).unwrap_or_default();
    let role = match value["role"].as_str().or(args["role"].as_str()) {
        Some("review") => "审查",
        _ => "探索",
    };
    let (status, icon, color) = match value["status"].as_str() {
        Some("queued") => ("已排队", icons::LOADING, THEME.tool_running),
        Some("cancelled") => ("已取消", icons::CANCEL, THEME.tool_detail),
        Some("interrupted") => ("已中断", icons::ALERT_OUTLINE, THEME.tool_failed),
        Some("failed") => ("失败", icons::CLOSE, THEME.tool_failed),
        Some("completed") => ("完成", icons::CHECK, THEME.tool_finished),
        _ if row.status == ToolStatus::Failed => ("失败", icons::CLOSE, THEME.tool_failed),
        _ => ("运行中", icons::LOADING, THEME.tool_running),
    };
    let task = args["task"]
        .as_str()
        .or(args["message"].as_str())
        .unwrap_or("委派任务");
    let title = safe_output(&format!("{role} · {task}"));
    let id = super::tool_display::agent_session_id(&row.output);
    let label = if id.is_some() { " 查看" } else { "" };
    let prefix = format!("  {icon} ");
    let available = usize::from(width).saturating_sub(prefix.width() + label.width());
    let mut header = LinkedLine::plain(Line::from(vec![
        Span::styled(prefix, Style::default().fg(color)),
        Span::styled(
            fit_label(&title, available, false),
            Style::default().fg(THEME.tool_summary),
        ),
    ]))
    .decorate(&[0]);
    header.links.clear();
    if let Some(id) = id {
        let start = header.line.to_string().len();
        header.line.spans.push(Span::styled(
            label,
            Style::default()
                .fg(THEME.accent)
                .add_modifier(Modifier::UNDERLINED),
        ));
        header.links.push(LinkTarget {
            start,
            end: start + label.len(),
            url: format!("orbit://session/{id}"),
        });
    }
    let seconds = value["elapsed_seconds"]
        .as_u64()
        .or_else(|| value["elapsed_ms"].as_u64().map(|ms| ms / 1000));
    let calls = value["tool_calls"]
        .as_u64()
        .or(value["_activity"]["tool_calls"].as_u64());
    let current = value["_activity"]["current"]
        .as_str()
        .filter(|text| !text.is_empty() && row.status == ToolStatus::Running);
    let mut statistics = Vec::new();
    if let Some(calls) = calls {
        statistics.push(format!("{calls} 次调用"));
    }
    if let Some(seconds) = seconds {
        statistics.push(format!("{seconds}s"));
    }
    let suffix = if statistics.is_empty() {
        String::new()
    } else {
        format!(" · {}", statistics.join(" · "))
    };
    let available = usize::from(width).saturating_sub(4);
    let detail = if suffix.width() < available {
        format!(
            "{}{}",
            fit_label(
                &safe_output(current.unwrap_or(status)),
                available - suffix.width(),
                false
            ),
            suffix
        )
    } else {
        fit_label(&format!("{status}{suffix}"), available, false)
    };
    let mut lines = vec![
        header,
        LinkedLine::chrome(Line::styled(
            format!("    {detail}"),
            Style::default().fg(THEME.tool_detail),
        )),
    ];
    // Generated metadata stays out of copied task text; result bodies use the shared renderer.
    if expanded {
        if let Some(model) = value["model"].as_str() {
            lines.push(LinkedLine::chrome(Line::styled(
                format!(
                    "    {}",
                    fit_label(
                        &safe_output(model),
                        usize::from(width).saturating_sub(4),
                        false
                    )
                ),
                Style::default().fg(THEME.tool_detail),
            )));
        }
        lines.extend(
            task.lines()
                .flat_map(|line| {
                    LinkedLine::plain(Line::from(safe_output(line))).wrap(width.saturating_sub(4))
                })
                .map(|line| line.framed(vec![Span::raw("    ")], vec![])),
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
            lines.extend(
                super::markdown::document(content, width.saturating_sub(2))
                    .into_iter()
                    .flat_map(|line| line.wrap(width.saturating_sub(2)))
                    .map(|line| line.framed(vec![Span::raw("  ")], vec![])),
            );
        }
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
