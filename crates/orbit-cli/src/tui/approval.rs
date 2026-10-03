use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Margin, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
};

use super::app::UiState;

#[derive(Debug, PartialEq, Eq)]
pub enum Action {
    Approve,
    AlwaysAllow,
    Deny,
    Cancel,
    None,
}

pub(super) fn file_modal(_state: &UiState) -> bool {
    false
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
                    KeyCode::Char('a') if state.approval_shown => Action::AlwaysAllow,
                    KeyCode::Char('n') => Action::Deny,
                    KeyCode::Char('c') => Action::Cancel,
                    _ => Action::None,
                };
            }

            match key.code {
                KeyCode::Tab => {
                    state.approval_choice = (state.approval_choice + 1) % 3;
                    return Action::None;
                }
                KeyCode::Left => {
                    state.approval_choice = state.approval_choice.saturating_sub(1);
                    return Action::None;
                }
                KeyCode::Right => {
                    state.approval_choice = (state.approval_choice + 1).min(2);
                    return Action::None;
                }
                KeyCode::Enter if state.approval_shown => {
                    return match state.approval_choice {
                        0 => Action::Approve,
                        1 => Action::AlwaysAllow,
                        _ => Action::Deny,
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
    if file_modal(state) {
        draw_file_modal(frame, state);
        return;
    }

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

            for change in diff.iter_all_changes() {
                let (line_no, sign, tag) = match change.tag() {
                    similar::ChangeTag::Delete => (
                        format!("{:>4}", change.old_index().unwrap_or(0) + 1),
                        "-",
                        similar::ChangeTag::Delete,
                    ),
                    similar::ChangeTag::Insert => (
                        format!("{:>4}", change.new_index().unwrap_or(0) + 1),
                        "+",
                        similar::ChangeTag::Insert,
                    ),
                    similar::ChangeTag::Equal => (
                        format!("{:>4}", change.new_index().unwrap_or(0) + 1),
                        " ",
                        similar::ChangeTag::Equal,
                    ),
                };

                let sign_style = match tag {
                    similar::ChangeTag::Delete => {
                        Style::default().fg(crate::tui::theme::THEME.error)
                    }
                    similar::ChangeTag::Insert => {
                        Style::default().fg(crate::tui::theme::THEME.success)
                    }
                    similar::ChangeTag::Equal => {
                        Style::default().fg(crate::tui::theme::THEME.diff_context)
                    }
                };

                let mut spans = vec![
                    Span::raw("  "),
                    Span::styled(
                        format!("{line_no} "),
                        Style::default().fg(crate::tui::theme::THEME.line_number),
                    ),
                    Span::styled(format!("{sign} "), sign_style),
                ];

                let source = if tag == similar::ChangeTag::Delete {
                    change.old_index().and_then(|index| old_lines.get(index))
                } else {
                    change.new_index().and_then(|index| new_lines.get(index))
                };
                let background = match tag {
                    similar::ChangeTag::Delete => Some(super::theme::THEME.diff_removed_bg),
                    similar::ChangeTag::Insert => Some(super::theme::THEME.diff_added_bg),
                    similar::ChangeTag::Equal => None,
                };
                if let Some(source) = source {
                    spans.extend(source.spans.iter().cloned().map(|mut span| {
                        if let Some(background) = background {
                            span.style = span.style.bg(background);
                        }
                        span
                    }));
                }

                diff_lines.push(Line::from(spans));
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
            let diff_lines = super::code::file_lines(&preview.path, &preview.before)
                .into_iter()
                .enumerate()
                .map(|(index, mut line)| {
                    for span in &mut line.spans {
                        span.style = span.style.bg(crate::tui::theme::THEME.diff_removed_bg);
                    }
                    line.spans.insert(
                        0,
                        Span::styled(
                            format!("  {:>4} - ", index + 1),
                            Style::default().fg(crate::tui::theme::THEME.error),
                        ),
                    );
                    line
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
                    format!("[{access}]"),
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
                        visible(cwd),
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
                        visible(line),
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
                        format!("  {}", visible(line)),
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

    let opt0_x = area.x + 1;
    let opt0_w = 12;
    let opt1_x = opt0_x + opt0_w + 2;
    let opt1_w = 12;
    let opt2_x = opt1_x + opt1_w + 2;
    let opt2_w = 8;

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
            "[ 允许一次 ]",
            if state.approval_choice == 0 {
                selected_style_normal
            } else {
                normal_style
            },
        ),
        Span::raw("  "),
        Span::styled(
            "[ 始终允许 ]",
            if state.approval_choice == 1 {
                selected_style_normal
            } else {
                normal_style
            },
        ),
        Span::raw("  "),
        Span::styled(
            "[ 拒绝 ]",
            if state.approval_choice == 2 {
                selected_style_danger
            } else {
                normal_style_danger
            },
        ),
    ];

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
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
        .border_style(Style::default().fg(crate::tui::theme::THEME.accent))
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
        .is_none_or(|cache| !cache.matches(before, after, widths, state.approval_expanded, path))
    {
        state.approval_diff = Some(super::approval_diff::Cache::new(
            before,
            after,
            widths,
            state.approval_expanded,
            path,
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

    draw_options(frame, state, sections[2]);
    state.approval_area = area;
    state.selection.approval = sections[1];
    // The modal owns pointer interaction; no links or composer clicks below it.
    state.selection.viewport = Rect::default();
    state.selection.footer = Rect::default();
    state.input_area = Rect::default();
    state.attachment_areas.clear();
    state.context_indicator_area = Rect::default();
    state.ime_cursor = None;
    state.approval_shown = true;
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
            Action::AlwaysAllow
        );

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
    use unicode_width::UnicodeWidthStr;

    #[test]
    fn approval_keeps_old_and_new_syntax_states_independent() {
        let mut state = UiState::new("mock");
        state.approval = Some(orbit_tools::approval::ApprovalPreview::Write(
            WritePreview {
                path: "src/Orbit Code.rs".into(),
                before: Some("/* Orbit block\nfn orbit() {}\n*/\n".into()),
                after: "// Orbit update\nfn orbit() {}\n".into(),
            },
        ));
        let mut terminal = Terminal::new(TestBackend::new(100, 25)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let buffer = terminal.backend().buffer();
        let point = (0..25)
            .find_map(|y| {
                let row: String = (0..100).map(|x| buffer[(x, y)].symbol()).collect();
                row.find("fn orbit").map(|x| (row[..x].width() as u16, y))
            })
            .expect("unchanged source line is visible");
        assert_eq!(buffer[point].fg, ratatui::style::Color::Rgb(198, 120, 221));

        let cache = super::super::approval_diff::Cache::new(
            "/* Orbit block\nfn orbit() {}\n*/\n",
            "// Orbit update\nfn orbit() {}\n",
            (50, 50),
            true,
            "src/Orbit Code.rs",
        );
        for (side, expected) in [
            (&cache.left, ratatui::style::Color::Rgb(163, 173, 190)),
            (&cache.right, ratatui::style::Color::Rgb(198, 120, 221)),
        ] {
            let line = side
                .iter()
                .find(|line| line.to_string().contains("fn orbit"))
                .unwrap();
            let keyword = line
                .spans
                .iter()
                .find(|span| span.content.contains("fn"))
                .unwrap();
            assert_eq!(keyword.style.fg, Some(expected));
        }
    }

    #[test]
    fn preview_wheels_scroll_preview_and_outside_wheels_scroll_chat() {
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

        assert_eq!(state.scroll, super::super::wheel::LINES);
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

        assert_eq!(state.approval_choice, 1);
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

    #[test]
    fn command_approval_card_renders_rounded_frame_and_buttons() {
        use crossterm::event::{MouseButton, MouseEvent};
        use orbit_tools::approval::{CommandPreview, CommandWorkspaceAccess};

        let mut state = UiState::new("mock");
        state.approval = Some(orbit_tools::approval::ApprovalPreview::Command(
            CommandPreview {
                command: "mkdir -p demo".into(),
                workspace_access: CommandWorkspaceAccess::ReadWrite,
                network_allowed: true,
                cwd: Some("/home/user/repo".into()),
                timeout_seconds: 30,
                external_capabilities: Vec::new(),
                external_write: false,
                git: None,
            },
        ));

        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal.draw(|f| draw(f, &mut state)).unwrap();

        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();

        let flat = text.replace(' ', "");
        assert!(text.contains("mkdir -p demo"), "{text}");
        assert!(flat.contains("权限确认"), "{text}");
        assert!(flat.contains("[读写]"), "{text}");
        assert!(flat.contains("[联网]"), "{text}");
        assert!(flat.contains("cwd:/home/user/repo"), "{text}");
        assert!(!text.contains("Tab 切换"), "{text}");
        assert!(state.approval_shown);
        assert_eq!(state.approval_options.len(), 3);

        let (opt0_rect, _) = state.approval_options[0];
        let (opt1_rect, _) = state.approval_options[1];
        let (opt2_rect, _) = state.approval_options[2];

        let click = |x, y| {
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: x,
                row: y,
                modifiers: KeyModifiers::NONE,
            })
        };

        assert_eq!(
            handle(&mut state, click(opt0_rect.x + 1, opt0_rect.y)),
            Action::Approve
        );
        assert_eq!(
            handle(&mut state, click(opt1_rect.x + 1, opt1_rect.y)),
            Action::AlwaysAllow
        );
        assert_eq!(
            handle(&mut state, click(opt2_rect.x + 1, opt2_rect.y)),
            Action::Deny
        );
    }

    #[test]
    fn git_dangerous_card_shows_risk_and_badges() {
        use orbit_tools::approval::{CommandPreview, CommandWorkspaceAccess};

        let mut state = UiState::new("mock");
        state.approval = Some(orbit_tools::approval::ApprovalPreview::Command(
            CommandPreview {
                command: "git reset --hard HEAD~1".into(),
                workspace_access: CommandWorkspaceAccess::ReadWrite,
                network_allowed: false,
                cwd: None,
                timeout_seconds: 30,
                external_capabilities: Vec::new(),
                external_write: false,
                git: orbit_runtime::git::GitCommandAnalysis::parse("git reset --hard HEAD~1"),
            },
        ));

        let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
        terminal.draw(|f| draw(f, &mut state)).unwrap();

        let text: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        let flat = text.replace(' ', "");

        assert!(flat.contains("权限确认"), "{text}");
        assert!(flat.contains("高危操作(reset)"), "{text}");
        assert!(text.contains("git reset --hard HEAD~1"), "{text}");
        assert!(flat.contains("[读写]"), "{text}");
        assert!(!flat.contains("[联网]"), "{text}");
        assert!(!text.contains("Tab 切换"), "{text}");
    }
}
