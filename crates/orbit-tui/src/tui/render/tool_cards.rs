//! Tool titles, result bodies and their shared frame.
use std::ops::Range;

use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};

use super::text::{compact_preview, parse_ansi_to_spans, safe_output, sanitize_linked_line};
use super::{TOOL_FAILED, TOOL_FINISHED, TOOL_RUNNING};
use crate::tui::{
    app::{ToolRow, ToolStatus},
    code, icons,
    markdown::{self, LinkedLine},
    theme::THEME,
    tool_display,
};

pub(in crate::tui) fn read_file_summary(arguments: &str, output: &str) -> String {
    let args = serde_json::from_str::<serde_json::Value>(arguments).ok();
    let path = args
        .as_ref()
        .and_then(|v| v["path"].as_str())
        .unwrap_or_default();
    let out = serde_json::from_str::<serde_json::Value>(output).ok();

    let start = out
        .as_ref()
        .and_then(|v| v["start_line"].as_u64())
        .or_else(|| args.as_ref().and_then(|v| v["start_line"].as_u64()));
    let end = out
        .as_ref()
        .and_then(|v| v["end_line"].as_u64())
        .or_else(|| {
            let max = args.as_ref().and_then(|v| v["max_lines"].as_u64())?;
            Some(start.unwrap_or(1).saturating_add(max).saturating_sub(1))
        });

    match (start, end) {
        (Some(s), Some(e)) => format!("{path}[{s}..={e}]"),
        (Some(s), None) => format!("{path}[{s}..]"),
        _ => path.to_owned(),
    }
}

fn tool_details(row: &ToolRow, expanded: bool, width: u16) -> Vec<LinkedLine> {
    let mut lines = Vec::new();
    let (marker, color) = match row.status {
        ToolStatus::Running => (icons::LOADING, TOOL_RUNNING),
        ToolStatus::Finished => (icons::CHECK, TOOL_FINISHED),
        ToolStatus::Failed => (icons::CLOSE, TOOL_FAILED),
    };

    let summary = tool_display::tool_summary(&row.name, &row.arguments, &row.output);

    lines.push(Line::from(vec![
        Span::styled(format!("  {marker} "), Style::default().fg(color)),
        Span::styled(
            tool_display::clipped(
                &safe_output(&format!("{} {summary}", row.name)),
                usize::from(width).saturating_sub(4),
            ),
            Style::default().fg(THEME.output_foreground),
        ),
    ]));

    if let Some(output) = tool_display::linked_lines(row, expanded, width) {
        let mut lines: Vec<_> = lines.into_iter().map(LinkedLine::plain).collect();
        lines.extend(output);
        lines.push(LinkedLine::plain(Line::from("")));
        return lines;
    }

    let output = serde_json::from_str::<serde_json::Value>(&row.output)
        .ok()
        .and_then(|value| {
            if value.get("stdout").is_some() {
                let is_running = value
                    .get("running")
                    .and_then(|r| r.as_bool())
                    .unwrap_or(false);
                let verification_error = value["verification_error"].as_str();
                let verification_detail = verification_error
                    .map(|error| format!("\n验证：{error}"))
                    .unwrap_or_default();

                let stdout = value["stdout"].as_str().unwrap_or_default();
                let stderr = value["stderr"].as_str().unwrap_or_default();

                if is_running {
                    Some(format!("运行中…\n{stdout}{stderr}{verification_detail}"))
                } else {
                    let mut text = String::new();
                    if !stdout.is_empty() {
                        let trimmed = stdout.trim();
                        if (trimmed.starts_with('{') || trimmed.starts_with('['))
                            && let Ok(json_val) = serde_json::from_str::<serde_json::Value>(trimmed)
                            && let Ok(pretty) = serde_json::to_string_pretty(&json_val)
                        {
                            text.push_str(&pretty);
                        } else {
                            text.push_str(stdout);
                        }
                    }

                    if !stderr.is_empty() {
                        if !text.is_empty() && !text.ends_with('\n') {
                            text.push('\n');
                        }
                        text.push_str(stderr);
                    }

                    if !verification_detail.is_empty() {
                        if !text.is_empty() && !text.ends_with('\n') {
                            text.push('\n');
                        }
                        text.push_str(&verification_detail);
                    }

                    if let Some(error) = value["retained_output_error"].as_str() {
                        if !text.is_empty() && !text.ends_with('\n') {
                            text.push('\n');
                        }
                        text.push_str(&format!(
                            "[输出日志不完整：{error} · 命令结果与尾部输出仍可用]"
                        ));
                    }

                    if text.is_empty()
                        && let Some(code) = value["exit_code"].as_i64().filter(|c| *c != 0)
                    {
                        text = format!("(exit {code})");
                    }

                    Some(text)
                }
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

    let is_json_output = (output.trim_start().starts_with('{')
        || output.trim_start().starts_with('['))
        && serde_json::from_str::<serde_json::Value>(&output).is_ok();
    if is_json_output {
        let mut renderer = code::Renderer::new("json");
        for line in output.lines() {
            lines.extend(renderer.render(line, code::Options::default()));
        }
    } else {
        for line in output.lines() {
            lines.push(Line::from(parse_ansi_to_spans(line)));
        }
    }

    lines.push(tool_display::toggle_line(true, width));

    lines.push(Line::from(""));
    lines.into_iter().map(LinkedLine::plain).collect()
}

pub(in crate::tui) fn tool_card(row: &ToolRow, expanded: bool, width: u16) -> Vec<Line<'static>> {
    tool_card_with_links(row, expanded, width)
        .into_iter()
        .map(|line| line.line)
        .collect()
}

pub(in crate::tui) fn tool_card_with_links(
    row: &ToolRow,
    expanded: bool,
    width: u16,
) -> Vec<LinkedLine> {
    tool_card_layout(row, expanded, width).lines
}

pub(in crate::tui) struct ToolCard {
    pub lines: Vec<LinkedLine>,
    pub header: Range<usize>,
    pub body: Range<usize>,
}

pub(in crate::tui) fn tool_card_layout(row: &ToolRow, expanded: bool, width: u16) -> ToolCard {
    let width = if width == u16::MAX { 120 } else { width };

    if !expanded || width < 10 {
        let header = tool_title_with_links(row, false, width.saturating_sub(4));
        let lines: Vec<_> = if width < 10 {
            tool_title_with_links(row, false, width)
        } else {
            header
                .into_iter()
                .map(|line| line.framed(vec![Span::raw("  ")], vec![]))
                .collect()
        };
        return ToolCard {
            header: 0..lines.len(),
            body: 0..0,
            lines,
        };
    }

    let inner_width = width - 4;
    let mut lines = vec![tool_border_line(width, ('╭', '╮'))];
    let title = tool_title_with_links(row, true, inner_width);
    let header = lines.len()..lines.len() + title.len();
    lines.extend(title.into_iter().map(|line| tool_frame_line(line, width)));
    lines.push(tool_frame_line(LinkedLine::chrome(Line::default()), width));
    let start = lines.len();
    lines.extend(
        tool_body_with_links(row, inner_width)
            .into_iter()
            .map(|line| tool_frame_line(line, width)),
    );
    let body = start..lines.len();
    lines.push(tool_border_line(width, ('╰', '╯')));
    ToolCard {
        lines,
        header,
        body,
    }
}

pub(in crate::tui) fn tool_title_with_links(
    row: &ToolRow,
    expanded: bool,
    width: u16,
) -> Vec<LinkedLine> {
    tool_title_with_summary(row, expanded, width, None)
}

pub(in crate::tui) fn tool_group_title_with_links(row: &ToolRow, width: u16) -> Vec<LinkedLine> {
    tool_title_with_summary(
        row,
        false,
        width,
        Some(tool_display::group_call_summary(row)),
    )
}

fn tool_title_with_summary(
    row: &ToolRow,
    expanded: bool,
    width: u16,
    summary: Option<String>,
) -> Vec<LinkedLine> {
    let color = match row.status {
        ToolStatus::Running => TOOL_RUNNING,
        ToolStatus::Finished => TOOL_FINISHED,
        ToolStatus::Failed => TOOL_FAILED,
    };

    if width < 6 {
        return vec![LinkedLine::plain(Line::styled(
            tool_display::clipped(&row.name, width as usize),
            Style::default().fg(color),
        ))];
    }

    let arguments = serde_json::from_str::<serde_json::Value>(&row.arguments).ok();
    let command = arguments
        .as_ref()
        .and_then(|value| {
            ["command", "query", "url", "path"]
                .iter()
                .find_map(|key| value[*key].as_str())
        })
        .unwrap_or("");
    let command = if let Some(summary) = summary {
        summary
    } else if !expanded {
        tool_display::tool_summary(&row.name, &row.arguments, &row.output)
    } else {
        command.to_owned()
    };

    let title_spans = if command.is_empty() {
        vec![Span::styled(
            safe_output(&row.name),
            Style::default().fg(color),
        )]
    } else {
        vec![
            Span::styled(safe_output(&row.name), Style::default().fg(color)),
            Span::raw(" "),
            Span::styled(
                safe_output(&command),
                Style::default().fg(THEME.tool_summary),
            ),
        ]
    };
    let title = |line| {
        let mut linked = LinkedLine::plain(line);
        if row.name == "web_fetch" {
            linked.links.clear();
        }
        linked
    };

    let mut header = crate::tui::subagents::header(row, color, width)
        .map(|line| vec![line])
        .unwrap_or_else(|| title(Line::from(title_spans)).wrap(width));
    if !expanded {
        header.truncate(1);
    }
    header
}

pub(in crate::tui) fn tool_body_with_links(row: &ToolRow, width: u16) -> Vec<LinkedLine> {
    let mut details = crate::tui::subagents::details(row, width).unwrap_or_else(|| {
        let mut details = tool_details(row, true, width);
        if !details.is_empty() {
            details.remove(0);
        }
        details
    });
    let toggle = tool_display::toggle_line(true, width);
    let collapse = tool_display::toggle_line(false, width);
    details.retain(|linked| linked.line != toggle && linked.line != collapse);
    while details
        .last()
        .is_some_and(|linked| linked.line.to_string().trim().is_empty())
    {
        details.pop();
    }

    let mut lines = Vec::new();
    for mut linked in details {
        if linked.line.to_string().starts_with("    ") {
            let mut remaining = 4;
            for span in &mut linked.line.spans {
                let count = span
                    .content
                    .chars()
                    .take_while(|c| *c == ' ')
                    .count()
                    .min(remaining);

                span.content = span.content[count..].to_owned().into();
                remaining -= count;
                if remaining == 0 {
                    break;
                }
            }

            linked.shift_offsets(-4);
        }

        sanitize_linked_line(&mut linked);
        let line = &mut linked.line;
        for span in &mut line.spans {
            span.style = line.style.patch(span.style);
            if span.style.fg == Some(Color::Reset) || span.style.fg.is_none() {
                span.style = span.style.fg(THEME.output_muted);
            }
        }

        line.spans.retain(|span| !span.content.is_empty());
        if markdown::expand_rule(line, width) {
            linked.decorations = std::iter::once(0..line.to_string().len()).collect();
            linked.chrome = true;
        }

        lines.extend(linked.wrap(width));
    }

    lines
}

pub(in crate::tui) fn tool_border_line(width: u16, ends: (char, char)) -> LinkedLine {
    LinkedLine::chrome(Line::from(Span::styled(
        format!(
            "{}{}{}",
            ends.0,
            "─".repeat(width.saturating_sub(2) as usize),
            ends.1
        ),
        Style::default().fg(THEME.tool_border),
    )))
}

pub(in crate::tui) fn tool_frame_line(linked: LinkedLine, width: u16) -> LinkedLine {
    let border_style = Style::default().fg(THEME.tool_border);
    let pad = usize::from(width.saturating_sub(4)).saturating_sub(linked.line.width());
    let line_bg = linked
        .line
        .spans
        .first()
        .and_then(|s| s.style.bg)
        .filter(|bg| linked.line.spans.iter().all(|s| s.style.bg == Some(*bg)));
    let (leading, trailing) = if let Some(bg) = line_bg {
        (
            vec![
                Span::styled("│", border_style),
                Span::styled(" ", Style::default().bg(bg)),
            ],
            vec![
                Span::styled(" ".repeat(pad + 1), Style::default().bg(bg)),
                Span::styled("│", border_style),
            ],
        )
    } else {
        (
            vec![Span::styled("│ ", border_style)],
            vec![Span::raw(" ".repeat(pad)), Span::styled(" │", border_style)],
        )
    };
    linked.framed(leading, trailing)
}

#[cfg(test)]
pub(in crate::tui) use default_tool_summary as tool_summary;

pub(in crate::tui) fn default_tool_summary(arguments: &str) -> String {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(arguments) else {
        return String::new();
    };
    if let Some(command) = value["command"].as_str() {
        let lines: Vec<_> = command
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();
        let target = lines
            .iter()
            .copied()
            .find(|line| !is_shell_preamble(line))
            .or_else(|| lines.first().copied())
            .unwrap_or_default();
        let mut summary = compact_preview(target, 160);

        if lines.len() > 1 {
            summary.push_str(" …");
        }

        return summary;
    }

    let target = value["path"]
        .as_str()
        .or_else(|| value["query"].as_str())
        .or_else(|| value["url"].as_str())
        .unwrap_or_default();
    let mut summary = compact_preview(target.lines().next().unwrap_or_default(), 120);

    if let Some(content) = value["content"].as_str() {
        summary.push_str(&format!(
            " · {} 行 · {} 字节",
            content.lines().count(),
            content.len()
        ));
    }

    summary
}

pub(in crate::tui) fn is_shell_preamble(line: &str) -> bool {
    let compact = line.split_whitespace().collect::<String>();
    compact.starts_with("set-")
        && compact[4..]
            .chars()
            .all(|ch| ch.is_ascii_alphabetic() || ch == 'o')
}
