//! Width-aware transcript and bounded overlay rendering.

use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, Paragraph},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::app::{App, CachedEntry, Entry, EntryKind};
use super::commands;
use super::markdown;
use orbit_tools::approval::ApprovalPreview;

const ACCENT: Color = Color::Rgb(218, 139, 109);
const DIM: Color = Color::Rgb(155, 153, 157);
const ERROR: Color = Color::Rgb(235, 119, 136);

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    let area = frame.area();
    if area.width < 24 || area.height < 8 {
        frame.render_widget(Paragraph::new("Orbit: enlarge the terminal"), area);
        return;
    }

    let input_height = input_height(app, area.width).min(6);
    let regions = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(input_height),
        Constraint::Length(1),
    ])
    .split(area);
    draw_header(frame, app, regions[0]);
    draw_transcript(frame, app, regions[1]);
    draw_input(frame, app, regions[2]);
    draw_footer(frame, app, regions[3]);

    if !app.running && !app.restoring && app.approval.is_none() {
        draw_command_palette(frame, app, regions[2], area);
    }
    if app.picker.is_some() {
        draw_picker(frame, app, area);
    }
    if app.model_picker.is_some() {
        draw_model_picker(frame, app, area);
    }
    if app.approval.is_some() {
        draw_approval(frame, app, area);
    }
}

fn draw_header(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let title = Line::from(vec![
        Span::styled(
            " Orbit ",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            short_path(&app.workspace, area.width as usize / 2),
            Style::default().fg(DIM),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(title),
        Rect::new(area.x, area.y, area.width, 1),
    );
    frame.render_widget(
        Paragraph::new(Line::from("─".repeat(area.width as usize))).style(Style::default().fg(DIM)),
        Rect::new(area.x, area.y + 1, area.width, 1),
    );
}

fn draw_transcript(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let content_width = area.width.saturating_sub(4) as usize;
    app.render_cache.resize_with(app.entries.len(), || None);
    for (index, entry) in app.entries.iter().enumerate() {
        let refresh = app.render_cache[index].as_ref().is_none_or(|cached| {
            cached.width != content_width || cached.revision != entry.revision
        });
        if refresh {
            app.render_cache[index] = Some(CachedEntry {
                width: content_width,
                revision: entry.revision,
                lines: entry_lines(entry, content_width, app.selected_tool == Some(index)),
            });
        }
    }

    let total_lines = app
        .render_cache
        .iter()
        .flatten()
        .map(|cached| cached.lines.len())
        .sum::<usize>();
    let max_scroll = total_lines.saturating_sub(area.height as usize);
    if app.follow_bottom {
        app.scroll = max_scroll;
    } else {
        app.scroll = app.scroll.min(max_scroll);
    }

    let mut lines = Vec::new();
    let mut skipped = app.scroll;
    for cached in app.render_cache.iter().flatten() {
        if skipped >= cached.lines.len() {
            skipped -= cached.lines.len();
            continue;
        }
        for line in cached.lines.iter().skip(skipped) {
            lines.push(line.clone());
            if lines.len() == area.height as usize {
                break;
            }
        }
        if lines.len() == area.height as usize {
            break;
        }
        skipped = 0;
    }
    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            "  Ask Orbit about this project.",
            Style::default().fg(DIM),
        )));
    }

    frame.render_widget(Paragraph::new(Text::from(lines)), area);
}

fn entry_lines(entry: &Entry, width: usize, selected: bool) -> Vec<Line<'static>> {
    if matches!(entry.kind, EntryKind::Assistant | EntryKind::Thinking) {
        let mut lines = markdown::render(
            &entry.text,
            width.saturating_sub(2),
            entry.kind == EntryKind::Thinking,
        )
        .into_iter()
        .map(|mut line| {
            line.spans.insert(0, Span::raw("  "));
            line
        })
        .collect::<Vec<_>>();
        lines.push(Line::default());
        return lines;
    }

    let (prefix, color) = match entry.kind {
        EntryKind::User => ("> ", ACCENT),
        EntryKind::Assistant => ("  ", Color::Reset),
        EntryKind::Thinking => ("  ", DIM),
        EntryKind::Tool => ("  ", if entry.failed { ERROR } else { ACCENT }),
        EntryKind::Notice => ("  ", DIM),
        EntryKind::Error => ("! ", ERROR),
    };
    let body = if entry.kind == EntryKind::Tool {
        let marker = if !entry.finished {
            "running"
        } else if entry.failed {
            "failed"
        } else {
            "done"
        };
        format!("{}  [{marker}]", entry.text)
    } else {
        entry.text.clone()
    };
    let mut lines = Vec::new();
    for (row, wrapped) in wrap(&body, width.saturating_sub(2)).into_iter().enumerate() {
        let label = if row == 0 { prefix } else { "  " };
        let style = Style::default().fg(color);
        let style = if entry.kind == EntryKind::User {
            style.add_modifier(Modifier::BOLD)
        } else {
            style
        };
        lines.push(Line::from(vec![
            Span::styled(label, Style::default().fg(color)),
            Span::styled(wrapped, style),
        ]));
    }
    if entry.kind == EntryKind::Tool && !entry.output.is_empty() {
        let preview = if entry.expanded {
            entry.output.as_str()
        } else {
            entry.preview.as_str()
        };
        let extra = entry
            .output
            .lines()
            .count()
            .saturating_sub(entry.preview.lines().count());
        for wrapped in wrap(preview, width.saturating_sub(4)) {
            lines.push(Line::from(Span::styled(
                format!("    {wrapped}"),
                Style::default().fg(if entry.failed { ERROR } else { DIM }),
            )));
        }
        if !entry.expanded && extra > 0 {
            lines.push(Line::from(Span::styled(
                format!(
                    "    {extra} more lines  {}",
                    if selected {
                        "Enter to expand"
                    } else {
                        "Tab to select"
                    }
                ),
                Style::default().fg(DIM),
            )));
        } else if selected {
            lines.push(Line::from(Span::styled(
                if entry.expanded {
                    "    Enter to collapse"
                } else {
                    "    Enter to expand"
                },
                Style::default().fg(DIM),
            )));
        }
    }
    lines.push(Line::default());
    lines
}

fn draw_input(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::TOP | Borders::BOTTOM)
        .border_style(Style::default().fg(ACCENT));
    frame.render_widget(block, area);
    if area.height < 3 {
        return;
    }
    let width = area.width.saturating_sub(4) as usize;
    let (visual, cursor) = visual_input(app.input.text(), app.input.cursor(), width);
    let capacity = area.height.saturating_sub(2) as usize;
    let hidden = visual.len().saturating_sub(capacity);
    let lines = visual
        .into_iter()
        .skip(hidden)
        .take(capacity)
        .enumerate()
        .map(|(index, text)| {
            let prefix = if index == 0 && hidden == 0 {
                "> "
            } else {
                "  "
            };
            Line::from(vec![
                Span::styled(prefix, Style::default().fg(ACCENT)),
                Span::raw(text),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(Text::from(lines)),
        Rect::new(
            area.x + 1,
            area.y + 1,
            area.width.saturating_sub(2),
            area.height - 2,
        ),
    );
    if app.picker.is_none() && app.approval.is_none() {
        let cursor_row = cursor
            .1
            .saturating_sub(hidden)
            .min(capacity.saturating_sub(1));
        frame.set_cursor_position((
            area.x + 3 + cursor.0.min(width) as u16,
            area.y + 1 + cursor_row as u16,
        ));
    }
}

fn draw_footer(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let session = app.session_id.chars().take(8).collect::<String>();
    let model = app.reasoning.map_or_else(
        || app.model.clone(),
        |effort| format!("{} {}", app.model, effort.as_str()),
    );
    let context_percent = app.context_used_tokens.saturating_mul(100) / app.context_tokens.max(1);
    let text = format!(
        " {}  |  {}  |  {}  |  {}% ({}/{}) tokens  |  session {}  |  Ctrl+C exit  PageUp/Down scroll",
        app.status,
        app.approval_mode.label(),
        model,
        context_percent,
        app.context_used_tokens,
        app.context_tokens,
        session
    );
    frame.render_widget(Paragraph::new(text).style(Style::default().fg(DIM)), area);
}

fn draw_command_palette(frame: &mut Frame<'_>, app: &App, input: Rect, area: Rect) {
    if app.picker.is_some() || app.model_picker.is_some() {
        return;
    }
    let matches = commands::matching(app.input.text());
    if matches.is_empty() {
        return;
    }

    let width = area.width.saturating_sub(4).min(96);
    let height = (matches.len() as u16 + 2)
        .min(area.height / 2)
        .min(input.y.saturating_sub(area.y));
    if width < 12 || height < 3 {
        return;
    }
    let popup = Rect::new(area.x + 2, input.y - height, width, height);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .title(" Commands ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let rows = inner.height as usize;
    let selected = app.command_selection.min(matches.len() - 1);
    let start = selected
        .saturating_sub(rows / 2)
        .min(matches.len().saturating_sub(rows));
    let lines = matches
        .iter()
        .enumerate()
        .skip(start)
        .take(rows)
        .map(|(index, command)| {
            let available = inner.width.saturating_sub(2) as usize;
            let name = format!("{:<14}", command.name);
            let description = if available > 14 {
                truncate_width(command.description, available - 14)
            } else {
                String::new()
            };
            let label = if index == selected { "> " } else { "  " };
            let style = if index == selected {
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Line::from(vec![
                Span::styled(label, style),
                Span::styled(truncate_width(&name, available), style),
                Span::styled(description, style),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), inner);
}

fn truncate_width(text: &str, width: usize) -> String {
    let mut output = String::new();
    let mut used = 0;
    for grapheme in text.graphemes(true) {
        let size = grapheme.width();
        if used + size > width {
            break;
        }
        output.push_str(grapheme);
        used += size;
    }
    output
}

fn draw_picker(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let popup = centered(area, 78, 66);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .title(" Resume session ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let Some(picker) = &app.picker else { return };
    let matches = picker.matching();
    let rows = inner.height.saturating_sub(2) as usize;
    let selected = picker.selected.min(matches.len().saturating_sub(1));
    let start = selected
        .saturating_sub(rows / 2)
        .min(matches.len().saturating_sub(rows));
    let mut lines = vec![Line::from(format!("Search: {}", picker.query))];
    for (index, session) in matches.iter().enumerate().skip(start).take(rows) {
        let title = if session.title.is_empty() {
            "(untitled)"
        } else {
            &session.title
        };
        let text = format!(
            "{} {}{}  {}",
            if index == selected { ">" } else { " " },
            session.id.chars().take(8).collect::<String>(),
            if session.id == app.session_id {
                " (current)"
            } else {
                ""
            },
            title
        );
        let style = if index == selected {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::styled(
            truncate_width(&text, inner.width as usize),
            style,
        ));
    }
    if matches.is_empty() {
        lines.push(Line::from("No matching sessions"));
    }
    lines.push(Line::from(Span::styled(
        "↑↓ select  Enter resume  Esc close",
        Style::default().fg(DIM),
    )));
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_model_picker(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let popup = centered(area, 70, 62);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .title(" Select model ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let Some(picker) = &app.model_picker else {
        return;
    };
    let rows = inner.height.saturating_sub(1) as usize;
    let start = picker.selected.saturating_sub(rows.saturating_sub(1));
    let mut lines = Vec::new();
    for (index, name) in picker.models.iter().enumerate().skip(start).take(rows) {
        let style = if index == picker.selected {
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        lines.push(Line::styled(
            format!(
                "{} {name}",
                if index == picker.selected { ">" } else { " " }
            ),
            style,
        ));
    }
    lines.push(Line::from(Span::styled(
        "↑↓ select  Enter use model  Esc close",
        Style::default().fg(DIM),
    )));
    frame.render_widget(Paragraph::new(lines), inner);
}

fn draw_approval(frame: &mut Frame<'_>, app: &mut App, area: Rect) {
    let popup = centered(area, 94, 84);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .title(" Approval required ")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let Some(request) = &app.approval else { return };
    match &request.preview {
        ApprovalPreview::Write(write) => {
            let regions = Layout::vertical([
                Constraint::Length(2),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(inner);
            frame.render_widget(
                Paragraph::new(truncate_width(
                    &format!("File change: {}", write.path),
                    regions[0].width as usize,
                )),
                regions[0],
            );
            let panes =
                Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
                    .split(regions[1]);
            let before = Block::default()
                .title(if write.before.is_some() {
                    " Before "
                } else {
                    " New file "
                })
                .borders(Borders::ALL)
                .border_style(Style::default().fg(DIM));
            let after = Block::default()
                .title(" After ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(DIM));
            let left = before.inner(panes[0]);
            let right = after.inner(panes[1]);
            frame.render_widget(before, panes[0]);
            frame.render_widget(after, panes[1]);

            let widths = (left.width, right.width);
            if app
                .approval_diff
                .as_ref()
                .is_none_or(|cache| cache.widths != widths)
            {
                app.approval_diff = Some(super::approval_diff::ApprovalDiff::new(
                    write.before.as_deref().unwrap_or(""),
                    &write.after,
                    widths,
                    app.approval_full,
                ));
            }
            let cache = app.approval_diff.as_ref().expect("approval diff is cached");
            let height = left.height.min(right.height) as usize;
            let max_scroll = cache.left.len().saturating_sub(height);
            app.approval_scroll = app.approval_scroll.min(max_scroll);
            frame.render_widget(
                Paragraph::new(
                    cache
                        .left
                        .iter()
                        .skip(app.approval_scroll)
                        .take(height)
                        .cloned()
                        .collect::<Vec<_>>(),
                ),
                left,
            );
            frame.render_widget(
                Paragraph::new(
                    cache
                        .right
                        .iter()
                        .skip(app.approval_scroll)
                        .take(height)
                        .cloned()
                        .collect::<Vec<_>>(),
                ),
                right,
            );
            frame.render_widget(
                Paragraph::new(format!(
                    "Y approve  N deny  Esc deny  ↑↓ scroll  Tab {}",
                    if app.approval_full {
                        "fold unchanged"
                    } else {
                        "show all"
                    }
                ))
                .style(Style::default().fg(ACCENT)),
                regions[2],
            );
        }
        ApprovalPreview::Command(command) => {
            let preview = format!(
                "Disposable workspace command  |  timeout {}s\n\n{}",
                command.timeout_seconds, command.command
            );
            let height = inner.height.saturating_sub(2);
            let mut lines = preview
                .lines()
                .flat_map(|line| wrap(line, inner.width.saturating_sub(1) as usize))
                .skip(app.approval_scroll)
                .take(height as usize)
                .map(Line::from)
                .collect::<Vec<_>>();
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                "Y approve  N deny  Esc deny  ↑↓ scroll",
                Style::default().fg(ACCENT),
            )));
            frame.render_widget(Paragraph::new(lines), inner);
        }
    }
}

fn input_height(app: &App, width: u16) -> u16 {
    let width = width.saturating_sub(4) as usize;
    let (lines, _) = visual_input(app.input.text(), app.input.cursor(), width);
    lines.len().clamp(1, 4) as u16 + 2
}

fn visual_input(text: &str, cursor: usize, width: usize) -> (Vec<String>, (usize, usize)) {
    let width = width.max(1);
    let mut lines = vec![String::new()];
    let mut column = 0;
    let mut caret = (0, 0);
    for (offset, grapheme) in text.grapheme_indices(true) {
        if offset == cursor {
            caret = (column, lines.len() - 1);
        }
        if grapheme == "\n" {
            lines.push(String::new());
            column = 0;
            continue;
        }
        let size = grapheme.width().max(1);
        if column + size > width && column > 0 {
            lines.push(String::new());
            column = 0;
        }
        lines.last_mut().unwrap().push_str(grapheme);
        column += size;
    }
    if cursor == text.len() {
        caret = (column, lines.len() - 1);
    }
    (lines, caret)
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    for source in text.split('\n') {
        let mut line = String::new();
        let mut used = 0;
        for grapheme in source.graphemes(true) {
            let size = grapheme.width().max(1);
            if used + size > width && !line.is_empty() {
                lines.push(std::mem::take(&mut line));
                used = 0;
            }
            line.push_str(grapheme);
            used += size;
        }
        lines.push(line);
    }
    lines
}

fn short_path(path: &str, width: usize) -> String {
    if path.width() <= width {
        path.to_owned()
    } else {
        let mut suffix = Vec::new();
        let mut used = 1;
        for grapheme in path.graphemes(true).rev() {
            let size = grapheme.width();
            if used + size > width {
                break;
            }
            suffix.push(grapheme);
            used += size;
        }
        suffix.reverse();
        format!("…{}", suffix.concat())
    }
}

fn centered(area: Rect, width_percent: u16, height_percent: u16) -> Rect {
    let width = area
        .width
        .saturating_mul(width_percent)
        .saturating_div(100)
        .max(24)
        .min(area.width);
    let height = area
        .height
        .saturating_mul(height_percent)
        .saturating_div(100)
        .max(8)
        .min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_agent::events::AgentEvent;
    use orbit_model::types::StreamEvent;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn render_handles_long_unicode_and_narrow_terminal() {
        let mut terminal = Terminal::new(TestBackend::new(44, 16)).unwrap();
        let mut app = App::new(
            "123456789".into(),
            "/very/long/project/path".into(),
            "model".into(),
            128_000,
        );
        app.submit(&"中文👩‍💻".repeat(120), &[]);
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        assert!(app.scroll > 0);
    }

    #[test]
    fn wraps_without_splitting_graphemes() {
        assert_eq!(wrap("甲👩‍💻乙", 4), vec!["甲👩‍💻", "乙"]);
    }

    #[test]
    fn streaming_rewraps_only_the_changed_entry() {
        let mut terminal = Terminal::new(TestBackend::new(60, 16)).unwrap();
        let mut app = App::new(
            "session".into(),
            "workspace".into(),
            "model".into(),
            128_000,
        );
        app.notice("stable history");
        app.submit("prompt", &[]);
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();
        let stable_lines = app.render_cache[0].as_ref().unwrap().lines.as_ptr();

        app.apply(AgentEvent::Model(StreamEvent::TextDelta("response".into())));
        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        assert_eq!(
            app.render_cache[0].as_ref().unwrap().lines.as_ptr(),
            stable_lines
        );
        assert_eq!(app.render_cache[2].as_ref().unwrap().revision, 1);
    }

    #[test]
    fn command_palette_scrolls_to_selection_in_a_narrow_terminal() {
        let mut terminal = Terminal::new(TestBackend::new(44, 14)).unwrap();
        let mut app = App::new(
            "session".into(),
            "workspace".into(),
            "model".into(),
            128_000,
        );
        app.input.insert("/");
        app.command_selection = 12;

        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let screen = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(screen.contains("/help"), "{screen}");
        assert!(!screen.contains("/new"), "{screen}");
    }

    #[test]
    fn long_tool_lines_remain_within_the_transcript_width() {
        let mut entry = Entry::new(EntryKind::Tool, "read_file src/long.rs".repeat(4));
        entry.finished = true;
        entry.output = "a".repeat(180);
        entry.preview = entry.output.clone();
        entry.expanded = true;

        let lines = entry_lines(&entry, 32, true);

        assert!(lines.iter().all(|line| line.width() <= 32));
    }

    #[test]
    fn resume_picker_marks_the_current_session_in_a_narrow_terminal() {
        let mut terminal = Terminal::new(TestBackend::new(44, 14)).unwrap();
        let mut app = App::new(
            "current-session".into(),
            "workspace".into(),
            "model".into(),
            128_000,
        );
        app.picker = Some(super::super::app::Picker {
            sessions: vec![orbit_store::Session {
                id: "current-session".into(),
                workspace: "workspace".into(),
                model: "model".into(),
                reasoning_effort: None,
                title: "A very long title that should be clipped to the popup".into(),
                created_at_ms: 0,
                updated_at_ms: 0,
            }],
            selected: 0,
            query: "current".into(),
        });

        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let screen = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(screen.contains("Search: current"), "{screen}");
        assert!(screen.contains("(current)"), "{screen}");
    }

    #[test]
    fn file_approval_renders_aligned_before_and_after_panes() {
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        let mut app = App::new(
            "session".into(),
            "workspace".into(),
            "model".into(),
            128_000,
        );
        let (decision, _receiver) = tokio::sync::oneshot::channel();
        app.approval = Some(orbit_tools::approval::ApprovalRequest {
            preview: ApprovalPreview::Write(orbit_tools::approval::WritePreview {
                path: "src/lib.rs".into(),
                before: Some("same\nold\ntail\n".into()),
                after: "same\nnew\ntail\n".into(),
            }),
            decision,
        });

        terminal.draw(|frame| draw(frame, &mut app)).unwrap();

        let screen = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(screen.contains("Before"), "{screen}");
        assert!(screen.contains("After"), "{screen}");
        assert!(screen.contains("old"), "{screen}");
        assert!(screen.contains("new"), "{screen}");
        assert!(app.approval_diff.is_some());
    }
}
