use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Layout},
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
        Event::Mouse(mouse) => match mouse.kind {
            MouseEventKind::ScrollUp => -3,
            MouseEventKind::ScrollDown => 3,
            _ => 0,
        },
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

pub fn draw(frame: &mut Frame<'_>, state: &mut UiState) {
    let preview = state
        .approval
        .as_ref()
        .expect("approval screen requires a proposal");
    let areas = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(3),
    ])
    .split(frame.area());
    frame.render_widget(
        Paragraph::new(format!(
            "写入确认 · {}\n{} · {} 字节 → {} 字节",
            visible(&preview.path),
            if preview.before.is_some() {
                "替换文件"
            } else {
                "创建文件"
            },
            preview.before.as_ref().map_or(0, String::len),
            preview.after.len()
        )),
        areas[0],
    );
    let mut lines = vec![
        Line::from(format!("目标文件：{}", visible(&preview.path))),
        Line::from("--- 修改前（完整内容）"),
    ];
    if let Some(before) = &preview.before {
        lines.extend(before.split('\n').map(|line| {
            Line::styled(
                format!("- {}", visible(line)),
                Style::default().fg(Color::Red),
            )
        }));
        lines.push(Line::from(if before.ends_with('\n') {
            "[末尾有换行]"
        } else {
            "[末尾无换行]"
        }));
    } else {
        lines.push(Line::from("[文件不存在]"));
    }
    lines.push(Line::from("+++ 修改后（完整内容）"));
    lines.extend(preview.after.split('\n').map(|line| {
        Line::styled(
            format!("+ {}", visible(line)),
            Style::default().fg(Color::Green),
        )
    }));
    lines.push(Line::from(if preview.after.ends_with('\n') {
        "[末尾有换行]"
    } else {
        "[末尾无换行]"
    }));
    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL));
    state.approval_max_scroll = paragraph
        .line_count(areas[1].width.saturating_sub(2))
        .saturating_sub(areas[1].height.saturating_sub(2) as usize)
        .min(u16::MAX as usize) as u16;
    state.approval_scroll = state.approval_scroll.min(state.approval_max_scroll);
    frame.render_widget(paragraph.scroll((state.approval_scroll, 0)), areas[1]);
    let help = if frame.area().width < 80 {
        "Ctrl+Y 允许  Esc 拒绝\n↑↓ 翻页  Ctrl+C 取消"
    } else {
        "Ctrl+Y 允许这次写入 · Ctrl+N / Esc 拒绝 · Ctrl+C 取消本轮\n↑↓ / 鼠标 / PgUp PgDn 翻阅 · Enter 不会确认"
    };
    let mouse_hint = if state.mouse_capture {
        "F2 切回拖选复制"
    } else {
        "可拖选复制 · F2 开启滚轮"
    };
    frame.render_widget(Paragraph::new(format!("{help}\n{mouse_hint}")), areas[2]);
    state.approval_shown = true;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

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
            Action::None
        );
        assert_eq!(handle(&mut state, yes), Action::Approve);
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
        state.approval = Some(WritePreview {
            path: "file".into(),
            before: Some("old".into()),
            after: format!(
                "{}END\\marker\u{1b}",
                "long line that wraps across the screen\n".repeat(80)
            ),
        });
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
        assert!(text.contains("Ctrl+Y"));
    }
}
