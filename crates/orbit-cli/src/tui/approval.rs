use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Margin, Rect},
    style::{Color, Style},
    text::Line,
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
};

use super::app::UiState;

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Approve,
    Deny,
    Cancel,
    None,
}

pub(super) fn file_modal(state: &UiState) -> bool {
    matches!(
        state.approval,
        Some(
            orbit_tools::approval::ApprovalPreview::Write(_)
                | orbit_tools::approval::ApprovalPreview::Delete(_)
        )
    )
}

pub fn handle(state: &mut UiState, event: Event) -> Action {
    let amount = match event {
        Event::Mouse(mouse) => {
            let amount = match mouse.kind {
                MouseEventKind::ScrollUp => -(super::wheel::LINES as isize),
                MouseEventKind::ScrollDown => super::wheel::LINES as isize,
                _ => 0,
            };
            if !state
                .approval_area
                .contains((mouse.column, mouse.row).into())
            {
                if file_modal(state) {
                    return Action::None;
                }
                if mouse.row < state.approval_area.y {
                    if amount < 0 {
                        state.scroll_up(super::wheel::LINES);
                    }
                    if amount > 0 {
                        state.scroll_down(super::wheel::LINES);
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
                KeyCode::Tab => {
                    state.approval_expanded = !state.approval_expanded;
                    state.approval_scroll = 0;
                    return Action::None;
                }
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

#[cfg(test)]
pub fn draw(frame: &mut Frame<'_>, state: &mut UiState) {
    draw_in(frame, state, frame.area());
}

pub fn draw_in(frame: &mut Frame<'_>, state: &mut UiState, area: Rect) {
    if file_modal(state) {
        draw_file_modal(frame, state);
        return;
    }
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
        orbit_tools::approval::ApprovalPreview::Write(_)
        | orbit_tools::approval::ApprovalPreview::Delete(_) => {
            return draw_file_modal(frame, state);
        }
        orbit_tools::approval::ApprovalPreview::Command(preview) => {
            let ws_desc = match preview.workspace_access {
                orbit_tools::approval::CommandWorkspaceAccess::Read => "只读 (受保护路径隔离)",
                orbit_tools::approval::CommandWorkspaceAccess::ReadWrite => "读写 (受保护路径隔离)",
            };
            let net_desc = if preview.network_allowed {
                "允许"
            } else {
                "禁止"
            };
            let mut lines = vec![
                Line::from(format!("工作区：{ws_desc}")),
                Line::from(format!("网络：{net_desc}")),
            ];
            if preview.external_capabilities.is_empty() {
                lines.push(Line::from("外部路径：无额外已授权 Capability"));
            } else {
                lines.push(Line::from("外部路径（已授权 Capability）："));
                for cap in &preview.external_capabilities {
                    lines.push(Line::from(format!("  • {cap}")));
                }
            }
            lines.push(Line::from(
                "受保护路径（如 .env、.git、配置及数据库）已强制隔离屏蔽。",
            ));
            lines.push(Line::from(""));
            lines.extend(
                preview
                    .command
                    .split('\n')
                    .map(|line| Line::styled(visible(line), Style::default().fg(Color::Cyan))),
            );
            (
                format!(
                    "命令执行确认\n超时：{} 秒 · 工作区：{} · 网络：{}",
                    preview.timeout_seconds,
                    match preview.workspace_access {
                        orbit_tools::approval::CommandWorkspaceAccess::Read => "只读",
                        orbit_tools::approval::CommandWorkspaceAccess::ReadWrite => "读写",
                    },
                    net_desc,
                ),
                lines,
            )
        }
        orbit_tools::approval::ApprovalPreview::Mcp(preview) => {
            let mut lines = vec![
                Line::from("MCP 是外部进程；Orbit 无法从工具声明判断其是否会读取、写入或联网。"),
                Line::from("请仅在信任该服务器及本次参数时允许。"),
                Line::from(""),
            ];
            lines.extend(
                preview
                    .arguments
                    .split('\n')
                    .map(|line| Line::styled(visible(line), Style::default().fg(Color::Cyan))),
            );
            (
                format!("MCP 调用确认 · {}::{}", preview.server, preview.tool),
                lines,
            )
        }
        orbit_tools::approval::ApprovalPreview::Capability(preview) => {
            let access_desc = if preview.access.allows_write() {
                "读写权限 (Read/Write)"
            } else {
                "读取权限 (Read)"
            };
            let lines = vec![
                Line::from(format!("环境：{}", preview.environment)),
                Line::from(format!("路径：{}", preview.path.display())),
                Line::from(format!("权限：{}", preview.access.description())),
                Line::from(format!("理由：{}", preview.reason)),
                Line::from(""),
                Line::from("该权限仅在当前 Runtime Session 内有效，会话结束自动销毁。"),
            ];
            (format!("工作区外权限申请 · {access_desc}"), lines)
        }
    };
    frame.render_widget(Paragraph::new(title), areas[0]);
    let lines = if state.approval_expanded {
        lines
    } else {
        let total = lines.len();
        let mut preview: Vec<_> = lines
            .into_iter()
            .take(4)
            .map(|mut line| {
                if line.width() > 160 {
                    line = Line::styled(
                        super::render::compact_preview(&line.to_string(), 160),
                        line.style,
                    );
                }
                line
            })
            .collect();
        if total > 4 {
            preview.push(Line::from(format!("… 另有 {} 行，Tab 展开查看", total - 4)));
        }
        preview
    };
    let paragraph = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL));
    state.approval_max_scroll = paragraph
        .line_count(areas[1].width.saturating_sub(2))
        .saturating_sub(areas[1].height.saturating_sub(2) as usize)
        .min(u16::MAX as usize);
    state.approval_scroll = state.approval_scroll.min(state.approval_max_scroll);
    frame.render_widget(
        paragraph.scroll((state.approval_scroll as u16, 0)),
        areas[1],
    );
    draw_options(
        frame,
        state,
        areas[2],
        if state.approval_expanded {
            "← → 切换 · Enter 确认 · Esc 拒绝 · Tab 收起"
        } else {
            "← → 切换 · Enter 确认 · Esc 拒绝 · Tab 展开"
        },
    );
    state.selection.approval = areas[1].inner(ratatui::layout::Margin::new(1, 1));
    state.approval_shown = true;
}

fn draw_options(frame: &mut Frame<'_>, state: &UiState, area: Rect, hint: &str) {
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
    frame.render_widget(Paragraph::new(vec![options, Line::from(hint)]), area);
}

pub(super) fn draw_file_modal(frame: &mut Frame<'_>, state: &mut UiState) {
    let (path, before, after, operation) = match state.approval.as_ref() {
        Some(orbit_tools::approval::ApprovalPreview::Write(preview)) => (
            preview.path.as_str(),
            preview.before.as_deref().unwrap_or_default(),
            preview.after.as_str(),
            if preview.before.is_some() {
                "修改文件"
            } else {
                "新建文件"
            },
        ),
        Some(orbit_tools::approval::ApprovalPreview::Delete(preview)) => (
            preview.path.as_str(),
            preview.before.as_str(),
            "",
            "删除文件",
        ),
        _ => return,
    };
    let screen = frame.area();
    let area = screen.inner(Margin::new(
        if screen.width >= 40 { 2 } else { 0 },
        if screen.height >= 16 { 1 } else { 0 },
    ));
    super::overlay::clear(frame, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(218, 139, 109)))
        .title(" 文件变更确认 ");
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let sections = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(2),
    ])
    .split(inner);
    let title = super::render::fit_label(
        &format!("{operation} · {}", visible(path)),
        sections[0].width as usize,
        true,
    );
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(title),
            Line::from(format!("{} 字节 → {} 字节", before.len(), after.len())),
        ]),
        sections[0],
    );
    let panes = Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(sections[1]);
    let left_block = Block::default().borders(Borders::ALL).title(" 修改前 ");
    let right_block = Block::default().borders(Borders::ALL).title(" 修改后 ");
    let left = left_block.inner(panes[0]);
    let right = right_block.inner(panes[1]);
    let widths = (left.width, right.width);
    if state
        .approval_diff
        .as_ref()
        .is_none_or(|cache| !cache.matches(before, after, widths, state.approval_expanded))
    {
        state.approval_diff = Some(super::approval_diff::Cache::new(
            before,
            after,
            widths,
            state.approval_expanded,
        ));
    }
    let cache = state.approval_diff.as_ref().unwrap();
    let height = usize::from(left.height.min(right.height));
    state.approval_max_scroll = cache.left.len().saturating_sub(height);
    state.approval_scroll = state.approval_scroll.min(state.approval_max_scroll);
    frame.render_widget(left_block, panes[0]);
    frame.render_widget(right_block, panes[1]);
    // Both panes consume exactly the same aligned physical-row offset.
    for (rows, pane) in [(&cache.left, left), (&cache.right, right)] {
        let visible: Vec<_> = rows
            .iter()
            .skip(state.approval_scroll)
            .take(height)
            .cloned()
            .collect();
        frame.render_widget(Paragraph::new(visible), pane);
    }
    let hint = format!(
        "←→ 选择 · Enter 确认 · Esc 拒绝 · 滚轮同步 · Tab {}",
        if state.approval_expanded {
            "仅看改动"
        } else {
            "显示全文"
        }
    );
    draw_options(frame, state, sections[2], &hint);
    state.approval_area = area;
    state.selection.approval = sections[1];
    // The modal owns pointer interaction; no links or composer clicks below it.
    state.selection.viewport = Rect::default();
    state.selection.input = Rect::default();
    state.selection.footer = Rect::default();
    state.selection.reset_caret();
    state.input_area = Rect::default();
    state.attachment_areas.clear();
    state.context_indicator_area = Rect::default();
    state.ime_cursor = None;
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
        assert_eq!(state.scroll, super::super::wheel::LINES);
        assert_eq!(state.approval_scroll, 0);
        handle(&mut state, wheel(MouseEventKind::ScrollDown, 15));
        assert_eq!(state.scroll, super::super::wheel::LINES);
        assert_eq!(state.approval_scroll, super::super::wheel::LINES);
        handle(&mut state, wheel(MouseEventKind::ScrollDown, 5));
        assert_eq!(state.scroll, 0);
        assert_eq!(state.approval_scroll, super::super::wheel::LINES);
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
    fn modal_wheels_in_either_pane_share_the_scroll_and_do_not_move_the_chat() {
        let mut state = UiState::new("mock");
        state.max_scroll = 100;
        state.input.insert("draft");
        state.approval = Some(orbit_tools::approval::ApprovalPreview::Write(
            WritePreview {
                path: "src/main.rs".into(),
                before: Some("old\n".repeat(100)),
                after: "new\n".repeat(100),
            },
        ));
        let mut terminal = Terminal::new(TestBackend::new(100, 25)).unwrap();
        terminal.draw(|f| draw_file_modal(f, &mut state)).unwrap();
        let cached_rows = state.approval_diff.as_ref().unwrap().left.as_ptr();
        for x in [10, 70] {
            handle(
                &mut state,
                Event::Mouse(crossterm::event::MouseEvent {
                    kind: MouseEventKind::ScrollDown,
                    column: x,
                    row: 10,
                    modifiers: KeyModifiers::NONE,
                }),
            );
        }
        assert_eq!(state.approval_scroll, super::super::wheel::LINES * 2);
        handle(
            &mut state,
            Event::Mouse(crossterm::event::MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            }),
        );
        assert_eq!(state.scroll, 0);
        assert_eq!(state.approval_scroll, super::super::wheel::LINES * 2);
        terminal.draw(|f| draw_file_modal(f, &mut state)).unwrap();
        assert_eq!(
            cached_rows,
            state.approval_diff.as_ref().unwrap().left.as_ptr()
        );
        let row_text: String = (0..100)
            .map(|x| terminal.backend().buffer()[(x, 5)].symbol())
            .collect();
        assert!(row_text.contains("-  17 │ old"), "{row_text}");
        assert!(row_text.contains("+  17 │ new"), "{row_text}");
        assert_eq!(state.input.text, "draft");
        for (width, height) in [(24, 10), (10, 5), (1, 1), (140, 40)] {
            terminal.backend_mut().resize(width, height);
            terminal.resize(Rect::new(0, 0, width, height)).unwrap();
            terminal.draw(|f| draw_file_modal(f, &mut state)).unwrap();
        }
    }

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
        assert!(!state.approval_expanded);
        handle(
            &mut state,
            Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Tab,
                KeyModifiers::NONE,
            )),
        );
        terminal.draw(|f| draw(f, &mut state)).unwrap();
        assert!(state.approval_expanded);
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
