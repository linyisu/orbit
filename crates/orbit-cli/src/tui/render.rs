use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Padding, Paragraph, Wrap},
};

use super::{
    app::{RunStatus, ToolStatus, TranscriptItem, UiState},
    commands::COMMANDS,
};

const ACCENT: Color = Color::Rgb(218, 139, 109);
const MUTED: Color = Color::Rgb(145, 145, 145);
const BORDER: Color = Color::Rgb(85, 85, 85);

fn muted(text: impl Into<String>) -> Span<'static> {
    Span::styled(text.into(), Style::default().fg(MUTED))
}

pub fn draw(frame: &mut Frame<'_>, state: &mut UiState) {
    let area = frame.area();
    state.selection.area = Rect::default();
    if area.width < 24 || area.height < 10 {
        frame.render_widget(
            Paragraph::new("请放大终端\n至少 24 列 × 10 行\nCtrl+D 退出"),
            area,
        );
        return;
    }
    if state.approval.is_some() {
        super::approval::draw(frame, state);
        return;
    }
    let width = area.width.saturating_sub(6) as usize;
    let (input_lines, cursor) = state.input.visual_lines(width);
    let input_height = input_lines
        .len()
        .clamp(1, 6)
        .min(area.height.saturating_sub(8) as usize) as u16;
    let menu = state.menu();
    let panel_height = if !menu.is_empty() {
        menu.len() as u16 + 1
    } else if state.show_help {
        10
    } else {
        0
    };
    let areas = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(input_height + 2),
        Constraint::Length(panel_height.min(area.height.saturating_sub(input_height + 8))),
        Constraint::Length(1),
    ])
    .split(area);

    draw_header(frame, state, areas[0]);
    draw_transcript(frame, state, areas[1]);
    draw_status(frame, state, areas[2]);
    draw_composer(frame, state, areas[3], input_lines, cursor);
    draw_panel(frame, state, areas[4]);
    let hint = if state.running() {
        "  滚轮浏览 · 拖选后 Ctrl+C 复制 · esc 取消"
    } else {
        "  滚轮浏览 · 拖选后 Ctrl+C 复制 · / 命令"
    };
    frame.render_widget(Paragraph::new(Line::from(muted(hint))), areas[5]);
}

fn draw_header(frame: &mut Frame<'_>, state: &UiState, area: Rect) {
    let line = Line::from(vec![
        Span::styled(
            "  ◇ Orbit",
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ),
        muted(format!(
            "  v{}  ·  {}",
            env!("CARGO_PKG_VERSION"),
            state.model
        )),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

fn welcome(state: &UiState) -> Vec<Line<'static>> {
    vec![
        Line::from(""),
        Line::from(Span::styled(
            "  开始一段新对话",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("  ◇  ", Style::default().fg(ACCENT)),
            Span::raw("你好，我是 Orbit。"),
        ]),
        Line::from(muted(if state.demo {
            "     输入 read Cargo.toml，体验真实文件读取。"
        } else {
            "     输入问题，按 Enter 开始。"
        })),
        Line::from(""),
        Line::from(muted(format!("  {}", state.workspace))),
        Line::from(""),
        Line::from(vec![
            Span::styled("  /help", Style::default().fg(ACCENT)),
            muted(" 查看帮助    /model 查看模型"),
        ]),
        Line::from(muted("  read_file 读取 · write_file 写入前确认。")),
        Line::from(""),
    ]
}

fn draw_transcript(frame: &mut Frame<'_>, state: &mut UiState, area: Rect) {
    state.selection.area = Rect {
        x: area.x + 2,
        width: area.width.saturating_sub(4),
        ..area
    };
    let lines = if state.transcript.is_empty() {
        welcome(state)
    } else {
        transcript_lines(state)
    };
    let paragraph = Paragraph::new(Text::from(lines))
        .block(Block::default().padding(Padding::horizontal(2)))
        .wrap(Wrap { trim: false });
    let previous_max = state.max_scroll;
    state.max_scroll = paragraph
        .line_count(area.width.saturating_sub(4))
        .saturating_sub(area.height as usize)
        .min(u16::MAX as usize) as u16;
    if !state.follow_bottom {
        state.scroll = state
            .scroll
            .saturating_add(state.max_scroll.saturating_sub(previous_max));
    }
    state.scroll = state.scroll.min(state.max_scroll);
    let offset = if state.follow_bottom {
        state.max_scroll
    } else {
        state.max_scroll.saturating_sub(state.scroll)
    };
    frame.render_widget(paragraph.scroll((offset, 0)), area);
}

fn transcript_lines(state: &UiState) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for item in &state.transcript {
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
                let mut code = false;
                for (index, line) in text.lines().enumerate() {
                    let prefix = if index == 0 { "● " } else { "  " };
                    if line.trim_start().starts_with("```") {
                        code = !code;
                        let label = line.trim_start().trim_start_matches('`');
                        lines.push(Line::from(muted(if code {
                            format!("  ┌─ {label}")
                        } else {
                            "  └─".into()
                        })));
                    } else if code {
                        lines.push(Line::from(vec![
                            muted("  │ "),
                            Span::styled(line.to_owned(), Style::default().fg(Color::Cyan)),
                        ]));
                    } else if line.starts_with('#') {
                        lines.push(Line::from(Span::styled(
                            format!("{prefix}{}", line.trim_start_matches('#').trim_start()),
                            Style::default().add_modifier(Modifier::BOLD),
                        )));
                    } else {
                        lines.push(Line::from(format!("{prefix}{line}")));
                    }
                }
                lines.push(Line::from(""));
            }
            TranscriptItem::Thinking(text) => {
                if state.expanded {
                    lines.extend(
                        text.lines()
                            .map(|line| Line::from(muted(format!("  ┊ {line}")))),
                    );
                } else {
                    lines.push(Line::from(muted("  ┊ 思考内容 · ctrl+o 展开")));
                }
            }
            TranscriptItem::Tool(row) => {
                let (marker, color) = match row.status {
                    ToolStatus::Running => ("◌", ACCENT),
                    ToolStatus::Finished => ("✓", Color::Green),
                    ToolStatus::Failed => ("×", Color::Red),
                };
                lines.push(Line::from(Span::styled(
                    format!(
                        "  {marker} {} {}",
                        row.name,
                        serde_json::from_str::<serde_json::Value>(&row.arguments)
                            .ok()
                            .and_then(|value| value["path"].as_str().map(str::to_owned))
                            .unwrap_or_default()
                    ),
                    Style::default().fg(color),
                )));
                let output = serde_json::from_str::<serde_json::Value>(&row.output)
                    .ok()
                    .and_then(|value| value["content"].as_str().map(str::to_owned))
                    .unwrap_or_else(|| row.output.clone());
                for line in output
                    .lines()
                    .take(if state.expanded { usize::MAX } else { 2 })
                {
                    lines.push(Line::from(muted(format!("    {line}"))));
                }
                if !state.expanded && output.lines().count() > 2 {
                    lines.push(Line::from(muted("    … ctrl+o 展开")));
                }
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
    lines
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
    state: &UiState,
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
    frame.render_widget(
        Paragraph::new(Span::styled("❯", Style::default().fg(ACCENT))),
        Rect { width: 2, ..inner },
    );
    let text_area = Rect {
        x: inner.x + 2,
        width: inner.width.saturating_sub(2),
        ..inner
    };
    let scroll = cursor
        .1
        .saturating_sub(text_area.height.saturating_sub(1) as usize);
    if state.input.text.is_empty() {
        frame.render_widget(Paragraph::new(muted("输入消息，或 / 查看命令")), text_area);
    } else {
        let text = Text::from(
            lines
                .into_iter()
                .skip(scroll)
                .map(Line::from)
                .collect::<Vec<_>>(),
        );
        frame.render_widget(Paragraph::new(text), text_area);
    }
    if text_area.width > 0 && text_area.height > 0 {
        // A steady, rendered caret never moves the terminal hardware cursor.
        let x = text_area.x + (cursor.0 as u16).min(text_area.width - 1);
        let y = text_area.y + (cursor.1.saturating_sub(scroll) as u16).min(text_area.height - 1);
        frame.buffer_mut()[(x, y)].set_style(Style::default().bg(Color::Gray).fg(Color::Black));
    }
}

fn draw_panel(frame: &mut Frame<'_>, state: &UiState, area: Rect) {
    let menu = state.menu();
    let lines = if !menu.is_empty() {
        menu.iter()
            .enumerate()
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
    } else if state.show_help {
        let mut lines = COMMANDS
            .iter()
            .map(|command| {
                Line::from(muted(format!(
                    "  {:<10} {}",
                    command.name, command.description
                )))
            })
            .collect::<Vec<_>>();
        lines.extend([
            Line::from(muted("  Enter 发送 · Ctrl+J / Shift+Enter 换行")),
            Line::from(muted("  Esc 取消响应 / 关闭菜单 · Ctrl+D 退出")),
            Line::from(muted("  ↑↓ 输入历史 · 滚轮/PgUp/PgDn 滚动 · Ctrl+O 展开")),
            Line::from(muted("  Ctrl+A/E 行首/尾 · Ctrl+U/K 删除到行首/尾")),
            Line::from(muted("  Ctrl+C 取消响应 / 清空输入 / 空输入时退出")),
        ]);
        lines
    } else {
        Vec::new()
    };
    frame.render_widget(Paragraph::new(lines), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

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
    fn welcome_commands_and_small_terminal_render() {
        let mut state = UiState::new("mock-model");
        let screen = render(&mut state, 100, 30);
        assert!(screen.contains("Orbit"));
        assert!(screen.contains("mock-model"));
        assert!(screen.replace(' ', "").contains("开始一段新对话"));
        state.input.insert("/");
        assert!(render(&mut state, 100, 30).contains("/clear"));
        assert!(render(&mut state, 20, 5).contains("请"));
    }

    #[test]
    fn rendered_caret_stays_fixed_while_streaming_and_selecting() {
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
        assert_eq!(terminal.backend().buffer().content[caret].bg, Color::Gray);
        state.apply(orbit_agent::events::AgentEvent::Completed {
            turns: 1,
            tool_calls: 0,
        });
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        assert!(!terminal.backend().cursor_visible());
        assert_eq!(terminal.backend().buffer().content[caret].bg, Color::Gray);
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
}
