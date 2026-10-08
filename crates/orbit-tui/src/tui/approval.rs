use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
};

use super::{app::UiState, render::safe_output};

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Approve,
    AlwaysAllow,
    Deny,
    Cancel,
    None,
}

/// Inline approvals own only explicit approval shortcuts and pointer events in their panel.
/// Editing/navigation/Enter remain composer actions, even while a tool waits for approval.
pub fn captures(state: &UiState, event: &Event) -> bool {
    match event {
        Event::Key(key) => {
            key.kind == KeyEventKind::Press
                && key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('y' | 'Y' | 'n' | 'c'))
                && !(key.code == KeyCode::Char('c') && state.input.selected_text().is_some())
        }
        Event::Mouse(mouse) => state
            .approval_area
            .contains((mouse.column, mouse.row).into()),
        _ => false,
    }
}

pub fn handle(state: &mut UiState, event: Event) -> Action {
    let amount = match event {
        Event::Mouse(mouse) => {
            if let Some((_, choice)) = state
                .approval_options
                .iter()
                .find(|(area, _)| area.contains((mouse.column, mouse.row).into()))
            {
                match mouse.kind {
                    MouseEventKind::Moved => state.approval_choice = *choice,
                    MouseEventKind::Down(crossterm::event::MouseButton::Left) => {
                        state.approval_choice = *choice;
                        if state.approval_shown {
                            return match *choice {
                                0 => Action::Approve,
                                1 => Action::AlwaysAllow,
                                _ => Action::Deny,
                            };
                        }
                    }

                    _ => {}
                }

                return Action::None;
            }

            let amount = match mouse.kind {
                MouseEventKind::ScrollUp => -(super::wheel::LINES as isize),
                MouseEventKind::ScrollDown => super::wheel::LINES as isize,
                _ => 0,
            };

            if !state
                .approval_area
                .contains((mouse.column, mouse.row).into())
            {
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

        Event::Key(key) if key.kind == KeyEventKind::Press => {
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                return match key.code {
                    KeyCode::Char('y' | 'Y') if state.approval_shown => {
                        if key.modifiers.contains(KeyModifiers::SHIFT) {
                            Action::AlwaysAllow
                        } else {
                            Action::Approve
                        }
                    }
                    KeyCode::Char('n') => Action::Deny,
                    KeyCode::Char('c') => Action::Cancel,
                    _ => Action::None,
                };
            }
            0
        }

        _ => 0,
    };

    state.approval_scroll = state
        .approval_scroll
        .saturating_add_signed(amount)
        .min(state.approval_max_scroll);

    Action::None
}

#[cfg(test)]
pub fn draw(frame: &mut Frame<'_>, state: &mut UiState) {
    draw_in(frame, state, frame.area());
}

pub(super) fn needed_height(preview: &orbit_tools::approval::ApprovalPreview) -> u16 {
    match preview {
        orbit_tools::approval::ApprovalPreview::Command(p) => {
            let cmd_lines = p.command.lines().count().clamp(1, 6) as u16;
            let mut extra = 0;
            if p.cwd.is_some() {
                extra += 1;
            }
            if let Some(git) = &p.git {
                match &git.kind {
                    orbit_runtime::git::GitOperationKind::Dangerous { .. }
                    | orbit_runtime::git::GitOperationKind::RemoteWrite { .. }
                    | orbit_runtime::git::GitOperationKind::BranchSwitchOrRebase { .. }
                    | orbit_runtime::git::GitOperationKind::LocalWrite { .. }
                    | orbit_runtime::git::GitOperationKind::ConfigMutation { .. } => {
                        extra += 1;
                    }

                    _ => {}
                }
            }

            4 + extra + cmd_lines
        }

        orbit_tools::approval::ApprovalPreview::Write(_) => 10,
        orbit_tools::approval::ApprovalPreview::Delete(p) => {
            let lines = p.before.lines().count().clamp(1, 6) as u16;
            4 + lines
        }

        orbit_tools::approval::ApprovalPreview::Capability(_) => 7,
        orbit_tools::approval::ApprovalPreview::Mcp(p) => {
            let arg_lines = p.arguments.lines().count().clamp(1, 4) as u16;
            4 + arg_lines
        }
    }
}

pub fn draw_in(frame: &mut Frame<'_>, state: &mut UiState, area: Rect) {
    if area.height < 3 || area.width < 10 {
        return;
    }

    let area = Rect {
        x: area.x + 2,
        width: area.width.saturating_sub(4),
        ..area
    };

    state.approval_area = area;
    let proposal = state
        .approval
        .as_ref()
        .expect("approval screen requires a proposal");

    let border_color = crate::tui::theme::THEME.accent;

    let (title, header_lines, body_lines) = match proposal {
        orbit_tools::approval::ApprovalPreview::Write(preview) => {
            let before = preview.before.as_deref().unwrap_or_default();
            let after = preview.after.as_str();
            let op_tag = if preview.before.is_some() {
                "[修改文件]"
            } else {
                "[新建文件]"
            };
            let title = Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    "权限确认",
                    Style::default()
                        .fg(crate::tui::theme::THEME.white)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(op_tag, Style::default().fg(crate::tui::theme::THEME.accent)),
                Span::raw(" "),
                Span::styled(
                    preview.path.clone(),
                    Style::default()
                        .fg(crate::tui::theme::THEME.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
            ]);

            let diff = similar::TextDiff::configure()
                .timeout(std::time::Duration::from_millis(250))
                .diff_lines(before, after);

            let old_lines = super::code::file_lines(&preview.path, before);
            let new_lines = super::code::file_lines(&preview.path, after);

            let mut diff_lines = Vec::new();

            let digits = before
                .lines()
                .count()
                .max(after.lines().count())
                .to_string()
                .len()
                .max(4);

            for change in diff.iter_all_changes() {
                let (index, source, change_style) = match change.tag() {
                    similar::ChangeTag::Delete => (
                        change.old_index(),
                        &old_lines,
                        Some(super::code::Change::Removed),
                    ),

                    similar::ChangeTag::Insert => (
                        change.new_index(),
                        &new_lines,
                        Some(super::code::Change::Added),
                    ),

                    similar::ChangeTag::Equal => (change.new_index(), &new_lines, None),
                };

                if let Some(index) = index
                    && let Some(source) = source.get(index)
                {
                    diff_lines.extend(super::code::render_line(
                        source,
                        super::code::Options {
                            gutter: super::code::Gutter::Diff {
                                number: index as u64 + 1,
                                digits,
                                indent: 2,
                            },
                            width: Some(area.width.saturating_sub(2)),
                            change: change_style,
                            ..Default::default()
                        },
                    ));
                }
            }

            if diff_lines.is_empty() {
                diff_lines.push(Line::from("  （无内容差异）"));
            }

            (title, Vec::new(), diff_lines)
        }

        orbit_tools::approval::ApprovalPreview::Delete(preview) => {
            let title = Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    "权限确认",
                    Style::default()
                        .fg(crate::tui::theme::THEME.white)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(
                    "[删除文件]",
                    Style::default().fg(crate::tui::theme::THEME.error),
                ),
                Span::raw(" "),
                Span::styled(
                    preview.path.clone(),
                    Style::default()
                        .fg(crate::tui::theme::THEME.error)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
            ]);
            let digits = preview.before.lines().count().to_string().len().max(4);
            let diff_lines = super::code::file_lines(&preview.path, &preview.before)
                .iter()
                .enumerate()
                .flat_map(|(index, line)| {
                    super::code::render_line(
                        line,
                        super::code::Options {
                            gutter: super::code::Gutter::Diff {
                                number: index as u64 + 1,
                                digits,
                                indent: 2,
                            },
                            width: Some(area.width.saturating_sub(2)),
                            change: Some(super::code::Change::Removed),
                            ..Default::default()
                        },
                    )
                })
                .collect::<Vec<_>>();

            (title, Vec::new(), diff_lines)
        }

        orbit_tools::approval::ApprovalPreview::Command(preview) => {
            let access = match preview.workspace_access {
                orbit_tools::approval::CommandWorkspaceAccess::Read => "只读",
                orbit_tools::approval::CommandWorkspaceAccess::ReadWrite => "读写",
            };

            let mut title_spans = vec![
                Span::raw(" "),
                Span::styled(
                    "权限确认",
                    Style::default()
                        .fg(crate::tui::theme::THEME.white)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(
                    if preview.sandbox {
                        format!("[沙箱 · {access}]")
                    } else {
                        "[普通执行]".into()
                    },
                    Style::default().fg(crate::tui::theme::THEME.accent),
                ),
            ];

            if preview.network_allowed {
                title_spans.push(Span::raw(" "));
                title_spans.push(Span::styled(
                    "[联网]",
                    Style::default().fg(crate::tui::theme::THEME.accent),
                ));
            }

            title_spans.push(Span::raw(" "));
            let title = Line::from(title_spans);

            let mut header_lines = Vec::new();
            if let Some(git) = &preview.git {
                match &git.kind {
                    orbit_runtime::git::GitOperationKind::RemoteWrite {
                        remote,
                        source_branch,
                        target_branch,
                    } => {
                        let branch_str = match (source_branch, target_branch) {
                            (Some(s), Some(t)) if s == t => format!("{remote}/{t}"),
                            (Some(s), Some(t)) => format!("{s} -> {remote}/{t}"),
                            _ => remote.clone(),
                        };

                        header_lines.push(Line::from(vec![
                            Span::raw("  "),
                            Span::styled(
                                "远端推送: ",
                                Style::default().fg(crate::tui::theme::THEME.muted),
                            ),
                            Span::styled(
                                branch_str,
                                Style::default().fg(crate::tui::theme::THEME.accent),
                            ),
                        ]));
                    }

                    orbit_runtime::git::GitOperationKind::Dangerous { subcommand, reason } => {
                        header_lines.push(Line::from(vec![
                            Span::raw("  "),
                            Span::styled(
                                format!("高危操作 ({subcommand}): "),
                                Style::default()
                                    .fg(crate::tui::theme::THEME.danger)
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(
                                reason.clone(),
                                Style::default().fg(crate::tui::theme::THEME.danger),
                            ),
                        ]));
                    }

                    orbit_runtime::git::GitOperationKind::BranchSwitchOrRebase {
                        subcommand,
                        target,
                        risk_notice,
                    } => {
                        let desc = risk_notice.as_deref().unwrap_or("可能影响未提交工作区修改");
                        header_lines.push(Line::from(vec![
                            Span::raw("  "),
                            Span::styled(
                                format!("分支操作: {subcommand} -> {target}（{desc}）"),
                                Style::default().fg(crate::tui::theme::THEME.description),
                            ),
                        ]));
                    }

                    orbit_runtime::git::GitOperationKind::LocalWrite {
                        subcommand,
                        description,
                    } => {
                        header_lines.push(Line::from(vec![
                            Span::raw("  "),
                            Span::styled(
                                format!("本地写入 ({subcommand}): {description}"),
                                Style::default().fg(crate::tui::theme::THEME.description),
                            ),
                        ]));
                    }

                    orbit_runtime::git::GitOperationKind::ConfigMutation { key, .. } => {
                        let target_key = key.as_deref().unwrap_or("仓库配置/Hooks");
                        header_lines.push(Line::from(vec![
                            Span::raw("  "),
                            Span::styled(
                                format!("配置保护: {target_key}"),
                                Style::default().fg(crate::tui::theme::THEME.description),
                            ),
                        ]));
                    }

                    _ => {}
                }
            }

            if let Some(ref cwd) = preview.cwd {
                header_lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled("cwd: ", Style::default().fg(crate::tui::theme::THEME.muted)),
                    Span::styled(
                        safe_output(cwd),
                        Style::default().fg(crate::tui::theme::THEME.description),
                    ),
                ]));
            }

            let mut body_lines = Vec::new();
            for (i, line) in preview.command.lines().enumerate() {
                let prefix = if i == 0 { "  $ " } else { "    " };
                body_lines.push(Line::from(vec![
                    Span::styled(
                        prefix,
                        Style::default()
                            .fg(crate::tui::theme::THEME.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        safe_output(line),
                        Style::default()
                            .fg(crate::tui::theme::THEME.white)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
            }

            (title, header_lines, body_lines)
        }

        orbit_tools::approval::ApprovalPreview::Mcp(preview) => {
            let title = Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    "权限确认",
                    Style::default()
                        .fg(crate::tui::theme::THEME.white)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(
                    "[工具调用]",
                    Style::default().fg(crate::tui::theme::THEME.accent),
                ),
                Span::raw(" "),
                Span::styled(
                    format!("{}::{}", preview.server, preview.tool),
                    Style::default().fg(crate::tui::theme::THEME.strong),
                ),
                Span::raw(" "),
            ]);

            let body_lines = preview
                .arguments
                .split('\n')
                .map(|line| {
                    Line::styled(
                        format!("  {}", safe_output(line)),
                        Style::default().fg(crate::tui::theme::THEME.approval_link),
                    )
                })
                .collect();

            (title, Vec::new(), body_lines)
        }

        orbit_tools::approval::ApprovalPreview::Capability(preview) => {
            let access_desc = if preview.access.allows_write() {
                "[外部读写]"
            } else {
                "[外部读取]"
            };
            let title = Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    "权限确认",
                    Style::default()
                        .fg(crate::tui::theme::THEME.white)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(
                    access_desc,
                    Style::default().fg(crate::tui::theme::THEME.accent),
                ),
                Span::raw(" "),
            ]);

            let mut header_lines = Vec::new();
            if !preview.reason.is_empty() {
                header_lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        "理由: ",
                        Style::default().fg(crate::tui::theme::THEME.muted),
                    ),
                    Span::styled(
                        &preview.reason,
                        Style::default().fg(crate::tui::theme::THEME.description),
                    ),
                ]));
            }

            let body_lines = vec![Line::styled(
                format!("  - {}/*", preview.path.display()),
                Style::default().fg(crate::tui::theme::THEME.approval_foreground),
            )];

            (title, header_lines, body_lines)
        }
    };

    let panel_block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(title);
    let inner_area = panel_block.inner(area);
    frame.render_widget(panel_block, area);

    let header_height = if !header_lines.is_empty() && inner_area.height >= 4 {
        header_lines.len().min(2) as u16
    } else {
        0
    };
    let show_gap = inner_area.height >= header_height + 3;
    let gap_height = if show_gap { 1 } else { 0 };

    let areas = Layout::vertical([
        Constraint::Length(header_height),
        Constraint::Min(1),
        Constraint::Length(gap_height),
        Constraint::Length(1),
    ])
    .split(inner_area);

    if header_height > 0 {
        frame.render_widget(Paragraph::new(header_lines), areas[0]);
    }

    let paragraph = Paragraph::new(body_lines).wrap(Wrap { trim: false });
    state.approval_max_scroll = paragraph
        .line_count(areas[1].width)
        .saturating_sub(areas[1].height as usize)
        .min(u16::MAX as usize);
    state.approval_scroll = state.approval_scroll.min(state.approval_max_scroll);
    frame.render_widget(
        paragraph.scroll((state.approval_scroll as u16, 0)),
        areas[1],
    );

    draw_options(frame, state, areas[3]);
    state.selection.approval = areas[1];
    state.approval_shown = true;
}

fn draw_options(frame: &mut Frame<'_>, state: &mut UiState, area: Rect) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let labels = if area.width >= 66 {
        [
            "[ 允许一次 Ctrl+Y ]",
            "[ 始终允许 Ctrl+Shift+Y ]",
            "[ 拒绝 Ctrl+N ]",
        ]
    } else {
        ["[ 允许一次 ]", "[ 始终允许 ]", "[ 拒绝 ]"]
    };
    use unicode_width::UnicodeWidthStr;
    let opt0_x = area.x + 1;
    let opt0_w = labels[0].width() as u16;
    let opt1_x = opt0_x + opt0_w + 2;
    let opt1_w = labels[1].width() as u16;
    let opt2_x = opt1_x + opt1_w + 2;
    let opt2_w = labels[2].width() as u16;

    state.approval_options = vec![
        (Rect::new(opt0_x, area.y, opt0_w, 1).intersection(area), 0),
        (Rect::new(opt1_x, area.y, opt1_w, 1).intersection(area), 1),
        (Rect::new(opt2_x, area.y, opt2_w, 1).intersection(area), 2),
    ];

    let selected_style_normal = Style::default()
        .bg(crate::tui::theme::THEME.accent)
        .fg(crate::tui::theme::THEME.on_accent)
        .add_modifier(Modifier::BOLD);
    let selected_style_danger = Style::default()
        .bg(crate::tui::theme::THEME.danger)
        .fg(crate::tui::theme::THEME.white)
        .add_modifier(Modifier::BOLD);
    let normal_style = Style::default().fg(crate::tui::theme::THEME.approval_detail);
    let normal_style_danger = Style::default().fg(crate::tui::theme::THEME.approval_muted);

    let spans = vec![
        Span::raw(" "),
        Span::styled(
            labels[0],
            if state.approval_choice == 0 {
                selected_style_normal
            } else {
                normal_style
            },
        ),
        Span::raw("  "),
        Span::styled(
            labels[1],
            if state.approval_choice == 1 {
                selected_style_normal
            } else {
                normal_style
            },
        ),
        Span::raw("  "),
        Span::styled(
            labels[2],
            if state.approval_choice == 2 {
                selected_style_danger
            } else {
                normal_style_danger
            },
        ),
    ];

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[cfg(test)]
mod tests {
    #[test]
    fn clicking_a_visible_approval_option_confirms_it() {
        use crossterm::event::{MouseButton, MouseEvent};
        let mut state = UiState::new("model");
        state.approval_options = vec![
            (Rect::new(2, 3, 10, 1), 0),
            (Rect::new(13, 3, 10, 1), 1),
            (Rect::new(24, 3, 8, 1), 2),
        ];
        let click = |column| {
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column,
                row: 3,
                modifiers: KeyModifiers::NONE,
            })
        };

        assert_eq!(handle(&mut state, click(3)), Action::None);
        state.approval_shown = true;
        assert_eq!(handle(&mut state, click(3)), Action::Approve);
        assert_eq!(handle(&mut state, click(15)), Action::AlwaysAllow);
        assert_eq!(handle(&mut state, click(26)), Action::Deny);
    }

    use crossterm::event::KeyEvent;

    use super::*;

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
    fn composer_keys_never_approve_or_get_captured() {
        let mut state = UiState::new("mock");
        state.approval_shown = true;
        for code in [
            KeyCode::Enter,
            KeyCode::Left,
            KeyCode::Tab,
            KeyCode::Esc,
            KeyCode::Char('a'),
        ] {
            let event = Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
            assert!(!captures(&state, &event));
            assert_eq!(handle(&mut state, event), Action::None);
        }
        assert!(!captures(&state, &Event::Paste("draft".into())));
        assert!(!captures(
            &state,
            &Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL))
        ));
        for (modifiers, expected) in [
            (KeyModifiers::CONTROL, Action::Approve),
            (
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                Action::AlwaysAllow,
            ),
        ] {
            let event = Event::Key(KeyEvent::new(KeyCode::Char('y'), modifiers));
            assert!(captures(&state, &event));
            assert_eq!(handle(&mut state, event), expected);
        }
    }
}

#[cfg(test)]
mod render_tests;
