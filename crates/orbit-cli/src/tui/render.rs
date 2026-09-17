use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Paragraph},
};

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::app::{RunStatus, ToolStatus, TranscriptItem, UiState};

const ACCENT: Color = Color::Rgb(218, 139, 109);
// Use the terminal foreground for readable text on both light and dark themes.
// Reserve subdued colors for borders and decorative artwork.
const MUTED: Color = Color::Reset;
const BORDER: Color = Color::Rgb(85, 85, 85);
const MODEL_CHANGE: Color = Color::Rgb(145, 145, 145);

fn muted(text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), Style::default().fg(MUTED))
}

pub fn draw(frame: &mut Frame<'_>, state: &mut UiState) {
    let area = frame.area();
    state.selection.viewport = Rect::default();
    state.selection.input = Rect::default();
    state.selection.footer = Rect::default();
    state.selection.approval = Rect::default();
    state.ime_cursor = None;
    state.selection.reset_caret();
    state.input_area = Rect::default();
    state.attachment_areas.clear();
    let width = area.width.saturating_sub(6) as usize;
    state.input_width = width;
    let (input_lines, cursor) = state.input.visual_lines(width);
    let input_height = input_lines
        .len()
        .clamp(1, 6)
        .min(area.height.saturating_sub(8) as usize) as u16;
    let attachment_height = u16::from(!state.attachments.is_empty());
    let menu = state.menu();
    let panel_height = if !menu.is_empty() {
        menu.len() as u16 + 1
    } else {
        0
    };
    let approval_height = if state.approval.is_some() {
        (area.height / 2).min(if state.approval_expanded { 18 } else { 12 })
    } else {
        0
    };
    let areas = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(approval_height),
        Constraint::Length(input_height + attachment_height + 2),
        Constraint::Length(
            panel_height.min(
                area.height
                    .saturating_sub(input_height + attachment_height + 8),
            ),
        ),
        Constraint::Length(1),
    ])
    .split(area);

    draw_header(frame, state, areas[0]);
    draw_transcript(frame, state, areas[1]);
    draw_status(frame, state, areas[2]);
    if state.approval.is_some() {
        super::approval::draw_in(frame, state, areas[3]);
        state.selection.approval = areas[3];
    }
    draw_composer(frame, state, areas[4], input_lines, cursor);
    draw_panel(frame, state, areas[5]);
    frame.render_widget(
        Paragraph::new(footer(state, areas[6].width as usize)),
        areas[6],
    );
    state.selection.footer = areas[6];
    super::dialogs::draw(frame, state);
}

/// Clip at grapheme boundaries, retaining the useful end of filesystem paths.
pub(super) fn fit_label(text: &str, width: usize, tail: bool) -> String {
    if text.width() <= width {
        return text.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut parts = Vec::new();
    let mut used = 1;
    let graphemes: Vec<_> = text.graphemes(true).collect();
    let iter: Box<dyn Iterator<Item = &&str>> = if tail {
        Box::new(graphemes.iter().rev())
    } else {
        Box::new(graphemes.iter())
    };
    for part in iter {
        if used + part.width() > width {
            break;
        }
        used += part.width();
        parts.push(*part);
    }
    if tail {
        parts.reverse();
        format!("…{}", parts.concat())
    } else {
        format!("{}…", parts.concat())
    }
}

fn footer(state: &UiState, width: usize) -> Line<'static> {
    let mode = state.permission_mode.label();
    let model_label = if state.attachments.is_empty() {
        safe_output(&state.model)
    } else {
        format!(
            "{} · 图片 {}",
            safe_output(&state.model),
            state.attachments.len()
        )
    };
    let effort = state
        .reasoning_effort
        .map(|effort| format!("思考 {}", effort.label()))
        .unwrap_or_else(|| "思考 —".into());
    let right = format!("{model_label} · {effort} · {mode}");
    let available = width.saturating_sub(4);
    let right = fit_label(&right, available.saturating_sub(8), false);
    let path = std::env::var_os("HOME")
        .and_then(|home| {
            std::path::Path::new(&state.workspace)
                .strip_prefix(home)
                .ok()
                .map(|suffix| {
                    if suffix.as_os_str().is_empty() {
                        "~".into()
                    } else {
                        format!("~/{}", suffix.display())
                    }
                })
        })
        .unwrap_or_else(|| state.workspace.clone());
    let path_width = available.saturating_sub(right.width());
    let path = fit_label(&safe_output(&path), path_width, true);
    let gap = available.saturating_sub(path.width() + right.width());
    Line::from(vec![
        Span::raw("  "),
        Span::styled(path, Style::default().fg(Color::Rgb(170, 170, 170))),
        Span::raw(" ".repeat(gap)),
        Span::raw(right),
        Span::raw("  "),
    ])
}

fn draw_header(frame: &mut Frame<'_>, _state: &UiState, area: Rect) {
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "  ◇ Orbit",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ))),
        area,
    );
}

fn wordmark() -> Vec<Line<'static>> {
    [
        " ██████╗ ██████╗ ██████╗ ██╗████████╗",
        "██╔═══██╗██╔══██╗██╔══██╗██║╚══██╔══╝",
        "██║   ██║██████╔╝██████╔╝██║   ██║",
        "██║   ██║██╔══██╗██╔══██╗██║   ██║",
        "╚██████╔╝██║  ██║██████╔╝██║   ██║",
        " ╚═════╝ ╚═╝  ╚═╝╚═════╝ ╚═╝   ╚═╝",
    ]
    .into_iter()
    .map(|line| {
        Line::from(Span::styled(
            line,
            Style::default().fg(Color::Rgb(230, 222, 210)),
        ))
    })
    .collect()
}

fn welcome() -> Vec<Line<'static>> {
    let rear = Style::default().fg(Color::Rgb(112, 80, 69));
    let front = Style::default().fg(ACCENT);
    let mut canvas = vec![vec![(' ', rear); 58]; 12];
    let stamp =
        |canvas: &mut Vec<Vec<(char, Style)>>, x: usize, y: usize, text: &str, style: Style| {
            for (offset, ch) in text.chars().enumerate() {
                if ch != ' ' {
                    canvas[y][x + offset] = (ch, style);
                }
            }
        };
    // The rear arc is occluded by the letters; the foreground arc crosses them.
    let back_arc = [
        "",
        "",
        "                                       ╱───────────────",
        "                             ╱─────────                ╲",
        "                      ╱──────",
        "               ╱──────",
        "          ╱────",
        "      ╱───",
        "   ╱──",
        "  ─",
        "",
        "",
    ];
    for (y, row) in back_arc.iter().enumerate() {
        stamp(&mut canvas, 0, y, row, rear);
    }
    for (y, row) in wordmark().into_iter().enumerate() {
        let mut x = 10;
        for span in row.spans {
            stamp(&mut canvas, x, y + 3, &span.content, span.style);
            x += span.content.chars().count();
        }
    }
    let front_arc = [
        "",
        "",
        "",
        "                                                       ╱",
        "                                                    ╱──",
        "                                                ╱───",
        "                                           ╱────",
        "                                    ╱──────",
        "                             ╱──────",
        "  ─                ╱─────────",
        "   ╲───────────────",
        "",
    ];
    for (y, row) in front_arc.iter().enumerate() {
        stamp(&mut canvas, 0, y, row, front);
    }
    for (x, y, star) in [(7, 1, "✦"), (50, 10, "✧"), (53, 1, "·"), (16, 11, "·")] {
        stamp(&mut canvas, x, y, star, front);
    }
    canvas
        .into_iter()
        .map(|row| {
            Line::from(
                row.into_iter()
                    .map(|(ch, style)| Span::styled(ch.to_string(), style))
                    .collect::<Vec<_>>(),
            )
        })
        .collect()
}

fn draw_transcript(frame: &mut Frame<'_>, state: &mut UiState, area: Rect) {
    state.selection.viewport = Rect {
        x: area.x + 2,
        // Keep one blank column between wrapped output and the navigation dots.
        width: area.width.saturating_sub(5),
        ..area
    };
    if state.transcript.is_empty() {
        state.navigation.reset_area();
        let mut logo = welcome();
        let mut width = logo.iter().map(Line::width).max().unwrap_or(0) as u16;
        if width > area.width.saturating_sub(4) || logo.len() > area.height as usize {
            logo = wordmark();
            width = logo.iter().map(Line::width).max().unwrap_or(0) as u16;
        }
        if width > area.width.saturating_sub(4) || logo.len() > area.height as usize {
            logo = vec![Line::from(Span::styled(
                "Orbit",
                Style::default().fg(ACCENT),
            ))];
            width = 5;
        }
        let height = logo.len() as u16;
        let centered = Rect::new(
            area.x + area.width.saturating_sub(width) / 2,
            area.y + u16::from(area.height > height),
            width,
            height,
        );
        frame.render_widget(Paragraph::new(logo), centered);
        return;
    }
    let width = state.selection.viewport.width;
    let previous_max = state.max_scroll;
    let previous_offset = previous_max.saturating_sub(state.scroll);
    let resize_anchor = (!state.follow_bottom
        && state.transcript_layout.width() != 0
        && state.transcript_layout.width() != width)
        .then(|| state.transcript_layout.anchor_at(previous_offset))
        .flatten();
    state
        .transcript_layout
        .update(&state.transcript, state.expanded, width);
    state.max_scroll = state
        .transcript_layout
        .height()
        .saturating_sub(area.height as usize);
    if !state.follow_bottom {
        if let Some(offset) =
            resize_anchor.and_then(|anchor| state.transcript_layout.offset_for_anchor(anchor))
        {
            state.scroll = state
                .max_scroll
                .saturating_sub(offset.min(state.max_scroll));
        } else {
            state.scroll = state
                .scroll
                .saturating_add(state.max_scroll.saturating_sub(previous_max));
        }
    }
    state.scroll = state.scroll.min(state.max_scroll);
    let offset = if state.follow_bottom {
        state.max_scroll
    } else {
        state.max_scroll.saturating_sub(state.scroll)
    };
    state
        .selection
        .transcript_view(offset, &state.transcript_layout);
    state
        .transcript_layout
        .draw(frame, state.selection.viewport, offset);
    super::navigation::draw(frame, state, area, offset);
}

pub(super) fn transcript_lines(items: &[TranscriptItem], expanded: bool) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for item in items {
        match item {
            TranscriptItem::User(text) => {
                for (index, line) in text.lines().enumerate() {
                    lines.push(Line::from(vec![
                        Span::styled(
                            if index == 0 { "❯ " } else { "  " },
                            Style::default().fg(ACCENT),
                        ),
                        Span::styled(
                            line.to_owned(),
                            Style::default().add_modifier(Modifier::BOLD),
                        ),
                    ]));
                }
                lines.push(Line::from(""));
            }
            TranscriptItem::Assistant(text) => {
                if text.is_empty() {
                    continue;
                }
                let mut code = None;
                for (index, line) in text.lines().enumerate() {
                    lines.push(assistant_line(line, index, &mut code));
                }
                lines.push(Line::from(""));
            }
            TranscriptItem::Thinking(text) => {
                lines.extend(text.lines().map(thinking_line));
            }
            TranscriptItem::Tool(row) => {
                let (marker, color) = match row.status {
                    ToolStatus::Running => ("◌", ACCENT),
                    ToolStatus::Finished => ("✓", Color::Green),
                    ToolStatus::Failed => ("×", Color::Red),
                };
                lines.push(Line::from(Span::styled(
                    format!("  {marker} {} {}", row.name, tool_summary(&row.arguments)),
                    Style::default().fg(color),
                )));
                if expanded
                    && let Ok(value) = serde_json::from_str::<serde_json::Value>(&row.arguments)
                    && let Some(command) = value["command"].as_str()
                {
                    lines.extend(
                        command
                            .lines()
                            .map(|line| Line::from(muted(format!("    {line}")))),
                    );
                }
                let output = serde_json::from_str::<serde_json::Value>(&row.output)
                    .ok()
                    .and_then(|value| {
                        if value.get("stdout").is_some() {
                            Some(format!(
                                "exit {} · timeout {} · truncated {}\n{}{}",
                                value["exit_code"],
                                value["timed_out"],
                                value["output_truncated"],
                                value["stdout"].as_str().unwrap_or_default(),
                                value["stderr"].as_str().unwrap_or_default()
                            ))
                        } else if let Some(bytes) = value["written_bytes"].as_u64() {
                            Some(format!(
                                "已{} · {bytes} 字节",
                                if value["created"].as_bool() == Some(true) {
                                    "创建"
                                } else {
                                    "写入"
                                }
                            ))
                        } else {
                            value["content"].as_str().map(str::to_owned)
                        }
                    })
                    .unwrap_or_else(|| row.output.clone());
                for line in output.lines().take(if expanded { usize::MAX } else { 2 }) {
                    let preview = if expanded {
                        line.to_owned()
                    } else {
                        compact_preview(line, 160)
                    };
                    lines.push(Line::from(muted(format!("    {preview}"))));
                }
                if !expanded && output.lines().count() > 2 {
                    lines.push(Line::from(muted("    …")));
                }
            }
            TranscriptItem::ModelChanged(text) => {
                lines.push(
                    Line::from(Span::styled(
                        text.to_owned(),
                        Style::default().fg(MODEL_CHANGE),
                    ))
                    .alignment(Alignment::Center),
                );
                lines.push(Line::from(""));
            }
            TranscriptItem::Error(text) | TranscriptItem::Notice(text) => {
                let error = matches!(item, TranscriptItem::Error(_));
                for line in text.lines() {
                    lines.push(Line::from(Span::styled(
                        format!("{} {line}", if error { "×" } else { "›" }),
                        Style::default().fg(if error { Color::Red } else { MUTED }),
                    )));
                }
                lines.push(Line::from(""));
            }
        }
    }
    for line in &mut lines {
        for span in &mut line.spans {
            span.content = safe_output(&span.content).into();
        }
    }
    lines
}

// Display budgets only: original arguments/results stay intact for execution and storage.
pub(super) fn compact_preview(text: &str, limit: usize) -> String {
    let text = safe_output(text);
    let mut chars = text.chars();
    let mut result: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        result.push('…');
    }
    result
}

fn tool_summary(arguments: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return String::new();
    };
    let target = value["path"]
        .as_str()
        .or_else(|| value["command"].as_str())
        .or_else(|| value["url"].as_str())
        .unwrap_or_default();
    let mut summary = compact_preview(target.lines().next().unwrap_or_default(), 100);
    if target.lines().count() > 1 {
        summary.push_str(" …");
    }
    if let Some(content) = value["content"].as_str() {
        summary.push_str(&format!(
            " · {} 行 · {} 字节",
            content.lines().count(),
            content.len()
        ));
    }
    summary
}

pub(super) fn assistant_line(
    line: &str,
    index: usize,
    code: &mut super::markdown::Fence,
) -> Line<'static> {
    super::markdown::line(line, index, code)
}

pub(super) fn thinking_line(line: &str) -> Line<'static> {
    Line::from(vec![Span::raw("  ┊ "), Span::raw(line.to_owned())])
        .style(Style::default().fg(Color::Rgb(170, 170, 170)))
}

pub(super) fn sanitize_line(line: &mut Line<'_>) {
    for span in &mut line.spans {
        span.content = safe_output(&span.content).into();
    }
}

pub(super) fn safe_output(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c == '\t' {
                "    ".into()
            } else if c.is_control() {
                c.escape_default().to_string()
            } else {
                c.to_string()
            }
        })
        .collect()
}

fn draw_status(frame: &mut Frame<'_>, state: &UiState, area: Rect) {
    let elapsed = state
        .started_at
        .map(|time| time.elapsed())
        .unwrap_or(state.elapsed);
    let text = match state.status {
        RunStatus::Idle => String::new(),
        RunStatus::Running => {
            let frames = ["✶", "✳", "✢", "✻"];
            format!(
                "  {} 正在响应…  {:.1}s  ·  第 {} 轮  ·  esc 取消",
                frames[(elapsed.as_millis() / 160) as usize % frames.len()],
                elapsed.as_secs_f32(),
                state.turn.max(1)
            )
        }
        RunStatus::Cancelling => "  ◌ 正在取消…".into(),
        RunStatus::Completed => format!("  ✓ 完成  ·  {:.1}s", elapsed.as_secs_f32()),
        RunStatus::Failed => "  × 请求失败 · 请查看上方错误，可重新发送".into(),
        RunStatus::Cancelled => "  ◇ 已取消 · 可以继续输入".into(),
    };
    frame.render_widget(
        Paragraph::new(Span::styled(text, Style::default().fg(ACCENT))),
        area,
    );
}

fn draw_composer(
    frame: &mut Frame<'_>,
    state: &mut UiState,
    area: Rect,
    lines: Vec<String>,
    cursor: (usize, usize),
) {
    let area = Rect {
        x: area.x + 2,
        width: area.width.saturating_sub(4),
        ..area
    };
    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(BORDER));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let attachment_height = u16::from(!state.attachments.is_empty());
    if attachment_height > 0 {
        if state
            .attachment_cursor
            .is_some_and(|index| index >= state.attachments.len())
        {
            state.attachment_cursor = None;
        }
        let right = inner.right();
        let mut x = inner.x + 2;
        for (index, attachment) in state.attachments.iter().enumerate() {
            if x >= right {
                break;
            }
            let label = format!("[图片 {} · {}]", index + 1, safe_output(attachment.name()));
            let label = fit_label(&label, (right - x) as usize, false);
            let width = label.width().min((right - x) as usize) as u16;
            if width == 0 {
                break;
            }
            let chip = Rect::new(x, inner.y, width, 1);
            let style = if state.attachment_cursor == Some(index) {
                Style::default()
                    .fg(Color::Black)
                    .bg(ACCENT)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(ACCENT)
            };
            frame.render_widget(Paragraph::new(Span::styled(label, style)), chip);
            state.attachment_areas.push(chip);
            x = x.saturating_add(width + 2);
        }
    }
    let prompt_area = Rect {
        y: inner.y + attachment_height,
        height: inner.height.saturating_sub(attachment_height),
        ..inner
    };
    frame.render_widget(
        Paragraph::new(Span::styled("❯", Style::default().fg(ACCENT))),
        Rect {
            width: 2,
            ..prompt_area
        },
    );
    let text_area = Rect {
        x: prompt_area.x + 2,
        width: prompt_area.width.saturating_sub(2),
        ..prompt_area
    };
    let scroll = cursor
        .1
        .saturating_sub(text_area.height.saturating_sub(1) as usize);
    state.input_area = text_area;
    state.selection.input = text_area;
    state.input_scroll = scroll;
    // IME preedit belongs to the terminal; placeholder text underneath it is misleading.
    let text = Text::from(
        lines
            .into_iter()
            .skip(scroll)
            .map(Line::from)
            .collect::<Vec<_>>(),
    );
    frame.render_widget(Paragraph::new(text), text_area);
    if text_area.width > 0
        && text_area.height > 0
        && !state.selection.active()
        && state.attachment_cursor.is_none()
    {
        // Draw a steady caret; restore the hidden hardware cursor after flushing.
        let x = text_area.x + (cursor.0 as u16).min(text_area.width - 1);
        let y = text_area.y + (cursor.1.saturating_sub(scroll) as u16).min(text_area.height - 1);
        state.ime_cursor = Some((x, y).into());
        state
            .selection
            .set_caret((x, y), frame.buffer_mut()[(x, y)].clone());
        frame.buffer_mut()[(x, y)].set_style(Style::default().bg(Color::Gray).fg(Color::Black));
        // Overwriting half of a CJK glyph with the caret can color its other
        // half in the terminal even though that trailing buffer cell is blank.
        // Explicitly repaint the adjacent cell after a narrow caret write.
        if frame.buffer_mut()[(x, y)].symbol().width() == 1 && x + 1 < text_area.right() {
            frame.buffer_mut()[(x + 1, y)]
                .set_diff_option(ratatui::buffer::CellDiffOption::AlwaysUpdate);
        }
    }
}

fn draw_panel(frame: &mut Frame<'_>, state: &UiState, area: Rect) {
    let menu = state.menu();
    let visible = area.height.saturating_sub(1).max(1) as usize;
    let first = state.menu_index.saturating_sub(visible - 1);
    let lines = if !menu.is_empty() {
        menu.iter()
            .enumerate()
            .skip(first)
            .take(visible)
            .map(|(index, command)| {
                let selected = index == state.menu_index.min(menu.len() - 1);
                Line::from(vec![
                    Span::styled(
                        format!(
                            "  {} {:<10}",
                            if selected { "›" } else { " " },
                            command.name
                        ),
                        Style::default().fg(if selected { ACCENT } else { MUTED }),
                    ),
                    muted(command.description),
                ])
            })
            .chain(std::iter::once(Line::from(muted(
                "    ↑↓ 选择 · tab 补全 · enter 执行 · esc 关闭",
            ))))
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    frame.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn tool_previews_bound_single_line_payloads_without_changing_results() {
        use super::super::app::ToolRow;
        let output = "中文".repeat(20000);
        let item = TranscriptItem::Tool(ToolRow {
            id: "write".into(),
            name: "write_file".into(),
            arguments: serde_json::json!({"path":"report.md", "content":output}).to_string(),
            status: ToolStatus::Finished,
            output: output.clone(),
        });
        let short = transcript_lines(std::slice::from_ref(&item), false);
        assert!(
            short[0]
                .to_string()
                .contains("report.md · 1 行 · 120000 字节")
        );
        assert!(short[1].to_string().chars().count() < 180);
        assert!(short[1].to_string().ends_with('…'));
        assert!(
            transcript_lines(&[item], true)[1]
                .to_string()
                .contains(&output)
        );
        assert!(
            tool_summary(&serde_json::json!({"command":"x".repeat(10000)}).to_string())
                .chars()
                .count()
                < 110
        );
    }

    fn render(state: &mut UiState, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, state)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    #[test]
    fn reasoning_is_visible_and_expansion_only_affects_one_visible_tool() {
        use super::super::app::ToolRow;
        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
        let mut state = UiState::new("model");
        state.apply(orbit_agent::events::AgentEvent::Model(
            orbit_model::types::StreamEvent::ReasoningDelta("reasoning visible now".into()),
        ));
        assert!(render(&mut state, 100, 30).contains("reasoning visible now"));
        for id in ["older", "latest"] {
            state.transcript.push(TranscriptItem::Tool(ToolRow {
                id: id.into(),
                name: "read_file".into(),
                arguments: "{}".into(),
                status: ToolStatus::Finished,
                output: format!("first\nsecond\n{id}-detail"),
            }));
        }
        let collapsed = render(&mut state, 100, 30);
        assert!(!collapsed.contains("latest-detail"));
        assert!(!collapsed.contains("ctrl+o"));
        super::super::keyboard_action(
            &mut state,
            Event::Key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)),
            false,
        );
        let expanded = render(&mut state, 100, 30);
        assert!(expanded.contains("latest-detail"));
        assert!(!expanded.contains("older-detail"));
        assert!(expanded.contains("reasoning visible now"));
        super::super::keyboard_action(
            &mut state,
            Event::Key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)),
            false,
        );
        assert!(!render(&mut state, 100, 30).contains("latest-detail"));
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let buffer = terminal.backend().buffer();
        let footer: String = (0..100).map(|x| buffer[(x, 29)].symbol()).collect();
        assert!(footer.contains("model") && footer.contains(state.permission_mode.label()));
        assert!(!footer.contains("Shift") && !footer.contains("恢复"));
    }

    #[test]
    fn model_change_is_centered_and_has_no_notice_prefix() {
        let item = TranscriptItem::ModelChanged("qwen::old → qwen::new".into());
        let lines = transcript_lines(&[item], false);
        assert_eq!(lines[0].to_string(), "qwen::old → qwen::new");
        assert_eq!(lines[0].alignment, Some(Alignment::Center));
        assert!(!lines[0].to_string().starts_with('›'));
    }

    #[test]
    fn welcome_commands_and_small_terminal_render() {
        let mut state = UiState::new("mock-model");
        let screen = render(&mut state, 100, 30);
        assert!(screen.contains("Orbit"));
        assert!(screen.contains("mock-model"));
        assert!(!screen.contains("开始一段新对话"));
        assert!(!screen.contains("/help"));
        state.input.insert("/");
        assert!(render(&mut state, 100, 30).contains("/new"));
        for (width, height) in [(20, 5), (10, 3), (1, 1)] {
            assert!(!render(&mut state, width, height).contains("请放大终端"));
        }
    }

    #[test]
    fn pending_images_render_as_chips_above_the_prompt() {
        let mut state = UiState::new("mock");
        state
            .attachments
            .push(orbit_model::types::Attachment::Image {
                name: "capture.png".into(),
                mime_type: "image/png".into(),
                path: "/tmp/unused.png".into(),
            });
        state.input.insert("describe this image");
        let screen = render(&mut state, 80, 24);
        assert!(screen.contains("1 · capture.png]"));
        assert!(screen.contains("describe this image"));
        assert!(state.input_area.y > 0);
        assert_eq!(state.selection.input, state.input_area);
        assert_eq!(state.attachment_areas.len(), 1);
        state.attachment_cursor = Some(0);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        assert!(!terminal.backend().cursor_visible());
        let chip = state.attachment_areas[0];
        assert_eq!(terminal.backend().buffer()[(chip.x, chip.y)].bg, ACCENT);
    }

    #[test]
    fn approval_stays_below_conversation_and_keeps_the_draft_visible() {
        let mut state = UiState::new("mock");
        state.transcript.push(TranscriptItem::Assistant(
            "Conversation remains visible".into(),
        ));
        state.input.insert("my draft");
        state.approval = Some(orbit_tools::approval::ApprovalPreview::Write(
            orbit_tools::approval::WritePreview {
                path: "price.c".into(),
                before: Some("return subtotal > 10000;\n".into()),
                after: "return subtotal >= 10000;\n".into(),
            },
        ));
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("Conversation remains visible"));
        assert!(text.contains("my draft"));
        assert!(text.contains("price.c"));
        assert!(text.contains("- return subtotal > 10000;"));
        assert!(text.contains("+ return subtotal >= 10000;"));
        assert!(text.replace(' ', "").contains("Enter确认"), "{text}");
        assert!(state.approval_shown);
    }

    #[test]
    fn composer_and_footer_can_be_selected_without_a_visible_caret() {
        use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let mut state = UiState::new("model-name");
        state.input.insert("你好 draft");
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        for footer in [false, true] {
            state.selection.clear();
            terminal
                .draw(|frame| {
                    draw(frame, &mut state);
                    state.selection.paint(frame);
                })
                .unwrap();
            let (x, y, end) = if footer {
                (0, 23, 79)
            } else {
                (
                    state.input_area.x,
                    state.input_area.y,
                    state.input_area.x + 3,
                )
            };
            for (kind, column) in [
                (MouseEventKind::Down(MouseButton::Left), x),
                (MouseEventKind::Drag(MouseButton::Left), end),
                (MouseEventKind::Up(MouseButton::Left), end),
            ] {
                state
                    .selection
                    .handle(&Event::Mouse(MouseEvent {
                        kind,
                        column,
                        row: y,
                        modifiers: KeyModifiers::NONE,
                    }))
                    .unwrap();
            }
            terminal
                .draw(|frame| assert!(state.selection.draw_frozen(frame)))
                .unwrap();
            assert!(
                terminal
                    .backend()
                    .buffer()
                    .content
                    .iter()
                    .all(|cell| cell.bg != Color::Gray)
            );
            if footer {
                assert!(state.selection.text().contains("model-name"));
            } else {
                assert_eq!(state.selection.text(), "你好");
            }
        }
    }

    #[test]
    fn selecting_hides_caret_without_moving_input_position() {
        let mut state = UiState::new("mock");
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        state.input.insert("draft");
        state.submit("hello".into());
        terminal
            .draw(|frame| {
                draw(frame, &mut state);
                state.selection.paint(frame);
            })
            .unwrap();
        assert!(!terminal.backend().cursor_visible());
        let caret = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .position(|c| c.bg == Color::Gray)
            .unwrap();
        let input_cursor = state.input.cursor;
        for kind in [
            crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            crossterm::event::MouseEventKind::Drag(crossterm::event::MouseButton::Left),
        ] {
            state
                .selection
                .handle(&crossterm::event::Event::Mouse(
                    crossterm::event::MouseEvent {
                        kind,
                        column: 4,
                        row: 3,
                        modifiers: crossterm::event::KeyModifiers::NONE,
                    },
                ))
                .unwrap();
        }
        state.apply(orbit_agent::events::AgentEvent::Model(
            orbit_model::types::StreamEvent::TextDelta("new output".into()),
        ));
        terminal
            .draw(|frame| {
                draw(frame, &mut state);
                state.selection.paint(frame);
            })
            .unwrap();
        assert_eq!(state.input.cursor, input_cursor);
        assert_ne!(terminal.backend().buffer().content[caret].bg, Color::Gray);
        state.apply(orbit_agent::events::AgentEvent::Completed {
            turns: 1,
            tool_calls: 0,
        });
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        assert!(!terminal.backend().cursor_visible());
        assert_ne!(terminal.backend().buffer().content[caret].bg, Color::Gray);
    }

    #[test]
    fn ime_anchor_tracks_unicode_input_and_stays_hidden_after_redraw() {
        use crate::tui::terminal::anchor_cursor;
        let mut state = UiState::new("model");
        state.workspace = "/tmp/中文项目".into();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        for (input, column) in [("", 4), ("你好", 8), ("你e\u{301}", 7), ("第一行\n你好", 8)]
        {
            state.input.set(input.into());
            terminal.draw(|frame| draw(frame, &mut state)).unwrap();
            anchor_cursor(&mut terminal, state.ime_cursor).unwrap();
            let position = state.ime_cursor.unwrap();
            assert_eq!(position.x, column);
            terminal.backend_mut().assert_cursor_position(position);
            assert!(!terminal.backend().cursor_visible());
            assert_eq!(
                terminal.backend().buffer()[(position.x, position.y)].bg,
                Color::Gray
            );
            // Streaming output must not leave the IME attached to the transcript.
            state.apply(orbit_agent::events::AgentEvent::Model(
                orbit_model::types::StreamEvent::TextDelta("输出中".into()),
            ));
            terminal.draw(|frame| draw(frame, &mut state)).unwrap();
            anchor_cursor(&mut terminal, state.ime_cursor).unwrap();
            terminal.backend_mut().assert_cursor_position(position);
        }
        state.picker = Some(crate::tui::dialogs::SessionPicker::new(Vec::new()));
        crate::tui::dialogs::handle(
            &mut state,
            crossterm::event::Event::Paste("中文".repeat(80)),
        );
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        anchor_cursor(&mut terminal, state.ime_cursor).unwrap();
        let position = state.ime_cursor.unwrap();
        terminal.backend_mut().assert_cursor_position(position);
        assert_eq!(
            terminal.backend().buffer()[(position.x, position.y)].symbol(),
            "▏"
        );
        assert!(!terminal.backend().cursor_visible());
    }

    #[test]
    fn deleting_cjk_repaints_the_trailing_half_after_the_caret() {
        let mut state = UiState::new("mock");
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        state.input.insert("你好世界中文");
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        while !state.input.text.is_empty() {
            let previous = terminal.backend().buffer().clone();
            state.input.backspace();
            terminal.draw(|frame| draw(frame, &mut state)).unwrap();
            let current = terminal.backend().buffer();
            let caret = state.ime_cursor.unwrap();
            let updates = previous.diff(current);
            let caret_update = updates
                .iter()
                .position(|(x, y, _)| (*x, *y) == (caret.x, caret.y))
                .unwrap();
            let clear_update = updates
                .iter()
                .position(|(x, y, cell)| {
                    (*x, *y) == (caret.x + 1, caret.y)
                        && cell.symbol() == " "
                        && cell.bg == Color::Reset
                })
                .expect(
                    "the terminal needs an explicit reset of the erased wide glyph's trailing half",
                );
            assert!(clear_update > caret_update);
            assert_eq!(
                current
                    .content
                    .iter()
                    .filter(|cell| cell.bg == Color::Gray)
                    .count(),
                1
            );
        }
    }

    #[test]
    fn footer_keeps_project_model_and_mode_with_unicode_paths() {
        let mut state = UiState::new("deepseek");
        state.workspace = format!("/tmp/{}/项目", "长目录/".repeat(40));
        for width in [24, 40, 80] {
            let line = footer(&state, width);
            assert!(line.width() <= width);
            let text = line.to_string();
            if width >= 40 {
                assert!(text.contains("项目"));
            }
            if width >= 80 {
                assert!(text.contains("思考"));
            }
            assert!(text.ends_with("  "));
        }
        assert_eq!(fit_label("你好e\u{301}", 3, true), "…e\u{301}");
    }

    #[test]
    fn output_control_sequences_are_visible_text_not_terminal_commands() {
        assert_eq!(
            safe_output("a\x1b]52;c;secret\x07"),
            "a\\u{1b}]52;c;secret\\u{7}"
        );
    }

    #[test]
    fn scrolling_during_streaming_keeps_the_visible_anchor() {
        use orbit_agent::events::AgentEvent;
        use orbit_model::types::StreamEvent;
        let mut state = UiState::new("mock");
        state.apply(AgentEvent::Model(StreamEvent::TextDelta(
            (0..200).map(|i| format!("line {i:03}\n")).collect(),
        )));
        render(&mut state, 80, 24);
        state.scroll_up(30);
        let before = render(&mut state, 80, 24);
        let offset = state.max_scroll - state.scroll;
        state.apply(AgentEvent::Model(StreamEvent::TextDelta(
            "more output\n".repeat(20),
        )));
        assert_eq!(before, render(&mut state, 80, 24));
        assert_eq!(state.max_scroll - state.scroll, offset);
        state.scroll_down(3);
        render(&mut state, 80, 24);
        assert_eq!(state.max_scroll - state.scroll, offset + 3);
        state.scroll_down(usize::MAX);
        assert!(state.follow_bottom);
        assert!(render(&mut state, 80, 24).contains("more output"));
    }

    #[test]
    fn follows_wrapped_answer_to_last_line() {
        let mut state = UiState::new("mock");
        state.transcript.push(TranscriptItem::Assistant(format!(
            "{}\nTHE END",
            "中文长回答".repeat(200)
        )));
        let screen = render(&mut state, 40, 16);
        assert!(
            screen.contains("THE END"),
            "scroll {} screen {screen:?}",
            state.max_scroll
        );
        assert!(state.max_scroll > 10);
        state.scroll_up(8);
        assert!(!render(&mut state, 40, 16).contains("THE END"));
    }

    #[test]
    fn resize_preserves_the_current_source_anchor() {
        let mut state = UiState::new("mock");
        state.transcript.push(TranscriptItem::Assistant(
            (0..80)
                .map(|index| format!("段落 {index:02} {}\n", "中文内容 words ".repeat(8)))
                .collect(),
        ));
        render(&mut state, 90, 18);
        state.scroll_up(70);
        render(&mut state, 90, 18);
        let old_offset = state.max_scroll - state.scroll;
        let anchor = state.transcript_layout.anchor_at(old_offset).unwrap();

        render(&mut state, 48, 18);
        let expected = state
            .transcript_layout
            .offset_for_anchor(anchor)
            .unwrap()
            .min(state.max_scroll);
        assert_eq!(state.max_scroll - state.scroll, expected);
        assert!(!state.follow_bottom);
    }
}
