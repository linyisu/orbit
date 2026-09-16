use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph, Wrap},
};

use super::app::UiState;

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Approve,
    Deny,
    Cancel,
    None,
}

pub fn handle(state: &mut UiState, event: Event) -> Action {
    let amount = match event {
        Event::Mouse(mouse) => {
            let amount = match mouse.kind {
                MouseEventKind::ScrollUp => -3,
                MouseEventKind::ScrollDown => 3,
                _ => 0,
            };
            if !state
                .approval_area
                .contains((mouse.column, mouse.row).into())
            {
                if mouse.row < state.approval_area.y {
                    if amount < 0 {
                        state.scroll_up(3);
                    }
                    if amount > 0 {
                        state.scroll_down(3);
                    }
                }
                return Action::None;
            }
            amount
        }
        Event::Key(key) if key.kind != KeyEventKind::Release => {
            if key.modifiers == KeyModifiers::CONTROL {
                return match key.code {
                    KeyCode::Char('y') if state.approval_shown => Action::Approve,
                    KeyCode::Char('n') => Action::Deny,
                    KeyCode::Char('c') => Action::Cancel,
                    _ => Action::None,
                };
            }
            match key.code {
                KeyCode::Left | KeyCode::Right => {
                    state.approval_choice ^= 1;
                    return Action::None;
                }
                KeyCode::Enter if state.approval_shown => {
                    return if state.approval_choice == 0 {
                        Action::Approve
                    } else {
                        Action::Deny
                    };
                }
                KeyCode::Esc => return Action::Deny,
                KeyCode::Up => -1,
                KeyCode::Down => 1,
                KeyCode::PageUp => -12,
                KeyCode::PageDown => 12,
                KeyCode::Home => {
                    state.approval_scroll = 0;
                    return Action::None;
                }
                KeyCode::End => {
                    state.approval_scroll = state.approval_max_scroll;
                    return Action::None;
                }
                _ => 0,
            }
        }
        _ => 0,
    };
    state.approval_scroll = state
        .approval_scroll
        .saturating_add_signed(amount)
        .min(state.approval_max_scroll);
    Action::None
}

fn visible(text: &str) -> String {
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

fn write_diff(preview: &orbit_tools::approval::WritePreview) -> Vec<Line<'static>> {
    let before: Vec<_> = preview
        .before
        .as_deref()
        .unwrap_or("")
        .split_inclusive('\n')
        .collect();
    let after: Vec<_> = preview.after.split_inclusive('\n').collect();
    let prefix = before
        .iter()
        .zip(&after)
        .take_while(|(a, b)| a == b)
        .count();
    let suffix = before[prefix..]
        .iter()
        .rev()
        .zip(after[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let start = prefix.saturating_sub(3);
    let mut lines = Vec::new();
    if start > 0 {
        lines.push(Line::from(format!("  … 前 {start} 行未改动")));
    }
    let mut add = |text: &str, marker: &str, color: Color| {
        lines.push(Line::styled(
            format!(
                "{marker}{}",
                visible(text.strip_suffix('\n').unwrap_or(text))
            ),
            Style::default().fg(color),
        ));
    };
    for text in &before[start..prefix] {
        add(text, "  ", Color::Reset);
    }
    for text in &before[prefix..before.len() - suffix] {
        add(text, "- ", Color::Red);
    }
    for text in &after[prefix..after.len() - suffix] {
        add(text, "+ ", Color::Green);
    }
    for text in after[after.len() - suffix..].iter().take(3) {
        add(text, "  ", Color::Reset);
    }
    if suffix > 3 {
        lines.push(Line::from(format!("  … 后 {} 行未改动", suffix - 3)));
    }
    lines.push(Line::from(if preview.after.ends_with('\n') {
        "[修改后末尾有换行]"
    } else {
        "[修改后末尾无换行]"
    }));
    lines
}

#[cfg(test)]
pub fn draw(frame: &mut Frame<'_>, state: &mut UiState) {
    draw_in(frame, state, frame.area());
}

pub fn draw_in(frame: &mut Frame<'_>, state: &mut UiState, area: Rect) {
    state.approval_area = area;
    let proposal = state
        .approval
        .as_ref()
        .expect("approval screen requires a proposal");
    let areas = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(2),
    ])
    .split(area);
    let (title, lines) = match proposal {
        orbit_tools::approval::ApprovalPreview::Write(preview) => {
            let title = format!(
                "写入确认 · {}\n{} · {} 字节 → {} 字节",
                visible(&preview.path),
                if preview.before.is_some() {
                    "替换文件"
                } else {
                    "创建文件"
                },
                preview.before.as_ref().map_or(0, String::len),
                preview.after.len()
            );
            let lines = write_diff(preview);
            (title, lines)
        }
        orbit_tools::approval::ApprovalPreview::Command(preview) => {
            let mut lines = vec![
                Line::from(if preview.writable_snapshot {
                    "临时可写副本 · 128 MiB · 结束后丢弃修改 · 无网络"
                } else {
                    "只读项目副本 · 无网络 · 不修改原项目"
                }),
                Line::from("不含配置、数据库、.git、符号链接和依赖缓存；仅系统工具。"),
                Line::from(""),
            ];
            lines.extend(
                preview
                    .command
                    .split('\n')
                    .map(|line| Line::styled(visible(line), Style::default().fg(Color::Cyan))),
            );
            (
                format!(
                    "命令执行确认\n超时：{} 秒 · 原项目只读 · 无网络",
                    preview.timeout_seconds
                ),
                lines,
            )
        }
    };
    frame.render_widget(Paragraph::new(title), areas[0]);
    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL));
    state.approval_max_scroll = paragraph
        .line_count(areas[1].width.saturating_sub(2))
        .saturating_sub(areas[1].height.saturating_sub(2) as usize)
        .min(u16::MAX as usize) as u16;
    state.approval_scroll = state.approval_scroll.min(state.approval_max_scroll);
    frame.render_widget(paragraph.scroll((state.approval_scroll, 0)), areas[1]);
    let options = Line::from(vec![
        ratatui::text::Span::styled(
            "  允许  ",
            if state.approval_choice == 0 {
                Style::default().bg(Color::White).fg(Color::Black)
            } else {
                Style::default().fg(Color::Reset)
            },
        ),
        ratatui::text::Span::raw("   "),
        ratatui::text::Span::styled(
            "  拒绝  ",
            if state.approval_choice == 1 {
                Style::default().bg(Color::White).fg(Color::Black)
            } else {
                Style::default().fg(Color::Reset)
            },
        ),
    ]);
    frame.render_widget(
        Paragraph::new(vec![
            options,
            Line::from("← → 切换 · Enter 确认 · Esc 拒绝"),
        ]),
        areas[2],
    );
    state.selection.area = areas[1].inner(ratatui::layout::Margin::new(1, 1));
    state.approval_shown = true;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    #[test]
    fn wheel_targets_chat_or_inline_preview_without_changing_input() {
        let mut state = UiState::new("mock");
        state.input.insert("draft");
        state.max_scroll = 100;
        state.approval_max_scroll = 100;
        state.approval_area = Rect::new(0, 12, 80, 10);
        let wheel = |kind, row| {
            Event::Mouse(crossterm::event::MouseEvent {
                kind,
                column: 10,
                row,
                modifiers: KeyModifiers::NONE,
            })
        };
        handle(&mut state, wheel(MouseEventKind::ScrollUp, 5));
        assert_eq!(state.scroll, 3);
        assert_eq!(state.approval_scroll, 0);
        handle(&mut state, wheel(MouseEventKind::ScrollDown, 15));
        assert_eq!(state.scroll, 3);
        assert_eq!(state.approval_scroll, 3);
        handle(&mut state, wheel(MouseEventKind::ScrollDown, 5));
        assert_eq!(state.scroll, 0);
        assert_eq!(state.approval_scroll, 3);
        assert_eq!(state.input.text, "draft");
    }

    #[test]
    fn approval_requires_explicit_key_after_preview_and_preserves_draft() {
        let mut state = UiState::new("mock");
        state.input.insert("draft");
        let yes = Event::Key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));
        assert_eq!(handle(&mut state, yes.clone()), Action::None);
        state.approval_shown = true;
        assert_eq!(handle(&mut state, Event::Paste("yes".into())), Action::None);
        assert_eq!(
            handle(
                &mut state,
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            ),
            Action::Approve
        );
        assert_eq!(handle(&mut state, yes), Action::Approve);
        handle(
            &mut state,
            Event::Key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE)),
        );
        assert_eq!(
            handle(
                &mut state,
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            ),
            Action::Deny
        );
        handle(
            &mut state,
            Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)),
        );
        assert_eq!(
            handle(
                &mut state,
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            ),
            Action::Approve
        );

        assert_eq!(
            handle(
                &mut state,
                Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))
            ),
            Action::Deny
        );
        assert_eq!(state.input.text, "draft");
    }
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use orbit_tools::approval::WritePreview;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn full_preview_scrolls_to_end_and_escapes_terminal_controls() {
        let mut state = UiState::new("mock");
        state.approval = Some(orbit_tools::approval::ApprovalPreview::Write(
            WritePreview {
                path: "file".into(),
                before: Some("old".into()),
                after: format!(
                    "{}END\\marker\u{1b}",
                    "long line that wraps across the screen\n".repeat(80)
                ),
            },
        ));
        let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
        terminal.draw(|f| draw(f, &mut state)).unwrap();
        assert!(state.approval_shown);
        assert!(state.approval_max_scroll > 0);
        state.approval_scroll = state.approval_max_scroll;
        terminal.draw(|f| draw(f, &mut state)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("END\\marker\\u{1b}"), "{text}");
        assert!(
            text.replace(' ', "").contains("允许") && text.replace(' ', "").contains("拒绝"),
            "{text}"
        );
    }
}
