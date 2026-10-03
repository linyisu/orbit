use super::theme::{ansi_color, desaturate_ansi_color, desaturate_for_thinking};
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
use super::icons;

use super::theme::THEME;

const ACCENT: Color = THEME.accent;
const THINKING_MUTED: Color = THEME.thinking_header_default;
const THINKING_BRIGHT: Color = THEME.thinking_header_hover;
const TOOL_FINISHED: Color = THEME.tool_finished;
const TOOL_RUNNING: Color = THEME.tool_running;
const TOOL_FAILED: Color = THEME.tool_failed;
// Use the terminal foreground for readable text on both light and dark themes.
// Reserve subdued colors for borders and decorative artwork.
const MUTED: Color = Color::Reset;
const BORDER: Color = THEME.border;
const MAX_COMMAND_MENU_ITEMS: usize = 8;

pub fn draw(frame: &mut Frame<'_>, state: &mut UiState) {
    let hover = state.pending_list_hover.take();
    draw_contents(frame, state);

    if let Some(mut mouse) = hover {
        // Hit testing must use the rows laid out after scrolling, not the
        // previous frame. Repaint the highlight without moving the viewport.
        mouse.kind = crossterm::event::MouseEventKind::Moved;
        if state.picker.is_some()
            || state.model_picker.is_some()
            || state.marketplace_dialog.is_some()
        {
            super::handle_list_hover(state, &crossterm::event::Event::Mouse(mouse));
        } else if state.menu_area.contains((mouse.column, mouse.row).into()) {
            super::completion::select_with_mouse(state, &mouse);
        }
        draw_contents(frame, state);
    }
    let now = std::time::Instant::now();
    let editing = state.picker.is_none()
        && state.model_picker.is_none()
        && state.marketplace_dialog.is_none()
        && state.restore_picker.is_none()
        && state.approval.is_none()
        && !state.show_help
        && state.input.selection().is_none()
        && !state.selection.active();
    state.cursor_motion.update(
        frame.area(),
        state.input_area,
        state.ime_cursor.filter(|_| editing),
        now,
    );
    if let Some(position) = state.ime_cursor.filter(|p| frame.area().contains(*p)) {
        frame.set_cursor_position(position);
    }
}

fn draw_contents(frame: &mut Frame<'_>, state: &mut UiState) {
    let area = frame.area();
    state.selection.viewport = Rect::default();
    state.selection.footer = Rect::default();
    state.selection.approval = Rect::default();
    state.context_indicator_area = Rect::default();
    state.ime_cursor = None;
    state.input_area = Rect::default();
    state.menu_area = Rect::default();
    state.menu_rows.clear();
    state.dialog_rows.clear();
    state.approval_options.clear();
    state.attachment_areas.clear();
    state.tab_areas.clear();
    state.tab_close_areas.clear();
    state.new_tab_area = Rect::default();
    let home = !state.has_conversation()
        && !state.tabs.is_empty()
        && state.resume_loading.is_none()
        && state.approval.is_none()
        && area.height >= 18;
    let content_width = if home { area.width.min(86) } else { area.width };

    let width = content_width.saturating_sub(6) as usize;
    state.input_width = width;
    let (input_lines, cursor) = state.input.visual_lines(width);
    let input_height = input_lines
        .len()
        .clamp(1, 6)
        .min(area.height.saturating_sub(8) as usize) as u16;
    let attachment_height = u16::from(!state.attachments.is_empty());
    let details_height = composer_details(state, width).len() as u16;
    let input_height = if home {
        input_height.min(
            area.height
                .saturating_sub(attachment_height + details_height + 11),
        )
    } else {
        input_height
    };

    let menu = state.menu();

    // Navigation previews belong to the transcript only. A command menu or
    // dialog is a separate interaction surface, so it must not retain a stale
    // tooltip from a dot the pointer hovered earlier.
    if state.restore_picker.is_some()
        || !menu.is_empty()
        || state.show_help
        || state.picker.is_some()
        || state.model_picker.is_some()
        || state.marketplace_dialog.is_some()
        || state.approval.is_some()
        || state.resume_loading.is_some()
    {
        state.navigation.clear_hover();
        state.tab_hover = None;
        state.context_hovered = false;
        state.link_hovered = None;
        state.transcript_pointer = None;
    }

    let panel_height = if let Some(picker) = &state.restore_picker {
        picker.height()
    } else if !menu.is_empty() {
        menu.len().min(MAX_COMMAND_MENU_ITEMS) as u16
    } else {
        0
    };

    let file_modal = super::approval::file_modal(state);
    let approval_height = if state.approval.is_some() && !file_modal {
        let needed = state
            .approval
            .as_ref()
            .map(super::approval::needed_height)
            .unwrap_or(8);
        (area.height / 2).min(needed)
    } else {
        0
    };

    let mut areas = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(approval_height),
        Constraint::Length(0),
        Constraint::Length(input_height + attachment_height + details_height + 3),
        Constraint::Length(0),
        Constraint::Length(1),
    ])
    .split(area)
    .to_vec();

    let mut home_logo = Rect::default();
    if home {
        let composer_height = input_height + attachment_height + details_height + 3;
        let header_height = u16::from(state.tabs.len() > 1);
        let available_height = area.height.saturating_sub(header_height);
        state
            .transcript_layout
            .update_expanded(&state.transcript, &state.expanded, width as u16);
        let output_height = state
            .transcript_layout
            .height()
            .min(8)
            .min(available_height.saturating_sub(composer_height + 5) as usize)
            as u16;
        let output_gap = u16::from(output_height > 0);
        let logo_height = 7
            .min(available_height.saturating_sub(composer_height + output_height + output_gap + 3));
        let group_height = logo_height + 1 + output_height + output_gap + composer_height + 2;
        let x = area.x + area.width.saturating_sub(content_width) / 2;
        let y = area.y + header_height + available_height.saturating_sub(group_height) / 2;
        areas[0] = if state.tabs.len() > 1 {
            Rect::new(area.x, area.y, area.width, 1)
        } else {
            Rect::default()
        };
        home_logo = Rect::new(x, y, content_width, logo_height);
        areas[1] = Rect::new(x, y + logo_height + 1, content_width, output_height);
        areas[2] = Rect::default();
        areas[4] = Rect::new(
            x,
            areas[1].bottom() + output_gap,
            content_width,
            composer_height,
        );
        areas[5] = Rect::default();
        areas[6] = Rect::new(x, areas[4].bottom(), content_width, 1);
    }

    let menu_height = panel_height.min(areas[4].y.saturating_sub(area.y + 1));
    areas[3] = Rect::new(
        areas[4].x,
        areas[4].y - menu_height,
        areas[4].width,
        menu_height,
    );
    state.menu_area = Rect::new(
        areas[3].x + 2,
        areas[3].y,
        areas[3].width.saturating_sub(4),
        menu_height,
    );

    draw_header(frame, state, areas[0]);
    if home {
        draw_welcome(frame, home_logo);
    }
    if home && areas[1].height == 0 {
        super::navigation::clear_previous_tooltip(frame, state);
        clear_previous_link_tooltip(frame, state);
        state.navigation.reset_area();
    } else {
        draw_transcript(frame, state, areas[1]);
    }

    if state.approval.is_some() && !file_modal {
        super::approval::draw_in(frame, state, areas[2]);
        state.selection.approval = areas[2];
    }

    draw_panel(frame, state, areas[3]);
    draw_composer(frame, state, areas[4], input_lines, cursor);
    draw_status(frame, state, areas[5]);
    let footer = footer_layout(state, areas[6].width as usize);
    state.context_details_visible = footer.context_details_visible;

    if let Some(offset) = footer.context_offset {
        state.context_indicator_area =
            Rect::new(areas[6].x + offset, areas[6].y, footer.context_width, 1);
    }

    frame.render_widget(Paragraph::new(footer.line), areas[6]);
    state.selection.footer = areas[6];
    draw_context_tooltip(frame, state);
    super::dialogs::draw(frame, state);

    if file_modal {
        super::approval::draw_file_modal(frame, state);
    }
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

/// Reserve equal padding on both sides before clipping a control label.
pub(super) fn padded_label(text: &str, width: usize, padding: usize) -> String {
    let padding = padding.min(width.saturating_sub(1) / 2);
    let space = " ".repeat(padding);
    let label = fit_label(text, width.saturating_sub(2 * padding), false);
    format!("{space}{label}{space}")
}

struct FooterLayout {
    line: Line<'static>,
    context_offset: Option<u16>,
    context_width: u16,
    context_details_visible: bool,
}

#[cfg(test)]
fn footer(state: &UiState, width: usize) -> Line<'static> {
    footer_layout(state, width).line
}

const PROGRESS_FRAMES: &[&str] = &[
    "      ",
    "█     ",
    "██    ",
    "███   ",
    "████  ",
    "█████ ",
    "██████",
];

fn footer_layout(state: &UiState, width: usize) -> FooterLayout {
    let available = width.saturating_sub(4);
    let context = format!(
        "{}/{}",
        context_percent_label(state),
        compact_tokens(input_context_budget(state)),
    );
    let context_details_visible = context.width() + 8 <= available;
    let right = if context_details_visible {
        context
    } else {
        context_percent_label(state)
    };

    let right = fit_label(&right, available, false);
    let path_width = available.saturating_sub(right.width() + 2);
    let project = std::path::Path::new(&state.workspace)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&state.workspace);
    let full_path = safe_output(&super::project_label::path_label(
        &state.workspace,
        state.completion_home.as_deref(),
    ));
    let path = if full_path.width() <= path_width {
        full_path
    } else {
        fit_label(&safe_output(project), path_width, false)
    };

    let (left_spans, left_width) = if state.status == RunStatus::Running {
        let ms = state.active_elapsed().as_millis();
        let bar = PROGRESS_FRAMES[(ms / 260) as usize % PROGRESS_FRAMES.len()];

        if available >= 24 {
            (
                vec![
                    Span::styled(
                        bar.to_string(),
                        Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        "  ·  ",
                        Style::default().fg(crate::tui::theme::THEME.border_muted),
                    ),
                    Span::styled(
                        "Ctrl+C",
                        Style::default().fg(crate::tui::theme::THEME.muted_text),
                    ),
                    Span::styled(" 取消", Style::default().fg(crate::tui::theme::THEME.faint)),
                ],
                22,
            )
        } else {
            (
                vec![Span::styled(
                    bar.to_string(),
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
                )],
                6,
            )
        }
    } else {
        let w = path.width();
        (
            vec![Span::styled(
                path,
                Style::default().fg(crate::tui::theme::THEME.secondary),
            )],
            w,
        )
    };

    let gap = available.saturating_sub(left_width + right.width());
    let context_offset = (!right.is_empty()).then_some((2 + left_width + gap) as u16);
    let context_width = right.width() as u16;

    let mut line_spans = vec![Span::raw("  ")];
    line_spans.extend(left_spans);
    line_spans.push(Span::raw(" ".repeat(gap)));
    line_spans.push(Span::styled(
        right,
        Style::default().fg(context_indicator_color(context_percent(state))),
    ));
    line_spans.push(Span::raw("  "));

    FooterLayout {
        line: Line::from(line_spans),
        context_offset,
        context_width,
        context_details_visible,
    }
}

fn composer_details(state: &UiState, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }

    let (provider, model) = state.model.split_once("::").unwrap_or(("", &state.model));
    let accent = Style::default().fg(ACCENT);
    let mut fields = vec![
        Span::styled(
            state.permission_mode.label().to_owned(),
            accent.add_modifier(Modifier::BOLD),
        ),
        Span::raw(safe_output(model)),
    ];

    if !provider.is_empty() {
        fields.push(Span::styled(
            safe_output(provider),
            Style::default().fg(crate::tui::theme::THEME.secondary),
        ));
    }

    let effort = state
        .reasoning_effort
        .map(|effort| effort.label())
        .unwrap_or("—");
    fields.push(Span::styled(effort, accent));

    let mut rows = Vec::new();
    let mut row = Line::default();

    for field in fields {
        let field = Span::styled(fit_label(&field.content, width, false), field.style);

        if !row.spans.is_empty() && row.width() + 2 + field.width() > width {
            rows.push(row);
            row = Line::default();
        }

        if !row.spans.is_empty() {
            row.spans.push(Span::raw("  "));
        }

        row.spans.push(field);
    }

    if !row.spans.is_empty() {
        rows.push(row);
    }

    rows
}

fn compact_tokens(tokens: usize) -> String {
    if tokens >= 1_000_000 {
        let millions = tokens as f64 / 1_000_000.0;

        if millions.fract() == 0.0 {
            format!("{}M", millions as usize)
        } else {
            format!("{millions:.1}M")
        }
    } else if tokens >= 1_000 {
        format!("{}k", tokens / 1_000)
    } else {
        tokens.to_string()
    }
}

fn input_context_budget(state: &UiState) -> usize {
    state.input_context_tokens.max(1)
}

fn context_percent(state: &UiState) -> usize {
    state
        .context_used_tokens
        .saturating_mul(100)
        .checked_div(input_context_budget(state))
        .unwrap_or(0)
}

fn context_percent_label(state: &UiState) -> String {
    if state.resume_loading.is_some() {
        return "—".into();
    }

    let percent = context_percent(state);

    if percent == 0 && state.context_used_tokens > 0 {
        "<1%".into()
    } else {
        format!("{percent}%")
    }
}

fn context_indicator_color(percent: usize) -> Color {
    match percent {
        0..=74 => Color::Reset,
        75..=89 => ACCENT,
        _ => crate::tui::theme::THEME.context_danger,
    }
}

pub(super) fn handle_context_hover(
    state: &mut UiState,
    event: &crossterm::event::Event,
) -> Option<bool> {
    let crossterm::event::Event::Mouse(mouse) = event else {
        return None;
    };
    if mouse.kind != crossterm::event::MouseEventKind::Moved {
        return None;
    }

    let hovered = state
        .context_indicator_area
        .contains((mouse.column, mouse.row).into());
    let changed = state.context_hovered != hovered;
    state.context_hovered = hovered;

    Some(changed)
}

/// Track a link under the pointer independently from selection and navigation.
/// The link target itself stays in the transcript layout so streamed output and
/// resize rebuilds never leave a stale hit target behind.
pub(super) fn handle_link_hover(
    state: &mut UiState,
    event: &crossterm::event::Event,
) -> Option<bool> {
    let crossterm::event::Event::Mouse(mouse) = event else {
        return None;
    };
    if mouse.kind != crossterm::event::MouseEventKind::Moved {
        return None;
    }

    let offset = if state.follow_bottom {
        state.max_scroll
    } else {
        state.max_scroll.saturating_sub(state.scroll)
    };

    let hovered = state
        .transcript_layout
        .link_at(state.selection.viewport, offset, (mouse.column, mouse.row))
        .map(|_| (mouse.column, mouse.row));
    let changed = state.link_hovered != hovered;
    state.link_hovered = hovered;

    Some(changed)
}

pub(super) fn handle_transcript_hover(
    state: &mut UiState,
    event: &crossterm::event::Event,
) -> Option<bool> {
    let crossterm::event::Event::Mouse(mouse) = event else {
        return None;
    };
    if mouse.kind != crossterm::event::MouseEventKind::Moved {
        return None;
    }

    let offset = state.max_scroll.saturating_sub(state.scroll);
    let hit = |point| {
        state.transcript_layout.disclosure_at(
            &state.transcript,
            state.selection.viewport,
            offset,
            point,
        )
    };
    let before = state
        .transcript_pointer
        .and_then(|point| hit(point).map(|index| (index, point.1)));
    let point = (mouse.column, mouse.row);
    let after = hit(point).map(|index| (index, point.1));
    if before != after {
        state.thinking_hover_started = after
            .filter(|(index, _)| {
                matches!(
                    state.transcript.get(*index),
                    Some(TranscriptItem::Thinking(_) | TranscriptItem::Tool(_))
                )
            })
            .map(|_| std::time::Instant::now());
    }
    state.transcript_pointer = Some(point);

    Some(before != after)
}

fn draw_context_tooltip(frame: &mut Frame<'_>, state: &UiState) {
    if !state.context_hovered
        || state.context_details_visible
        || state.context_indicator_area.width == 0
    {
        return;
    }

    let percent = context_percent(state);
    let remaining = 100usize.saturating_sub(if percent == 0 && state.context_used_tokens > 0 {
        1
    } else {
        percent.min(100)
    });
    let lines = [
        if state.calibration().samples() > 0 {
            format!(
                "下一次请求上下文（{} 个样本校准）",
                state.calibration().samples()
            )
        } else {
            "下一次请求上下文（本地估算）".to_owned()
        },
        "包含系统提示、工具定义及当前历史。".to_owned(),
        format!(
            "{} 已用（剩余 {}{remaining}%）",
            context_percent_label(state),
            if percent == 0 && state.context_used_tokens > 0 {
                ">"
            } else {
                ""
            }
        ),
        format!(
            "估算 {} / {} tokens",
            compact_tokens(state.context_used_tokens),
            compact_tokens(input_context_budget(state)),
        ),
        format!(
            "输出上限 {} · 请求预算 {}",
            compact_tokens(state.max_output_tokens as usize),
            compact_tokens(state.context_tokens),
        ),
    ];

    let width = lines
        .iter()
        .map(|line| line.width())
        .max()
        .unwrap_or(0)
        .saturating_add(4)
        .min(frame.area().width as usize) as u16;
    let height = 7;
    let x = state
        .context_indicator_area
        .x
        .saturating_sub(width.saturating_sub(1))
        .min(frame.area().right().saturating_sub(width));
    let y = state.context_indicator_area.y.saturating_sub(height);
    let area = Rect::new(x, y, width, height);
    super::overlay::clear(frame, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(BORDER))
        .style(Style::default().bg(crate::tui::theme::THEME.overlay_background));
    let inner = block.inner(area);

    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new(lines.join("\n")).style(
            Style::default()
                .fg(crate::tui::theme::THEME.overlay_foreground)
                .bg(crate::tui::theme::THEME.overlay_background),
        ),
        inner,
    );
}

fn clear_previous_link_tooltip(frame: &mut Frame<'_>, state: &mut UiState) {
    if state.link_tooltip_area.width != 0 && state.link_tooltip_area.height != 0 {
        super::overlay::clear(frame, state.link_tooltip_area);
    }

    state.link_tooltip_area = Rect::default();
}

fn draw_link_tooltip(frame: &mut Frame<'_>, state: &mut UiState) {
    let Some((anchor_x, anchor_y)) = state.link_hovered else {
        return;
    };
    let offset = state.max_scroll.saturating_sub(state.scroll);
    if state
        .transcript_layout
        .link_at(state.selection.viewport, offset, (anchor_x, anchor_y))
        .is_none()
    {
        state.link_hovered = None;
        return;
    }

    let text = "Ctrl + Click 跳转";
    let width = (text.width() as u16 + 2).min(state.selection.viewport.width);

    if width == 0 {
        return;
    }

    let x = anchor_x.min(state.selection.viewport.right().saturating_sub(width));
    let y = if anchor_y + 1 < state.selection.viewport.bottom() {
        anchor_y + 1
    } else {
        anchor_y.saturating_sub(1)
    };

    let area = Rect::new(x, y, width, 1);
    super::overlay::clear(frame, area);
    frame.render_widget(
        Paragraph::new(fit_label(text, width as usize, false)).style(
            Style::default()
                .fg(crate::tui::theme::THEME.overlay_foreground)
                .bg(crate::tui::theme::THEME.overlay_background),
        ),
        area,
    );
    state.link_tooltip_area = area;
}

pub(super) fn scrolling_title(
    text: &str,
    width: usize,
    elapsed: Option<std::time::Duration>,
) -> String {
    let Some(elapsed) = elapsed.filter(|_| text.width() > width) else {
        return fit_label(text, width, false);
    };

    let graphemes: Vec<&str> = text.graphemes(true).collect();
    let separator = [" ", " ", " ", " "];
    let cycle_items: Vec<&str> = graphemes.iter().copied().chain(separator).collect();

    let step = (elapsed.as_millis().saturating_sub(100) / 120) as usize;
    let start = step % cycle_items.len();
    let mut shown = String::new();
    let mut current_width = 0;

    for &part in cycle_items.iter().cycle().skip(start) {
        let part_w = part.width();
        if current_width + part_w > width {
            break;
        }

        shown.push_str(part);
        current_width += part_w;
    }

    while current_width < width {
        shown.push(' ');
        current_width += 1;
    }

    shown
}

fn tab_control_style(state: &UiState, target: &super::TabAction) -> Style {
    let base = Style::default().fg(ACCENT).remove_modifier(Modifier::BOLD);
    let Some((_, since)) = state
        .tab_hover
        .as_ref()
        .filter(|(action, _)| action == target)
    else {
        return base;
    };

    base.bg(super::theme::tab_hover_background(since.elapsed()))
}

fn draw_header(frame: &mut Frame<'_>, state: &mut UiState, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let titles: Vec<_> = state
        .tabs
        .iter()
        .map(|tab| {
            if tab.title.trim().is_empty() {
                "新会话".to_owned()
            } else {
                safe_output(&tab.title)
            }
        })
        .collect();
    let widths: Vec<_> = titles
        .iter()
        .map(|title| title.width().min(32) + 5)
        .collect();
    let active_index = state.tabs.iter().position(|tab| tab.active).unwrap_or(0);
    let budget = area.width.saturating_sub(8) as usize;
    let mut start = 0;

    while start < active_index
        && widths[start..=active_index]
            .iter()
            .map(|width| width + 1)
            .sum::<usize>()
            > budget
    {
        start += 1;
    }

    let mut x = area.x + 2;

    for (index, tab) in state.tabs.iter().enumerate().skip(start) {
        let available = area.right().saturating_sub(x + 5) as usize;

        if available < 6 {
            break;
        }

        let width = widths[index].min(available) as u16;
        let is_running =
            (tab.active && state.status == RunStatus::Running) || tab.status == RunStatus::Running;
        let spinner_frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
        let spinner_prefix = if tab.waiting_approval {
            "待审批 ".into()
        } else if is_running {
            let idx = (tab.elapsed.as_millis() / 80) as usize % spinner_frames.len();
            format!("{} ", spinner_frames[idx])
        } else {
            String::new()
        };
        let title_width = width.saturating_sub(5 + spinner_prefix.width() as u16) as usize;
        let elapsed = state.tab_hover.as_ref().and_then(|(action, since)| {
            (*action == super::TabAction::Select(tab.id.clone())).then(|| since.elapsed())
        });
        let title = scrolling_title(&titles[index], title_width, elapsed);
        let display_title = format!("{spinner_prefix}{title}");
        let rect = Rect::new(x, area.y, width, 1);
        let style = if tab.active {
            Style::default()
                .fg(ACCENT)
                .bg(crate::tui::theme::THEME.tab_hover_background)
                .add_modifier(Modifier::BOLD)
        } else if elapsed.is_some() {
            Style::default()
                .fg(ACCENT)
                .bg(crate::tui::theme::THEME.tab_background)
        } else if tab.status == RunStatus::Failed {
            Style::default().fg(crate::tui::theme::THEME.tab_error)
        } else {
            Style::default().fg(crate::tui::theme::THEME.tab_inactive)
        };

        frame.render_widget(
            Paragraph::new(format!(
                " {display_title}{}",
                " ".repeat((width as usize).saturating_sub(1 + display_title.width()))
            ))
            .style(style),
            rect,
        );
        let close = Rect::new(rect.right() - 3, rect.y, 3, 1);
        let hovered = state.tab_hover.as_ref().is_some_and(|(action, _)| {
            matches!(action, super::TabAction::Select(id) | super::TabAction::Close(id) if id == &tab.id)
        });

        if hovered {
            frame.render_widget(
                Paragraph::new(format!(" {} ", icons::CLOSE_THICK)).style(style.patch(
                    tab_control_style(state, &super::TabAction::Close(tab.id.clone())),
                )),
                close,
            );
        }

        state.tab_areas.push((rect, tab.id.clone()));
        state.tab_close_areas.push((close, tab.id.clone()));
        x = x.saturating_add(width + 1);
    }

    if area.width >= 5 {
        let plus_x = x.min(area.right().saturating_sub(4));
        state.new_tab_area = Rect::new(plus_x, area.y, 3, 1);
        frame.render_widget(
            Paragraph::new(format!(" {} ", icons::PLUS_THICK))
                .style(tab_control_style(state, &super::TabAction::New)),
            state.new_tab_area,
        );
    }
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
            Style::default().fg(crate::tui::theme::THEME.logo_foreground),
        ))
    })
    .collect()
}

fn welcome() -> Vec<Line<'static>> {
    let rear = Style::default().fg(crate::tui::theme::THEME.logo_shadow);
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

    for (x, y, star) in [
        (7, 1, icons::STAR_FOUR_POINTS),
        (50, 10, icons::STAR_FOUR_POINTS_OUTLINE),
        (53, 1, icons::CIRCLE_SMALL),
        (16, 11, icons::CIRCLE_SMALL),
    ] {
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

fn draw_welcome(frame: &mut Frame<'_>, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

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
}

fn draw_transcript(frame: &mut Frame<'_>, state: &mut UiState, area: Rect) {
    super::navigation::clear_previous_tooltip(frame, state);
    clear_previous_link_tooltip(frame, state);
    state.selection.viewport = Rect {
        x: area.x + 2,
        // Keep a generous blank gap between wrapped output and the navigation dots
        // so double-width CJK characters never collide with or touch the dots.
        width: area.width.saturating_sub(6),
        ..area
    };
    if area.height > 1
        && state
            .transcript
            .iter()
            .any(|item| matches!(item, TranscriptItem::User(_)))
    {
        state.selection.viewport.height -= 1;
    }

    if state.transcript.is_empty() {
        state.navigation.reset_area();

        if state.resume_loading.is_some() {
            return;
        }

        draw_welcome(frame, area);
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
        .update_expanded(&state.transcript, &state.expanded, width);
    state.max_scroll = state
        .transcript_layout
        .height()
        .saturating_sub(state.selection.viewport.height as usize);

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
    if !state.selection.dragging()
        && let Some(point) = state.transcript_pointer
        && let Some(index) = state.transcript_layout.disclosure_at(
            &state.transcript,
            state.selection.viewport,
            offset,
            point,
        )
    {
        match state.transcript.get(index) {
            Some(TranscriptItem::Thinking(_)) => {
                for x in state.selection.viewport.x..state.selection.viewport.right() {
                    let cell = &mut frame.buffer_mut()[(x, point.1)];
                    if cell.symbol() != " " {
                        cell.set_fg(THINKING_BRIGHT);
                        cell.set_style(cell.style().add_modifier(Modifier::BOLD));
                    }
                }
            }
            Some(TranscriptItem::Tool(row)) => {
                let start = state.transcript_layout.item_offset(index).unwrap_or(0);
                let absolute =
                    offset + usize::from(point.1.saturating_sub(state.selection.viewport.y));
                let relative = absolute.saturating_sub(start);
                let expanded = state.expanded.contains(&index);
                let header_lines =
                    tool_header_line_count(row, expanded, state.selection.viewport.width);

                if relative < header_lines {
                    for rel in 0..header_lines {
                        let row_abs = start + rel;
                        if row_abs >= offset {
                            let y = state.selection.viewport.y + (row_abs - offset) as u16;
                            if y < state.selection.viewport.bottom() {
                                for x in
                                    state.selection.viewport.x..state.selection.viewport.right()
                                {
                                    let cell = &mut frame.buffer_mut()[(x, y)];
                                    if cell.symbol() != " " {
                                        cell.set_style(cell.style().add_modifier(Modifier::BOLD));
                                        if cell.fg == TOOL_FINISHED {
                                            cell.set_fg(crate::tui::theme::THEME.tool_hover);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    super::navigation::draw(frame, state, area, offset);
    draw_link_tooltip(frame, state);
}

#[cfg(test)]
pub(super) fn transcript_lines(items: &[TranscriptItem], expanded: bool) -> Vec<Line<'static>> {
    transcript_lines_at_width(items, expanded, u16::MAX)
}

pub(super) fn transcript_lines_at_width(
    items: &[TranscriptItem],
    expanded: bool,
    width: u16,
) -> Vec<Line<'static>> {
    let mut lines = Vec::new();

    for (index, item) in items.iter().enumerate() {
        match item {
            TranscriptItem::User(text) => {
                let text = text.trim();
                for (index, line) in text.lines().enumerate() {
                    lines.push(Line::from(vec![
                        Span::styled(
                            if index == 0 {
                                format!("{} ", icons::CHEVRON_RIGHT)
                            } else {
                                "  ".into()
                            },
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
                let text = text.trim();
                if text.is_empty() {
                    continue;
                }

                if lines
                    .last()
                    .is_some_and(|line| !line.to_string().trim().is_empty())
                {
                    lines.push(Line::from(""));
                }

                let mut code = None;
                let mut prev_empty = false;

                for (index, line) in text.lines().enumerate() {
                    let is_empty = line.trim().is_empty();
                    if is_empty && prev_empty {
                        continue;
                    }
                    prev_empty = is_empty;
                    lines.push(assistant_line(line, index, &mut code));
                }

                lines.push(Line::from(""));
            }

            TranscriptItem::Thinking(row) => {
                if row.finished {
                    let label = row.elapsed.map_or_else(
                        || "思考".to_owned(),
                        |elapsed| {
                            if elapsed.as_secs_f64() < 0.1 {
                                "思考 <0.1s".into()
                            } else {
                                format!("思考 {:.1}s", elapsed.as_secs_f64())
                            }
                        },
                    );
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(label, Style::default().fg(THINKING_MUTED)),
                    ]));

                    if !expanded {
                        lines.push(Line::from(""));
                        continue;
                    }
                }

                let text = row.content.trim_end();
                let mut code = None;

                for (index, line) in text.lines().enumerate() {
                    lines.push(thinking_line(line, index, &mut code));
                }

                if !text.is_empty() {
                    lines.push(Line::from(""));
                }
            }

            TranscriptItem::Tool(row) => {
                let prev_is_tool =
                    index > 0 && matches!(items.get(index - 1), Some(TranscriptItem::Tool(_)));
                if !prev_is_tool
                    && lines
                        .last()
                        .is_some_and(|line| !line.to_string().trim().is_empty())
                {
                    lines.push(Line::from(""));
                }

                lines.extend(tool_card(row, expanded, width));

                let next_is_tool = matches!(items.get(index + 1), Some(TranscriptItem::Tool(_)));
                if !next_is_tool {
                    lines.push(Line::from(""));
                }
            }

            TranscriptItem::CommandResult(text) => {
                for line in text.lines() {
                    lines.push(Line::from(Span::styled(
                        format!("  {line}"),
                        Style::default().fg(crate::tui::theme::THEME.detail),
                    )));
                }

                lines.push(Line::from(""));
            }

            TranscriptItem::CommandError(text) => {
                for (index, line) in text.lines().enumerate() {
                    lines.push(Line::from(Span::styled(
                        format!("{} {line}", if index == 0 { icons::CIRCLE } else { " " }),
                        Style::default().fg(ACCENT),
                    )));
                }

                lines.push(Line::from(""));
            }

            TranscriptItem::Maintenance(text) => {
                for line in text.lines() {
                    lines.push(
                        Line::from(Span::styled(
                            format!("── {line} ──"),
                            Style::default().fg(crate::tui::theme::THEME.output_muted),
                        ))
                        .alignment(Alignment::Center),
                    );
                }

                lines.push(Line::from(""));
            }

            TranscriptItem::Error(text) | TranscriptItem::Notice(text) => {
                let error = matches!(item, TranscriptItem::Error(_));

                for line in text.lines() {
                    lines.push(Line::from(Span::styled(
                        format!(
                            "{} {line}",
                            if error {
                                icons::CLOSE
                            } else {
                                icons::CHEVRON_RIGHT
                            }
                        ),
                        Style::default().fg(if error {
                            crate::tui::theme::THEME.error
                        } else {
                            MUTED
                        }),
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

pub(super) fn parse_ansi_to_spans(text: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut current_style = Style::default();
    let mut chars = text.chars().peekable();
    let mut current_chunk = String::new();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    let mut seq = String::new();
                    while let Some(&c) = chars.peek() {
                        chars.next();
                        if c.is_ascii_alphabetic() {
                            seq.push(c);
                            break;
                        }
                        seq.push(c);
                    }
                    if seq.ends_with('m') {
                        if !current_chunk.is_empty() {
                            spans.push(Span::styled(current_chunk.clone(), current_style));
                            current_chunk.clear();
                        }
                        let code_part = &seq[..seq.len() - 1];
                        if code_part.is_empty() || code_part == "0" {
                            current_style = Style::default();
                        } else {
                            for code in code_part.split(';') {
                                match code.trim().parse::<u16>() {
                                    Ok(0) => current_style = Style::default(),
                                    Ok(1) => {
                                        current_style = current_style.add_modifier(Modifier::BOLD)
                                    }
                                    Ok(2) => {
                                        current_style = current_style.add_modifier(Modifier::DIM)
                                    }
                                    Ok(3) => {
                                        current_style = current_style.add_modifier(Modifier::ITALIC)
                                    }
                                    Ok(4) => {
                                        current_style =
                                            current_style.add_modifier(Modifier::UNDERLINED)
                                    }
                                    Ok(39) => current_style = current_style.fg(Color::Reset),
                                    Ok(c) => {
                                        if let Some(color) = ansi_color(c) {
                                            current_style =
                                                current_style.fg(desaturate_ansi_color(color));
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    continue;
                }
                Some('(' | ')' | '*' | '+') => {
                    chars.next();
                    chars.next();
                    continue;
                }
                Some(']') => {
                    chars.next();
                    while let Some(&c) = chars.peek() {
                        chars.next();
                        if c == '\x07' || c == '\x1b' {
                            if c == '\x1b' && chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                    continue;
                }
                Some(_) => {
                    chars.next();
                    continue;
                }
                None => continue,
            }
        }

        if ch == '\t' {
            current_chunk.push_str("    ");
        } else if ch != '\r' {
            current_chunk.push(ch);
        }
    }

    if !current_chunk.is_empty() {
        spans.push(Span::styled(current_chunk, current_style));
    }

    if spans.is_empty() {
        vec![Span::raw("")]
    } else {
        spans
    }
}

pub(super) fn read_file_summary(arguments: &str, output: &str) -> String {
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

fn tool_details(
    row: &super::app::ToolRow,
    expanded: bool,
    width: u16,
) -> Vec<super::markdown::LinkedLine> {
    use super::markdown::LinkedLine;
    let mut lines = Vec::new();
    let (marker, color) = match row.status {
        ToolStatus::Running => (icons::LOADING, TOOL_RUNNING),
        ToolStatus::Finished => (icons::CHECK, TOOL_FINISHED),
        ToolStatus::Failed => (icons::CLOSE, TOOL_FAILED),
    };

    let summary = super::tool_display::tool_summary(&row.name, &row.arguments, &row.output);

    lines.push(Line::from(vec![
        Span::styled(format!("  {marker} "), Style::default().fg(color)),
        Span::styled(
            super::tool_display::clipped(
                &safe_output(&format!("{} {summary}", row.name)),
                usize::from(width).saturating_sub(4),
            ),
            Style::default().fg(crate::tui::theme::THEME.output_foreground),
        ),
    ]));

    if let Some(output) = super::tool_display::linked_lines(row, expanded, width) {
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
                    if let Some(output_id) = value["output_id"].as_str() {
                        if !text.is_empty() && !text.ends_with('\n') {
                            text.push('\n');
                        }
                        text.push_str(&format!(
                            "[完整输出已留存: {output_id} · 可使用 read_command_output 查询]"
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
        let mut highlighter = super::code::Highlighter::new("json");
        for line in output.lines() {
            lines.push(highlighter.line(line));
        }
    } else {
        for line in output.lines() {
            lines.push(Line::from(parse_ansi_to_spans(line)));
        }
    }

    lines.push(super::tool_display::toggle_line(true, width));

    lines.push(Line::from(""));
    lines.into_iter().map(LinkedLine::plain).collect()
}

pub(super) fn tool_card(
    row: &super::app::ToolRow,
    expanded: bool,
    width: u16,
) -> Vec<Line<'static>> {
    tool_card_with_links(row, expanded, width)
        .into_iter()
        .map(|line| line.line)
        .collect()
}

pub(super) fn tool_card_with_links(
    row: &super::app::ToolRow,
    expanded: bool,
    width: u16,
) -> Vec<super::markdown::LinkedLine> {
    use super::markdown::LinkedLine;
    let width = if width == u16::MAX { 120 } else { width };
    let color = match row.status {
        ToolStatus::Running => TOOL_RUNNING,
        ToolStatus::Finished => TOOL_FINISHED,
        ToolStatus::Failed => TOOL_FAILED,
    };
    if width < 10 {
        return vec![LinkedLine::plain(Line::styled(
            super::tool_display::clipped(&row.name, width as usize),
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
    let command = if !expanded {
        super::tool_display::tool_summary(&row.name, &row.arguments, &row.output)
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
                Style::default().fg(crate::tui::theme::THEME.tool_summary),
            ),
        ]
    };

    // Collapsed state: strictly 1 command line with colored command name and dimmed action summary
    if !expanded {
        let mut full_title_spans = vec![Span::raw("  ")];
        full_title_spans.extend(title_spans);
        let title_line = Line::from(full_title_spans);
        let title_width = width.saturating_sub(2);
        let first_line = LinkedLine::plain(title_line)
            .wrap(title_width)
            .into_iter()
            .next()
            .unwrap();
        return vec![first_line];
    }

    // Expanded state: rounded enclosed card flush with user input frame
    if width < 4 {
        return vec![LinkedLine::plain(Line::from(title_spans))];
    }

    let border_style = Style::default().fg(crate::tui::theme::THEME.tool_border);
    let inner_width = usize::from(width).saturating_sub(4);

    let mut card_lines = Vec::new();

    card_lines.push(LinkedLine::plain(Line::from(Span::styled(
        format!("╭{}╮", "─".repeat(inner_width + 2)),
        border_style,
    ))));

    let header_line = Line::from(title_spans);
    for h_line in LinkedLine::plain(header_line).wrap(inner_width as u16) {
        let h_pad = inner_width.saturating_sub(h_line.line.width());
        let mut framed_header = vec![Span::styled("│ ", border_style)];
        let offset = framed_header[0].content.len();
        let links = h_line
            .links
            .into_iter()
            .map(|mut link| {
                link.start += offset;
                link.end += offset;
                link
            })
            .collect();
        framed_header.extend(h_line.line.spans);
        framed_header.push(Span::raw(" ".repeat(h_pad)));
        framed_header.push(Span::styled(" │", border_style));
        card_lines.push(LinkedLine {
            line: Line::from(framed_header),
            links,
        });
    }

    card_lines.push(LinkedLine::plain(Line::from(vec![
        Span::styled("│ ", border_style),
        Span::raw(" ".repeat(inner_width)),
        Span::styled(" │", border_style),
    ])));

    let mut details = tool_details(row, true, inner_width as u16);
    if !details.is_empty() {
        details.remove(0);
    }
    let toggle = super::tool_display::toggle_line(true, inner_width as u16);
    let collapse = super::tool_display::toggle_line(false, inner_width as u16);
    details.retain(|linked| linked.line != toggle && linked.line != collapse);
    while details
        .last()
        .is_some_and(|linked| linked.line.to_string().trim().is_empty())
    {
        details.pop();
    }

    for mut linked in details {
        let line = &mut linked.line;
        if line.to_string().starts_with("    ") {
            let mut remaining = 4;
            for span in &mut line.spans {
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
            linked.links.retain_mut(|link| {
                if link.end <= 4 {
                    return false;
                }
                link.start = link.start.saturating_sub(4);
                link.end -= 4;
                true
            });
        }
        for span in &mut line.spans {
            span.content = safe_output(&span.content).into();
            span.style = line.style.patch(span.style);
            if span.style.fg == Some(Color::Reset) || span.style.fg.is_none() {
                span.style = span.style.fg(crate::tui::theme::THEME.output_muted);
            }
        }
        line.spans.retain(|span| !span.content.is_empty());
        super::markdown::expand_rule(line, inner_width as u16);

        for sub in linked.wrap(inner_width as u16) {
            let pad = inner_width.saturating_sub(sub.line.width());
            let line_bg = sub
                .line
                .spans
                .first()
                .and_then(|s| s.style.bg)
                .filter(|bg| sub.line.spans.iter().all(|s| s.style.bg == Some(*bg)));

            let mut spans = if let Some(bg) = line_bg {
                vec![
                    Span::styled("│", border_style),
                    Span::styled(" ", Style::default().bg(bg)),
                ]
            } else {
                vec![Span::styled("│ ", border_style)]
            };
            let offset = spans.iter().map(|span| span.content.len()).sum::<usize>();
            let links = sub
                .links
                .into_iter()
                .map(|mut link| {
                    link.start += offset;
                    link.end += offset;
                    link
                })
                .collect();
            spans.extend(sub.line.spans);
            if let Some(bg) = line_bg {
                spans.push(Span::styled(" ".repeat(pad), Style::default().bg(bg)));
                spans.push(Span::styled(" ", Style::default().bg(bg)));
                spans.push(Span::styled("│", border_style));
            } else {
                spans.push(Span::raw(" ".repeat(pad)));
                spans.push(Span::styled(" │", border_style));
            }
            card_lines.push(LinkedLine {
                line: Line::from(spans),
                links,
            });
        }
    }

    card_lines.push(LinkedLine::plain(Line::from(Span::styled(
        format!("╰{}╯", "─".repeat(inner_width + 2)),
        border_style,
    ))));

    card_lines
}

#[cfg(test)]
pub(super) use default_tool_summary as tool_summary;

pub(super) fn default_tool_summary(arguments: &str) -> String {
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

fn tool_header_line_count(row: &super::app::ToolRow, expanded: bool, width: u16) -> usize {
    let _ = (row, width);
    if !expanded { 1 } else { 2 }
}

pub(super) fn is_shell_preamble(line: &str) -> bool {
    let compact = line.split_whitespace().collect::<String>();
    compact.starts_with("set-")
        && compact[4..]
            .chars()
            .all(|ch| ch.is_ascii_alphabetic() || ch == 'o')
}

pub(super) fn assistant_line(
    line: &str,
    index: usize,
    code: &mut super::markdown::Fence,
) -> Line<'static> {
    super::markdown::line(line, index, code)
}

pub(super) fn thinking_line(
    line: &str,
    index: usize,
    code: &mut super::markdown::Fence,
) -> Line<'static> {
    let mut rendered = super::markdown::line(line, index, code);
    // `markdown::line` reserves two columns for ordinary transcript content.
    // Replace that indentation with a muted thinking rail while retaining the
    // markdown spans behind it.
    let mut has_thinking_rail = false;

    if let Some(first) = rendered.spans.first_mut() {
        // Keep both rails for code inside a thought: `┊` identifies reasoning
        // and `│` identifies the Markdown code block. They are one compound
        // gutter so wrapped code lines reserve their combined width.
        if first.content == "  │ " {
            first.content = "  ┊ │ ".into();
            has_thinking_rail = true;
        } else if let Some(rest) = first.content.strip_prefix("  ") {
            if rest.is_empty() {
                rendered.spans.remove(0);
            } else {
                first.content = rest.to_owned().into();
            }
        }
    }

    // Markdown still contributes structure and semantic color. Apply only a
    // uniform 30% neutral wash across prose, Markdown and syntax colors.
    for span in &mut rendered.spans {
        span.style = span.style.fg(desaturate_for_thinking(span.style.fg));
        if let Some(background) = span.style.bg {
            span.style = span.style.bg(desaturate_for_thinking(Some(background)));
        }
    }

    if !has_thinking_rail {
        rendered.spans.insert(
            0,
            Span::styled(
                "  ┊ ",
                Style::default().fg(crate::tui::theme::THEME.thinking_rail),
            ),
        );
    }

    rendered
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
    if let Some(message) = &state.resume_loading {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("  {message}"),
                Style::default().fg(ACCENT),
            ))),
            area,
        );
        return;
    }

    let elapsed = state.active_elapsed();
    let text = match state.status {
        RunStatus::Idle | RunStatus::Completed | RunStatus::Cancelled => String::new(),
        RunStatus::Running if state.approval.is_some() => "  等待审批".into(),
        RunStatus::Running => {
            let ms = elapsed.as_millis();
            let bar = PROGRESS_FRAMES[(ms / 260) as usize % PROGRESS_FRAMES.len()];
            format!("  {}  ·  Ctrl+C 取消", bar)
        }
        RunStatus::Cancelling => format!("  {} 正在取消…", icons::LOADING),
        RunStatus::Failed => format!("  {} 操作未完成 · 请查看上方错误", icons::CLOSE),
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
        .border_style(Style::default().fg(ACCENT));
    let inner = block.inner(area);

    frame.render_widget(block, area);
    let details = composer_details(state, inner.width.saturating_sub(2) as usize);
    let details_height = (details.len() as u16).min(inner.height.saturating_sub(1));

    if details_height > 0 {
        frame.render_widget(
            Paragraph::new(details),
            Rect::new(
                inner.x + 2,
                inner.bottom() - details_height,
                inner.width.saturating_sub(2),
                details_height,
            ),
        );
    }

    let inner = Rect {
        height: inner.height.saturating_sub(1 + details_height),
        ..inner
    };
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
                    .fg(crate::tui::theme::THEME.on_accent)
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
        Paragraph::new(Span::styled(
            icons::CHEVRON_RIGHT,
            Style::default().fg(ACCENT),
        )),
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
    // The right padding also holds the caret before a newline after a full row.
    state.input_area = Rect {
        width: (text_area.width + 1).min(frame.area().right().saturating_sub(text_area.x)),
        ..text_area
    };
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

    for (row, columns) in state.input.selection_cells(state.input_width) {
        if row >= scroll && row < scroll + text_area.height as usize {
            let y = text_area.y + (row - scroll) as u16;
            for column in columns {
                if column >= state.input_area.width as usize {
                    break;
                }
                let cell = &mut frame.buffer_mut()[(text_area.x + column as u16, y)];
                cell.set_style(super::theme::selection_style(cell.style()));
            }
        }
    }
    if text_area.width > 0
        && text_area.height > 0
        && state.picker.is_none()
        && state.model_picker.is_none()
        && state.marketplace_dialog.is_none()
        && state.approval.is_none()
        && !state.show_help
        && state.attachment_cursor.is_none()
    {
        let x = text_area.x + (cursor.0 as u16).min(state.input_area.width - 1);
        let y = text_area.y + (cursor.1.saturating_sub(scroll) as u16).min(text_area.height - 1);
        state.ime_cursor = Some((x, y).into());
    }
}

fn draw_panel(frame: &mut Frame<'_>, state: &mut UiState, area: Rect) {
    if let Some(picker) = &mut state.restore_picker {
        picker.draw(frame, area);
        return;
    }
    let menu = state.menu();

    if menu.is_empty() || area.height == 0 || area.width <= 4 {
        return;
    }

    let area = Rect {
        x: area.x + 2,
        width: area.width - 4,
        ..area
    };
    let background = Style::default();
    let visible = area.height as usize;
    // Keep keyboard focus near the middle while moving through a long menu.
    let max_first = menu.len().saturating_sub(visible);
    if !state.menu_manual_scroll {
        state.menu_offset = state
            .menu_offset
            .min(state.menu_index)
            .max(state.menu_index.saturating_sub(visible.saturating_sub(1)))
            .min(max_first);
    }
    state.menu_offset = state.menu_offset.min(max_first);
    let first = state.menu_offset;

    super::overlay::clear(frame, area);
    frame.render_widget(Block::default().style(background), area);

    for (row, (index, command)) in menu
        .iter()
        .enumerate()
        .skip(first)
        .take(visible)
        .enumerate()
    {
        let selected = index == state.menu_index.min(menu.len() - 1);
        let style = if selected {
            Style::default()
                .fg(crate::tui::theme::THEME.on_accent)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD)
        } else {
            background.fg(MUTED)
        };
        let text = format!(
            "{:<width$} {}",
            safe_output(command.display.as_deref().unwrap_or(&command.name)),
            safe_output(&command.description),
            width = super::commands::COMMAND_NAME_WIDTH,
        );

        let text = padded_label(text.trim_end(), area.width as usize, 3);
        let width =
            unicode_width::UnicodeWidthStr::width(text.as_str()).min(area.width as usize) as u16;
        state
            .menu_rows
            .push((Rect::new(area.x, area.y + row as u16, width, 1), index));
        frame.render_widget(
            Paragraph::new(text).style(style),
            Rect::new(area.x, area.y + row as u16, width, 1),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::app::SessionTab;
    use ratatui::{Terminal, backend::TestBackend};

    #[test]
    fn tools_expand_independently_and_commands_expand_in_full() {
        let mut state = UiState::new("mock");
        for i in 0..2 {
            state
                .transcript
                .push(TranscriptItem::Tool(super::super::app::ToolRow {
                id: i.to_string(),
                name: "run_command".into(),
                status: ToolStatus::Finished,
                arguments:
                    serde_json::json!({ "command": format!("echo {} end-{i}", "long ".repeat(30)) })
                        .to_string(),
                output: "one\ntwo\nthree\nfour".into(),
            }));
        }
        render(&mut state, 100, 40);

        super::super::toggle_transcript_item(&mut state, 0);
        super::super::toggle_transcript_item(&mut state, 1);
        assert_eq!(state.expanded, [0, 1].into_iter().collect());
        let TranscriptItem::Tool(row) = &state.transcript[0] else {
            unreachable!()
        };
        let full = tool_card(row, true, 80)
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(full.contains("run_command"));
        assert!(full.contains('╭') && full.contains('╰'));
        assert!(full.contains("end-0"));
        assert_eq!(full.matches("end-0").count(), 1);

        super::super::toggle_transcript_item(&mut state, 0);
        assert_eq!(state.expanded, [1].into_iter().collect());
    }

    #[test]
    fn parse_ansi_to_spans_strips_charset_and_preserves_clean_text() {
        let raw = "\x1b[31m-\x1b(B\x1b[m\x1b[32m+    let cell = &mut frame;\x1b(B\x1b[m";
        let spans = parse_ansi_to_spans(raw);
        let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(!joined.contains("\u{1b}"));
        assert!(!joined.contains("(B"));
        assert_eq!(joined, "-+    let cell = &mut frame;");
    }

    #[test]
    fn completed_thinking_drops_trailing_blank_rows_without_changing_content() {
        let mut row: super::super::app::ThinkingRow = "thought\n\n  \n".to_owned().into();
        row.finished = true;
        row.elapsed = Some(std::time::Duration::ZERO);
        let lines = transcript_lines(&[TranscriptItem::Thinking(row.clone())], true);

        assert!(lines[0].to_string().contains("<0.1s"));
        assert_eq!(lines.len(), 3);
        assert_eq!(row.content, "thought\n\n  \n");
    }

    #[test]
    fn completed_reasoning_collapses_and_header_hover_and_click_reveal_it() {
        use crossterm::event::{Event, KeyModifiers, MouseEvent, MouseEventKind};
        use orbit_agent::events::AgentEvent;
        use orbit_model::types::StreamEvent;

        let mut state = UiState::new("model");
        state.apply(AgentEvent::Model(StreamEvent::ReasoningDelta(
            "private working text".into(),
        )));
        assert!(render(&mut state, 90, 30).contains("private working text"));

        state.apply(AgentEvent::Model(StreamEvent::TextDelta(
            "final response".into(),
        )));
        let TranscriptItem::Thinking(row) = &mut state.transcript[0] else {
            unreachable!()
        };
        row.elapsed = Some(std::time::Duration::from_millis(7500));
        let collapsed = render(&mut state, 90, 30);
        assert!(collapsed.contains("7.5s"), "{collapsed}");
        assert!(
            transcript_lines(&state.transcript[..1], false)[0]
                .to_string()
                .contains("思考 7.5s")
        );
        assert!(!collapsed.contains("private working text"));
        assert!(collapsed.contains("final response"));

        let area = state.selection.viewport;
        let offset = state.max_scroll.saturating_sub(state.scroll);
        let point = (area.y..area.bottom())
            .find_map(|y| {
                (area.x..area.right()).find_map(|x| {
                    (state
                        .transcript_layout
                        .disclosure_at(&state.transcript, area, offset, (x, y))
                        == Some(0))
                    .then_some((x, y))
                })
            })
            .expect("thought header is clickable");
        let moved = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: point.0,
            row: point.1,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(handle_transcript_hover(&mut state, &moved), Some(true));
        state.thinking_hover_started =
            Some(std::time::Instant::now() - std::time::Duration::from_millis(150));
        let mut terminal = Terminal::new(TestBackend::new(90, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        assert!(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .any(|cell| cell.symbol() == "思" && cell.fg == THINKING_BRIGHT)
        );

        assert!(super::super::toggle_transcript_at(&mut state, point));
        assert!(render(&mut state, 90, 30).contains("private working text"));
        assert!(super::super::toggle_transcript_at(&mut state, point));
        assert!(!render(&mut state, 90, 30).contains("private working text"));

        let leave = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(handle_transcript_hover(&mut state, &leave), Some(true));
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        assert!(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .any(|cell| cell.symbol() == "思" && cell.fg == THINKING_MUTED)
        );
    }

    #[test]
    fn collapsed_tool_cards_fit_width_and_bound_the_preview() {
        use super::super::app::ToolRow;

        for status in [
            ToolStatus::Running,
            ToolStatus::Finished,
            ToolStatus::Failed,
        ] {
            let item = TranscriptItem::Tool(ToolRow {
                id: "call".into(),
                name: "run_command".into(),
                status,
                arguments:
                    serde_json::json!({ "command": format!("中文\t{}", "command ".repeat(50)) })
                        .to_string(),
                output: "hidden result".into(),
            });

            for width in [1, 8, 24, 80] {
                let lines = transcript_lines_at_width(std::slice::from_ref(&item), false, width);
                assert!(lines.len() <= 9);
                assert!(
                    lines.iter().all(|line| line.width() <= usize::from(width)),
                    "{lines:?}"
                );
                assert!(lines.last().unwrap().to_string().is_empty());
            }
        }
    }

    #[test]
    fn hovered_title_scrolls_without_splitting_wide_or_combining_characters() {
        let title = "中文e\u{301}标题长到需要滚动显示最后内容";
        let initial = scrolling_title(title, 12, None);

        assert!(initial.ends_with('…'));

        let frames: Vec<_> = (0..50)
            .map(|tick| {
                scrolling_title(
                    title,
                    12,
                    Some(std::time::Duration::from_millis(tick * 120)),
                )
            })
            .collect();

        assert!(frames.iter().all(|frame| frame.width() <= 12));
        assert!(frames.iter().any(|frame| frame.ends_with("最后内容")));
        assert!(frames.iter().all(|frame| !frame.starts_with('\u{301}')));
    }

    #[test]
    fn new_tab_button_changes_on_hover_and_restores_on_leave() {
        let mut state = UiState::new("model");
        let plain = tab_control_style(&state, &super::super::TabAction::New);
        state.tab_hover = Some((super::super::TabAction::New, std::time::Instant::now()));

        assert_ne!(
            tab_control_style(&state, &super::super::TabAction::New),
            plain
        );

        state.tab_hover = None;

        assert_eq!(
            tab_control_style(&state, &super::super::TabAction::New),
            plain
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn editor_returns_to_home_until_the_draft_is_sent() {
        let directory = tempfile::tempdir().unwrap();
        let editor = super::super::editor::Editor::new("/bin/true".into());
        let mut state = UiState::new("model");
        state.tabs.push(SessionTab {
            waiting_approval: false,
            elapsed: std::time::Duration::ZERO,
            id: "new".into(),
            title: "新会话".into(),
            status: RunStatus::Idle,
            active: true,
        });
        state.input.set("/editor".into());
        state.input.take();
        let text = editor
            .edit("尚未发送的草稿\n第二行", directory.path())
            .await
            .unwrap();

        state.restore_editor_draft(text);
        let screen = render(&mut state, 140, 40);

        assert!(state.transcript.is_empty());
        assert_eq!(state.status, RunStatus::Idle);
        assert!(state.input_area.x > 20);
        assert!(state.input_area.y < 30);
        assert!(screen.replace(' ', "").contains("尚未发送的草稿"));
        assert_eq!(state.tabs.len(), 1);

        let prompt = state.input.take().unwrap();
        state.submit(prompt);
        render(&mut state, 140, 40);
        assert!(state.input_area.x < 20);
        assert!(!state.transcript.is_empty());
    }

    #[test]
    fn home_centers_composer_and_conversation_exposes_new_session_button() {
        let mut state = UiState::new("provider::model");
        state.tabs.push(SessionTab {
            waiting_approval: false,
            elapsed: std::time::Duration::ZERO,
            id: "first".into(),
            title: "New session".into(),
            status: RunStatus::Idle,
            active: true,
        });

        render(&mut state, 140, 40);
        let home_input = state.input_area;

        assert!(home_input.x > 20);
        assert!(home_input.y > 10 && home_input.y < 30);
        assert!(state.tab_areas.is_empty());
        assert_eq!(state.new_tab_area, Rect::default());

        state.transcript.push(TranscriptItem::User("Hello".into()));
        render(&mut state, 140, 40);

        assert!(state.input_area.y > home_input.y);
        assert!(state.input_area.width > home_input.width);
        assert_eq!(state.tab_areas.len(), 1);
        assert_eq!(state.new_tab_area.height, 1);
        assert!(state.new_tab_area.x >= state.tab_areas[0].0.right());
        assert!(!render(&mut state, 140, 40).contains(icons::CLOSE_THICK));

        state.tab_hover = Some((
            super::super::TabAction::Select("first".into()),
            std::time::Instant::now(),
        ));

        assert!(render(&mut state, 140, 40).contains(icons::CLOSE_THICK));

        state.tab_hover = None;

        assert!(!render(&mut state, 140, 40).contains(icons::CLOSE_THICK));
    }

    #[tokio::test]
    async fn commands_keep_home_and_their_output_scrollable_until_a_message_is_sent() {
        let directory = tempfile::tempdir().unwrap();
        let mut session = crate::session::SessionState::open_in(
            directory.path().join("orbit.db"),
            None,
            "orbit-model".into(),
            directory.path(),
        )
        .await
        .unwrap();
        let mut state = UiState::new("orbit-model");
        state.tabs.push(SessionTab {
            waiting_approval: false,
            elapsed: std::time::Duration::ZERO,
            id: session.id().into(),
            title: "Orbit session".into(),
            status: RunStatus::Idle,
            active: true,
        });
        state
            .transcript
            .push(TranscriptItem::CommandResult("Orbit 命令输出".into()));
        let screen = render(&mut state, 140, 40);
        assert!(screen.replace(' ', "").contains("Orbit命令输出"));
        assert!(state.input_area.x > 20);
        let mut model = orbit_model::client::ModelClient::new(Default::default()).unwrap();
        let mut history = Vec::new();
        let tools = orbit_agent::tool::ToolRegistry::new();
        let instructions = crate::instructions::Instructions::default();
        let skills = crate::skills::Skills::default();
        for command in [
            "/mode ask",
            "/status",
            "/context",
            "/skills",
            "/orbit-unknown",
        ] {
            super::super::run_command(
                command,
                &mut state,
                &mut model,
                &mut history,
                &mut session,
                super::super::CommandContext {
                    tools: &tools,
                    instructions: &instructions,
                    skills: &skills,
                    mcp_statuses: &[],
                    model_options: &[],
                },
            )
            .await
            .unwrap();
            render(&mut state, 140, 40);
            assert!(!state.has_conversation(), "{command}");
            assert!(state.input_area.x > 20, "{command}");
            assert_eq!(state.status, RunStatus::Idle);
            assert!(state.selection.viewport.height <= 8);
        }
        assert!(state.max_scroll > 0);
        state.scroll_up(state.max_scroll);
        let screen = render(&mut state, 140, 40);
        assert!(screen.replace(' ', "").contains("Orbit命令输出"));
        state.scroll_down(state.max_scroll);
        let screen = render(&mut state, 140, 40);
        assert!(screen.replace(' ', "").contains("未知命令"));
        render(&mut state, 28, 18);
        assert!(state.selection.viewport.height > 0);
        assert!(state.input_area.bottom() < 18);
        state.submit("Orbit message".into());
        render(&mut state, 140, 40);
        assert!(state.has_conversation());
        assert!(state.input_area.x < 20);
        assert_eq!(state.new_tab_area.height, 1);
    }

    #[test]
    fn composer_metadata_is_below_input_without_a_background_or_sidebar() {
        for width in [30, 140] {
            let mut state = UiState::new("provider::test-model");
            state.reasoning_effort = Some(orbit_model::types::ReasoningEffort::High);
            state.input.set("draft".into());
            let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();

            terminal.draw(|frame| draw(frame, &mut state)).unwrap();

            let buffer = terminal.backend().buffer();
            let metadata: String = (state.input_area.bottom()..22)
                .flat_map(|y| (0..width).map(move |x| buffer[(x, y)].symbol()))
                .collect();

            for label in ["auto", "test-model", "provider", "high"] {
                assert!(metadata.contains(label), "{width}: {metadata:?}");
            }

            assert!(state.selection.viewport.right() > width - 10);
            assert!(buffer.content.iter().all(|cell| {
                cell.bg == Color::Reset || cell.bg == crate::tui::theme::THEME.caret
            }));
            assert!(
                composer_details(&state, width.saturating_sub(7) as usize)
                    .iter()
                    .all(|line| line.width() <= width.saturating_sub(7) as usize)
            );
        }
    }

    #[test]
    fn selected_tab_remains_visible_and_clickable_at_narrow_width() {
        let mut state = UiState::new("model");
        state.tabs = (0..5)
            .map(|index| SessionTab {
                waiting_approval: false,
                elapsed: std::time::Duration::ZERO,
                id: format!("session-{index}"),
                title: format!("Conversation {index}"),
                status: RunStatus::Idle,
                active: index == 4,
            })
            .collect();
        let mut terminal = Terminal::new(TestBackend::new(40, 16)).unwrap();

        terminal.draw(|frame| draw(frame, &mut state)).unwrap();

        assert!(state.tab_areas.iter().any(|(_, id)| id == "session-4"));
        assert!(state.tab_areas.iter().all(|(area, _)| area.right() <= 40));
    }

    #[test]
    fn tool_previews_bound_single_line_payloads_without_changing_results() {
        use super::super::app::ToolRow;

        let output = "中文".repeat(20000);
        let item = TranscriptItem::Tool(ToolRow {
            id: "write".into(),
            name: "write_file".into(),
            arguments: serde_json::json!({ "path": "report.md", "content": output }).to_string(),
            status: ToolStatus::Finished,
            output: output.clone(),
        });
        let short = transcript_lines(std::slice::from_ref(&item), false);

        assert!(
            short[0]
                .to_string()
                .contains("report.md · 1 行 · 120000 字节")
        );
        assert!(short.len() <= 9);
        assert!(short.last().unwrap().to_string().is_empty());
        assert!(!short.iter().any(|line| line.to_string().contains("[展开]")));
        let expanded = transcript_lines(std::slice::from_ref(&item), true);
        assert!(expanded.len() > short.len());
        let TranscriptItem::Tool(row) = &item else {
            unreachable!()
        };
        assert_eq!(row.output, output);
        assert!(
            tool_summary(&serde_json::json!({ "command": "x".repeat(10000) }).to_string())
                .chars()
                .count()
                < 170
        );
    }

    #[test]
    fn tool_card_collapsed_single_line_and_expanded_box_click_behavior() {
        use super::super::app::ToolRow;
        use ratatui::layout::Rect;

        let row = ToolRow {
            id: "write_1".into(),
            name: "write_file".into(),
            arguments: serde_json::json!({
                "path": "hello.py",
                "content": "print(\"Hello, world!\")"
            })
            .to_string(),
            status: ToolStatus::Finished,
            output: serde_json::json!({ "written_bytes": 22, "created": true }).to_string(),
        };

        // Collapsed state: strictly single line command
        let collapsed = tool_card(&row, false, 80);
        assert_eq!(collapsed.len(), 1);
        assert!(
            collapsed[0]
                .to_string()
                .starts_with("  write_file hello.py")
        );
        assert!(!collapsed[0].to_string().contains(icons::CHECK));
        assert!(!collapsed[0].to_string().contains(icons::LOADING));
        assert!(!collapsed[0].to_string().contains(icons::CLOSE));

        // Expanded state: rounded box with breathing blank line and line-numbered code
        let expanded = tool_card(&row, true, 80);
        assert!(expanded.len() >= 4);
        assert!(expanded[0].to_string().starts_with('╭') && expanded[0].to_string().ends_with('╮'));
        assert!(expanded[1].to_string().contains("write_file hello.py"));
        assert!(
            expanded[2].to_string().starts_with("│ ") && expanded[2].to_string().ends_with(" │")
        );
        assert!(
            expanded[3]
                .to_string()
                .contains("1 │ print(\"Hello, world!\")")
        );
        let bottom = &expanded[expanded.len() - 1];
        assert!(bottom.to_string().starts_with('╰') && bottom.to_string().ends_with('╯'));

        for line in &expanded {
            assert_eq!(line.width(), 80);
        }

        // Click testing: any position inside the card (top border, header, blank line, code, bottom border) returns Some(0)
        let items = vec![TranscriptItem::Tool(row)];
        let mut layout = super::super::transcript::TranscriptLayout::default();
        let area = Rect::new(0, 0, 80, 40);

        layout.update(&items, Some(0), 80);
        let card_height = layout.height() - 1;
        for y in 0..card_height {
            assert_eq!(
                layout.disclosure_at(&items, area, 0, (0, y as u16)),
                Some(0)
            );
            assert_eq!(
                layout.disclosure_at(&items, area, 0, (2, y as u16)),
                Some(0)
            );
            assert_eq!(
                layout.disclosure_at(&items, area, 0, (30, y as u16)),
                Some(0)
            );
            assert_eq!(
                layout.disclosure_at(&items, area, 0, (70, y as u16)),
                Some(0)
            );
            assert_eq!(
                layout.disclosure_at(&items, area, 0, (79, y as u16)),
                Some(0)
            );
            assert_eq!(layout.disclosure_at(&items, area, 0, (80, y as u16)), None);
        }
    }

    #[test]
    fn tool_card_edit_file_renders_diff_backgrounds_across_row() {
        use super::super::app::ToolRow;

        let row = ToolRow {
            id: "edit_1".into(),
            name: "edit_file".into(),
            arguments: serde_json::json!({
                "path": "hello.py",
                "old_text": "x = 1",
                "new_text": "x = 2"
            })
            .to_string(),
            status: ToolStatus::Finished,
            output: "success".into(),
        };

        let card = tool_card(&row, true, 80);
        for line in &card {
            assert_eq!(line.width(), 80);
        }

        let rem_line = card
            .iter()
            .find(|l| l.to_string().contains("- x = 1"))
            .expect("removed line found");
        let add_line = card
            .iter()
            .find(|l| l.to_string().contains("+ x = 2"))
            .expect("added line found");

        let rem_bg = Some(crate::tui::theme::THEME.diff_removed_bg);
        assert_eq!(rem_line.spans[0].content, "│");
        assert_eq!(rem_line.spans[1].style.bg, rem_bg);
        assert_eq!(rem_line.spans.last().unwrap().content, "│");
        assert_eq!(rem_line.spans[rem_line.spans.len() - 2].style.bg, rem_bg);

        let add_bg = Some(crate::tui::theme::THEME.diff_added_bg);
        assert_eq!(add_line.spans[0].content, "│");
        assert_eq!(add_line.spans[1].style.bg, add_bg);
        assert_eq!(add_line.spans.last().unwrap().content, "│");
        assert_eq!(add_line.spans[add_line.spans.len() - 2].style.bg, add_bg);
    }

    #[test]
    fn markdown_tool_previews_render_structure_and_preserve_links_after_wrapping() {
        use super::super::app::ToolRow;
        let content = "# Orbit Guide\n\n- **Orbit** uses `Rust`.\n\n| Item | Link |\n| --- | --- |\n| Docs | [Orbit](https://orbit.example/docs) |\n\n```rust\nlet url = \"https://orbit.example/code\";\n```\n\n---";
        let numbered = content
            .lines()
            .enumerate()
            .map(|(index, line)| format!("{}: {line}\n", index + 40))
            .collect::<String>();
        for name in ["read_file", "write_file", "web_fetch", "read_document"] {
            let row = ToolRow {
                id: "orbit-preview".into(),
                name: name.into(),
                arguments: serde_json::json!({ "path": "docs/Orbit Guide.MD", "content": content })
                    .to_string(),
                status: ToolStatus::Finished,
                output: serde_json::json!({
                    "path": "docs/Orbit Guide.MD",
                    "format": "pdf",
                    "content": if name == "read_file" { &numbered } else { content },
                    "start_line": 40,
                    "end_line": 53,
                    "start_char": 0,
                    "total_chars": content.chars().count(),
                    "truncated": false
                })
                .to_string(),
            };
            let original = row.output.clone();
            for width in [10, 16, 32, 80, 180] {
                let card = tool_card_with_links(&row, true, width);
                assert!(
                    card.iter()
                        .all(|line| line.line.width() == usize::from(width)),
                    "{name} {width}"
                );
                let text = card
                    .iter()
                    .map(|linked| linked.line.to_string())
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(!text.contains("# Orbit Guide"), "{name} {width}");
                let links: Vec<_> = card.iter().flat_map(|line| &line.links).collect();
                assert!(
                    links
                        .iter()
                        .any(|link| link.url == "https://orbit.example/docs"),
                    "{name} {width}"
                );
                assert!(
                    !links
                        .iter()
                        .any(|link| link.url == "https://orbit.example/code"),
                    "{name} {width}"
                );
                for linked in &card {
                    for link in &linked.links {
                        let text = linked.line.to_string();
                        let label = &text[link.start..link.end];
                        assert!(
                            !label.is_empty() && !label.contains('│'),
                            "{name} {width}: {label}"
                        );
                    }
                }
                if width >= 32 {
                    assert!(text.contains("Orbit Guide"));
                    assert!(text.contains("• Orbit uses Rust."));
                    assert!(text.contains('┬'));
                }
                assert!(
                    card.iter()
                        .any(|linked| linked.line.spans.iter().any(|span| {
                            span.style.fg == Some(super::super::theme::THEME.md_rule)
                                && span.content == "─".repeat(usize::from(width) - 4)
                        })),
                    "{name} {width}"
                );
            }
            assert_eq!(row.output, original);
        }
    }

    #[test]
    fn source_colors_and_multiline_state_match_across_code_previews() {
        use super::super::{app::ToolRow, approval_diff, markdown};
        let content =
            "/* Orbit block\nfn inside_comment() {}\n*/\nfn main() {\n    let name = \"orbit\";\n}";
        let samples: Vec<_> = content.lines().collect();
        let numbered = samples
            .iter()
            .enumerate()
            .map(|(index, line)| format!("{}: {line}\n", index + 1))
            .collect::<String>();
        let tools: Vec<_> = ["read_file", "write_file", "edit_file"].into_iter().map(|name| ToolRow {
            id: "orbit-code".into(),
            name: name.into(),
            arguments: serde_json::json!({ "path": "src/Orbit Code.RS", "content": content, "old_text": "", "new_text": content }).to_string(),
            status: ToolStatus::Finished,
            output: serde_json::json!({ "path": "src/Orbit Code.RS", "content": numbered, "end_line": samples.len() }).to_string(),
        }).collect();
        let cards: Vec<_> = tools.iter().map(|row| tool_card(row, true, 100)).collect();
        let message = markdown::document(&format!("```rust\n{content}\n```"), 100);
        let diff = approval_diff::Cache::new("", content, (100, 100), true, "src/Orbit Code.RS");
        let colors = |line: &Line<'static>, source: &str| {
            let start = line.to_string().find(source).unwrap();
            let colors: Vec<_> = line
                .spans
                .iter()
                .flat_map(|span| std::iter::repeat_n(span.style.fg, span.content.len()))
                .collect();
            colors[start..start + source.len()].to_vec()
        };
        for (index, sample) in samples.iter().enumerate() {
            let expected = colors(&message[index + 1].line, sample);
            for card in &cards {
                assert_eq!(colors(&card[index + 3], sample), expected, "{sample}");
            }
            assert_eq!(colors(&diff.right[index], sample), expected, "{sample}");
        }
        let comment_colors = colors(&message[2].line, samples[1]);
        assert!(
            comment_colors
                .iter()
                .all(|color| *color == Some(Color::Rgb(120, 130, 145)))
        );
        assert_eq!(
            colors(&message[4].line, samples[3])[0],
            Some(Color::Rgb(198, 120, 221))
        );
    }

    #[test]
    fn consecutive_tools_do_not_have_blank_line_between_them() {
        use super::super::app::ToolRow;

        let row1 = ToolRow {
            id: "1".into(),
            name: "run_command".into(),
            arguments: serde_json::json!({ "command": "git status" }).to_string(),
            status: ToolStatus::Finished,
            output: "clean".into(),
        };
        let row2 = ToolRow {
            id: "2".into(),
            name: "read_file".into(),
            arguments: serde_json::json!({ "path": "main.rs" }).to_string(),
            status: ToolStatus::Finished,
            output: "fn main() {}".into(),
        };
        let items = vec![TranscriptItem::Tool(row1), TranscriptItem::Tool(row2)];
        let lines = transcript_lines(&items, false);
        // lines[0] = row1, lines[1] = row2, lines[2] = trailing empty line after tool block
        assert_eq!(lines.len(), 3);
        assert!(lines[0].to_string().contains("run_command"));
        assert!(lines[1].to_string().contains("read_file"));
        assert!(lines[2].to_string().is_empty());

        let mut layout = super::super::transcript::TranscriptLayout::default();
        layout.update(&items, None, 80);
        // In layout: entry 0 = row1 (height 1), entry 1 = row2 + trailing empty line (height 2) -> total 3
        assert_eq!(layout.height(), 3);
    }

    #[test]
    fn command_preview_skips_a_shell_safety_preamble() {
        let summary = tool_summary(
            &serde_json::json!({ "command": "set -e\ncargo test --workspace --all-features" })
                .to_string(),
        );

        assert!(summary.starts_with("cargo test --workspace"), "{summary}");
        assert!(!summary.starts_with("set -e"), "{summary}");
    }

    #[test]
    fn command_menu_keeps_composer_anchored_and_highlights_the_entire_selection() {
        for home in [false, true] {
            let mut state = UiState::new("model");

            if home {
                state.tabs.push(SessionTab {
                    waiting_approval: false,
                    elapsed: std::time::Duration::ZERO,
                    id: "test".into(),
                    title: "新会话".into(),
                    active: true,
                    status: RunStatus::Idle,
                });
            }

            let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
            terminal.draw(|frame| draw(frame, &mut state)).unwrap();
            let input = state.input_area;
            let viewport = state.selection.viewport;
            let scroll = state.max_scroll;
            state.input.set("/".into());

            terminal.draw(|frame| draw(frame, &mut state)).unwrap();

            assert_eq!(state.input_area, input);
            assert_eq!(state.selection.viewport, viewport);
            assert_eq!(state.max_scroll, scroll);
            let buffer = terminal.backend().buffer();
            let selected = buffer
                .content
                .iter()
                .filter(|cell| cell.bg == ACCENT)
                .map(|cell| cell.symbol().width())
                .sum::<usize>();
            let selected_area = state
                .menu_rows
                .iter()
                .find(|(_, index)| *index == state.menu_index)
                .unwrap()
                .0;
            assert_eq!(selected, selected_area.width as usize);
            assert!(selected < if home { 82 } else { 96 });
        }
    }

    #[test]
    fn command_menu_limits_visible_items_and_tracks_selection() {
        let mut state = UiState::new("model");
        state.input.set("/".into());
        let first_page = render(&mut state, 100, 20);

        assert!(first_page.contains("/new"));
        assert!(!first_page.contains("/tasks"));

        assert!(!first_page.contains("Tab 补全"));

        state.menu_index = 6;
        let middle_page = render(&mut state, 100, 20);

        assert!(middle_page.contains("  /resume"));
        assert!(middle_page.contains("/new"));
        assert_eq!(state.menu_offset, 0);

        state.menu_index = state.menu().len() - 1;
        let last_page = render(&mut state, 100, 20);

        assert!(last_page.contains("/exit"));
        assert!(!last_page.contains("/new"));
        assert!(!last_page.contains("Esc 关闭"));
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
    fn reasoning_is_visible_and_expansion_only_affects_one_visible_collapsible() {
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
                output: format!("first\nsecond\nthird\n{id}-detail"),
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

        assert!(footer.contains("/96k"));
        assert!(!footer.contains("model") && !footer.contains(state.permission_mode.label()));
        assert!(!footer.contains("Shift") && !footer.contains("恢复"));
    }

    #[test]
    fn clicking_memory_hint_toggles_that_memory_instead_of_the_last_tool() {
        use super::super::app::ToolRow;

        use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

        let mut state = UiState::new("model");
        state.transcript.push(TranscriptItem::Tool(ToolRow {
            id: "memory".into(),
            name: "memory_save".into(),
            status: ToolStatus::Finished,
            arguments: r#"{"title":"审查记录"}"#.into(),
            output: serde_json::json!({
                "memory": { "category": "fact", "content": "long text ".repeat(40) }
            })
            .to_string(),
        }));
        state.transcript.push(TranscriptItem::Tool(ToolRow {
            id: "later".into(),
            name: "other_tool".into(),
            status: ToolStatus::Finished,
            arguments: "{}".into(),
            output: "first\nsecond\nthird".into(),
        }));
        let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();

        for expected in [Some(0), None] {
            terminal.draw(|frame| draw(frame, &mut state)).unwrap();
            let point = (0..40)
                .find_map(|y| {
                    (0..100).find_map(|x| {
                        state
                            .transcript_layout
                            .disclosure_at(
                                &state.transcript,
                                state.selection.viewport,
                                state.max_scroll.saturating_sub(state.scroll),
                                (x, y),
                            )
                            .filter(|index| *index == 0)
                            .map(|_| (x, y))
                    })
                })
                .expect("memory toggle is visible");
            let down = Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: point.0,
                row: point.1,
                modifiers: KeyModifiers::NONE,
            });
            let up = Event::Mouse(MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: point.0,
                row: point.1,
                modifiers: KeyModifiers::NONE,
            });
            state.selection.handle(&down).unwrap();

            assert_eq!(state.selection.click(&up), Some(point));
            assert!(super::super::toggle_transcript_at(&mut state, point));
            assert_eq!(state.expanded, expected.into_iter().collect());
        }

        assert!(!super::super::toggle_transcript_at(&mut state, (0, 0)));
    }

    #[test]
    fn tool_controls_expand_read_search_and_generic_outputs_and_keep_the_draft() {
        use super::super::app::ToolRow;

        let cases = [
            (
                "read_file",
                serde_json::json!({ "start_line": 1, "end_line": 1, "content": format!("1: {}", "中文".repeat(80)) })
                .to_string(),
            ),
            (
                "search_text",
                serde_json::json!({ "matches": [{ "path": "src/main.rs", "line": 2, "text": "hit", "context": [{ "line": 1, "text": "context" }] }] })
                .to_string(),
            ),
            (
                "run_command",
                serde_json::json!({ "stdout": "first\nsecond\nthird\n", "stderr": "", "exit_code": 0 })
                .to_string(),
            ),
            ("mcp_external", "result ".repeat(80)),
            ("failed_tool", "error detail\nmore\nlast".into()),
        ];

        for (name, output) in cases {
            let mut state = UiState::new("model");
            state.input.set("unfinished draft".into());
            state.transcript.push(TranscriptItem::Tool(ToolRow {
                id: "first".into(),
                name: name.into(),
                arguments: "{}".into(),
                status: if name == "failed_tool" {
                    ToolStatus::Failed
                } else {
                    ToolStatus::Finished
                },
                output: output.clone(),
            }));

            for expected in [Some(0), None] {
                render(&mut state, 90, 32);
                let area = state.selection.viewport;
                let offset = state.max_scroll.saturating_sub(state.scroll);
                let point = (area.y..area.bottom())
                    .find_map(|y| {
                        (area.x..area.right()).find_map(|x| {
                            state
                                .transcript_layout
                                .disclosure_at(&state.transcript, area, offset, (x, y))
                                .map(|_| (x, y))
                        })
                    })
                    .unwrap_or_else(|| panic!("missing control for {name}"));

                assert!(super::super::toggle_transcript_at(&mut state, point));
                assert_eq!(state.expanded, expected.into_iter().collect());
                assert_eq!(state.input.text, "unfinished draft");

                let TranscriptItem::Tool(row) = &state.transcript[0] else {
                    unreachable!()
                };

                assert_eq!(row.output, output);
            }
        }
    }

    #[test]
    fn shortcut_collapses_scrolled_tool_with_clickable_output() {
        use super::super::app::ToolRow;

        use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

        let mut state = UiState::new("model");

        for index in 0..3 {
            state.transcript.push(TranscriptItem::Tool(ToolRow {
                id: index.to_string(),
                name: format!("tool_{index}"),
                arguments: "{}".into(),
                status: ToolStatus::Finished,
                output: (0..100).map(|line| format!("detail {line}\n")).collect(),
            }));
        }

        render(&mut state, 90, 24);
        super::super::toggle_transcript_item(&mut state, 1);
        let after_tool = state.transcript_layout.item_offset(2).unwrap();
        state.scroll = state.max_scroll.saturating_sub(after_tool - 5);
        render(&mut state, 90, 24);
        let area = state.selection.viewport;
        let offset = state.max_scroll.saturating_sub(state.scroll);
        assert!((area.y..area.bottom()).any(|y| {
            state
                .transcript_layout
                .disclosure_at(&state.transcript, area, offset, (area.x + 4, y))
                == Some(1)
        }));
        super::super::keyboard_action(
            &mut state,
            Event::Key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)),
            false,
        );
        assert!(state.expanded.is_empty());
        assert!(render(&mut state, 90, 24).contains("tool_1"));

        super::super::toggle_transcript_item(&mut state, 1);
        state.scroll = 0;
        render(&mut state, 90, 24);
        super::super::keyboard_action(
            &mut state,
            Event::Key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)),
            true,
        );

        assert_eq!(
            state.expanded,
            Default::default(),
            "must collapse tool_1 instead of expanding tool_2"
        );
    }

    #[test]
    fn model_change_uses_the_same_result_style_as_other_commands() {
        let item = TranscriptItem::CommandResult("qwen::old → qwen::new".into());
        let lines = transcript_lines(&[item], false);

        assert_eq!(lines[0].to_string(), "  qwen::old → qwen::new");
    }

    #[test]
    fn command_results_use_a_distinct_result_rail() {
        let lines = transcript_lines(
            &[
                TranscriptItem::CommandResult("状态\n细节".into()),
                TranscriptItem::CommandError("未知命令：/stauts。是否想输入 /status？".into()),
            ],
            false,
        );

        assert_eq!(lines[0].to_string(), "  状态");
        assert_eq!(lines[1].to_string(), "  细节");
        assert!(
            lines[3]
                .to_string()
                .starts_with(&format!("{} 未知命令", icons::CIRCLE))
        );
        assert!(
            lines
                .iter()
                .all(|line| !line.to_string().contains("/model asd"))
        );
    }

    #[test]
    fn welcome_commands_and_small_terminal_render() {
        let mut state = UiState::new("mock-model");
        let screen = render(&mut state, 100, 30);

        assert!(screen.contains("████"));
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
    fn running_status_shows_the_actual_cancel_shortcut() {
        let mut state = UiState::new("mock-model");
        state.begin_run();
        let screen = render(&mut state, 100, 24);
        let compact = screen.replace(' ', "");

        assert!(compact.contains("Ctrl+C取消"));
        assert!(!compact.contains("esc取消"));
        assert!(!compact.contains("第1轮"));
        assert!(!compact.contains("0.0s"));

        state.status = RunStatus::Completed;
        state.started_at = None;
        state.elapsed = std::time::Duration::from_millis(7500);
        let completed = render(&mut state, 100, 24);
        assert!(!completed.contains("7.5s"));
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
        assert_eq!(state.attachment_areas.len(), 1);

        state.attachment_cursor = Some(0);
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();

        assert!(!terminal.backend().cursor_visible());

        let chip = state.attachment_areas[0];

        assert_eq!(terminal.backend().buffer()[(chip.x, chip.y)].bg, ACCENT);
    }

    #[test]
    fn file_approval_keeps_conversation_and_draft_visible() {
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
        assert_eq!(state.input.text, "my draft");
        assert!(state.selection.viewport.height > 0);
        assert!(state.input_area.height > 0);
        assert!(text.contains("price.c"));
        assert!(text.contains("return subtotal > 10000;"));
        assert!(text.contains("return subtotal >= 10000;"));
        assert!(text.replace(' ', "").contains("允许"), "{text}");
        assert!(state.approval_shown);
    }

    #[test]
    fn transcript_and_footer_selection_keeps_the_native_input_cursor() {
        use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let mut state = UiState::new("orbit-model");
        state.input.insert("你好 draft");
        state.submit("Orbit".into());
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        for footer in [false, true] {
            state.selection.clear();
            terminal
                .draw(|frame| {
                    draw(frame, &mut state);
                    state.selection.paint(frame);
                })
                .unwrap();
            let caret = state.ime_cursor.unwrap();
            let cursor = state.input.cursor;
            let area = if footer {
                state.selection.footer
            } else {
                state.selection.viewport
            };
            for (kind, column) in [
                (MouseEventKind::Down(MouseButton::Left), area.x),
                (MouseEventKind::Drag(MouseButton::Left), area.right() - 1),
                (MouseEventKind::Up(MouseButton::Left), area.right() - 1),
            ] {
                state
                    .selection
                    .handle(&Event::Mouse(MouseEvent {
                        kind,
                        column,
                        row: if footer || matches!(kind, MouseEventKind::Down(_)) {
                            area.y
                        } else {
                            area.bottom() - 1
                        },
                        modifiers: KeyModifiers::NONE,
                    }))
                    .unwrap();
            }
            terminal
                .draw(|frame| {
                    assert!(state.selection.draw_frozen(frame));
                    frame.set_cursor_position(caret);
                })
                .unwrap();
            assert_eq!(state.input.cursor, cursor);
            assert!(terminal.backend().cursor_visible());
            terminal.backend_mut().assert_cursor_position(caret);
            if footer {
                assert!(state.selection.text().contains("/96k"));
            } else {
                assert!(state.selection.text().contains("Orbit"));
            }
        }
    }

    #[test]
    fn native_cursor_tracks_unicode_input_streaming_and_dialog_focus() {
        let mut state = UiState::new("model");
        state.workspace = "/tmp/中文项目".into();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();

        for (input, column) in [("", 4), ("你好", 8), ("你e\u{301}", 7), ("第一行\n你好", 8)]
        {
            state.input.set(input.into());
            terminal.draw(|frame| draw(frame, &mut state)).unwrap();
            let position = state.ime_cursor.unwrap();

            assert_eq!(position.x, column);

            terminal.backend_mut().assert_cursor_position(position);

            assert!(terminal.backend().cursor_visible());
            assert_eq!(
                terminal.backend().buffer()[(position.x, position.y)].bg,
                Color::Reset
            );

            // Streaming output must not leave the IME attached to the transcript.
            state.apply(orbit_agent::events::AgentEvent::Model(
                orbit_model::types::StreamEvent::TextDelta("输出中".into()),
            ));
            terminal.draw(|frame| draw(frame, &mut state)).unwrap();
            terminal.backend_mut().assert_cursor_position(position);
        }

        state.picker = Some(crate::tui::dialogs::SessionPicker::new(Vec::new()));
        crate::tui::dialogs::handle(
            &mut state,
            crossterm::event::Event::Paste("中文".repeat(80)),
        );
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let position = state.ime_cursor.unwrap();
        terminal.backend_mut().assert_cursor_position(position);

        assert_eq!(
            terminal.backend().buffer()[(position.x, position.y)].symbol(),
            " "
        );
        assert!(terminal.backend().cursor_visible());
    }

    #[test]
    fn native_caret_does_not_overwrite_wide_characters_or_the_end_of_full_rows() {
        let mut state = UiState::new("orbit-model");
        state.input.insert("你好世界中文");
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        while !state.input.text.is_empty() {
            state.input.backspace();
            terminal.draw(|frame| draw(frame, &mut state)).unwrap();
            let caret = state.ime_cursor.unwrap();
            assert!(terminal.backend().cursor_visible());
            terminal.backend_mut().assert_cursor_position(caret);
            assert_eq!(
                terminal.backend().buffer()[(caret.x, caret.y)].symbol(),
                " "
            );
            assert_eq!(
                terminal.backend().buffer()[(caret.x + 1, caret.y)].bg,
                Color::Reset
            );
        }
        state.input.set(format!("{}\nnext", "a".repeat(74)));
        state.input.set_cursor(74);
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let caret = state.ime_cursor.unwrap();
        assert_eq!(caret.x, state.input_area.right() - 1);
        assert_eq!(
            terminal.backend().buffer()[(caret.x - 1, caret.y)].symbol(),
            "a"
        );
        assert_eq!(
            terminal.backend().buffer()[(caret.x, caret.y)].symbol(),
            " "
        );
    }

    #[test]
    fn frozen_copy_selection_excludes_transient_cursor_effects() {
        use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let mut state = UiState::new("orbit-model");
        state.cursor_motion.enabled = true;
        state.submit("Orbit".into());
        state.input.set("draft".into());
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        for text in ["draft", "draft Orbit"] {
            state.input.set(text.into());
            terminal
                .draw(|frame| {
                    draw(frame, &mut state);
                    state.selection.paint(frame);
                    state
                        .cursor_motion
                        .paint(frame.buffer_mut(), std::time::Instant::now());
                })
                .unwrap();
        }
        assert!(state.cursor_motion.active(std::time::Instant::now()));
        let area = state.selection.viewport;
        state
            .selection
            .handle(&Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: area.x,
                row: area.y,
                modifiers: KeyModifiers::NONE,
            }))
            .unwrap();
        state.cursor_motion.clear();
        terminal
            .draw(|frame| {
                assert!(state.selection.draw_frozen(frame));
                let input = state.input_area;
                for x in input.x..input.right() {
                    assert_eq!(frame.buffer_mut()[(x, input.y)].bg, Color::Reset);
                }
            })
            .unwrap();
    }

    #[test]
    fn input_selection_paints_whole_graphemes_and_does_not_freeze_the_transcript() {
        let mut state = UiState::new("orbit-model");
        state.input.set("你好👩‍💻 Orbit".into());
        state.input.begin_selection(0);
        state.input.extend_selection("你好👩‍💻".len());
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| {
                draw(frame, &mut state);
                let area = state.input_area;
                let buffer = frame.buffer_mut();
                for x in area.x..area.x + 6 {
                    assert_eq!(buffer[(x, area.y)].bg, THEME.selection);
                }
                assert_eq!(buffer[(area.x + 6, area.y)].bg, Color::Reset);
                assert!(!state.selection.draw_frozen(frame));
            })
            .unwrap();
        assert_eq!(state.input.selected_text(), Some("你好👩‍💻"));
    }

    #[test]
    fn footer_keeps_project_and_context_with_unicode_paths() {
        let mut state = UiState::new("deepseek");
        state.workspace = format!("/tmp/{}/项目", "长目录/".repeat(40));
        state.context_used_tokens = 51_200;

        for width in [24, 40, 80] {
            let line = footer(&state, width);

            assert!(line.width() <= width);

            let text = line.to_string();

            if width >= 40 {
                assert!(text.contains("项目"));
            }

            if width >= 80 {
                assert!(!text.contains("󰧑"));
                assert!(!text.contains("思考"));
                assert!(text.contains("53%/96k"));
                assert!(!text.contains(state.permission_mode.label()));
                assert!(!text.contains('·'));
            }

            assert!(text.ends_with("  "));
        }

        assert_eq!(fit_label("你好e\u{301}", 3, true), "…e\u{301}");
        assert_eq!(compact_tokens(999), "999");
        assert_eq!(compact_tokens(1_999), "1k");
        assert_eq!(compact_tokens(1_000_000), "1M");
        assert_eq!(compact_tokens(1_500_000), "1.5M");
    }

    #[test]
    fn context_indicator_reveals_its_budget_on_hover() {
        use crossterm::event::{Event, KeyModifiers, MouseEvent, MouseEventKind};

        let mut state = UiState::new("model");
        state.context_used_tokens = 51_200;
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let indicator = state.context_indicator_area;

        assert_eq!(indicator.width, 7);

        let hover = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: indicator.x,
            row: indicator.y,
            modifiers: KeyModifiers::NONE,
        });

        assert_eq!(handle_context_hover(&mut state, &hover), Some(true));

        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let content: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect();

        assert!(
            content.contains("53%/96k") && !content.contains("上下文窗口（本地估算）"),
            "{content}"
        );

        let outside = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });

        assert_eq!(handle_context_hover(&mut state, &outside), Some(true));
    }

    #[test]
    fn small_nonempty_context_is_not_displayed_as_zero() {
        let mut state = UiState::new("model");
        state.input_context_tokens = 224_000;

        for (tokens, expected) in [
            (0, "0%/224k"),
            (1, "<1%/224k"),
            (2_239, "<1%/224k"),
            (2_240, "1%/224k"),
            (224_000, "100%/224k"),
            (322_560, "144%/224k"),
        ] {
            state.context_used_tokens = tokens;

            assert!(footer(&state, 120).to_string().contains(expected));
        }
    }

    #[test]
    fn link_hover_shows_and_clears_its_ctrl_click_hint() {
        use crossterm::event::{Event, KeyModifiers, MouseEvent, MouseEventKind};

        let mut state = UiState::new("model");
        state.transcript.push(TranscriptItem::Assistant(
            "打开 https://example.com/docs 查看文档。".into(),
        ));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let offset = state.max_scroll;
        let target =
            (state.selection.viewport.y..state.selection.viewport.bottom()).find_map(|y| {
                (state.selection.viewport.x..state.selection.viewport.right()).find_map(|x| {
                    state
                        .transcript_layout
                        .link_at(state.selection.viewport, offset, (x, y))
                        .map(|_| (x, y))
                })
            });
        let (x, y) = target.expect("bare URL has a hit target");
        let hover = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        });

        assert_eq!(handle_link_hover(&mut state, &hover), Some(true));

        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let shown: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();

        assert!(shown.replace(' ', "").contains("Ctrl+Click跳转"), "{shown}");

        let leave = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });

        assert_eq!(handle_link_hover(&mut state, &leave), Some(true));

        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let hidden: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();

        assert!(
            !hidden.replace(' ', "").contains("Ctrl+Click跳转"),
            "{hidden}"
        );
    }

    #[test]
    fn repository_url_in_a_table_has_hover_feedback_after_wrapping_and_resize() {
        use crossterm::event::{Event, KeyModifiers, MouseEvent, MouseEventKind};
        use ratatui::buffer::CellWidth;
        let url = "https://github.com/linyisu/orbit";
        let mut state = UiState::new("model");
        state.transcript.push(TranscriptItem::Assistant(format!(
            "Orbit 示例项目\n\n| 项 | 值 |\n| --- | --- |\n| 项目 | Orbit |\n| 仓库 | {url} |\n| 提交署名 | Orbit 测试身份（虚构） |\n| Windows 用户名 | orbit（示例路径 C:\\Users\\orbit\\） |\n| 会话 ID | orbit-test-session |\n| 用途 | TUI 链接测试 |\n"
        )));
        for width in [100, 64, 30] {
            let mut terminal = Terminal::new(TestBackend::new(width, 80)).unwrap();
            terminal.draw(|frame| draw(frame, &mut state)).unwrap();
            let area = state.selection.viewport;
            let mut points = Vec::new();
            for y in area.y..area.bottom() {
                for x in area.x..area.right() {
                    if state
                        .transcript_layout
                        .link_at(area, state.max_scroll, (x, y))
                        == Some(url)
                    {
                        points.push((x, y));
                    }
                }
            }
            assert_eq!(points.len(), url.len(), "width {width}: {points:?}");
            for &(x, y) in [points.first().unwrap(), points.last().unwrap()] {
                let hover = Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Moved,
                    column: x,
                    row: y,
                    modifiers: KeyModifiers::NONE,
                });
                handle_link_hover(&mut state, &hover);
                terminal.draw(|frame| draw(frame, &mut state)).unwrap();
                let hint = state.link_tooltip_area;
                assert!(hint.width > 0, "width {width}, point ({x}, {y})");
                let mut text = String::new();
                let mut column = hint.x;
                while column < hint.right() {
                    let cell = &terminal.backend().buffer()[(column, hint.y)];
                    text.push_str(cell.symbol());
                    column += cell.cell_width().max(1);
                }
                assert!(text.replace(' ', "").contains("Ctrl+Click跳转"), "{text}");
            }
            state.link_hovered = None;
        }
    }

    #[test]
    fn output_control_sequences_are_visible_text_not_terminal_commands() {
        assert_eq!(
            safe_output("a\x1b]52;c;secret\x07"),
            "a\\u{1b}]52;c;secret\\u{7}"
        );
    }

    #[test]
    fn thinking_uses_markdown_with_a_subdued_continuous_rail() {
        let mut code = None;
        let line = thinking_line("- **重点** 与 `code`", 0, &mut code);

        assert_eq!(line.spans[0].content, "  ┊ ");
        assert!(
            line.spans
                .iter()
                .any(|span| span.content == "重点"
                    && span.style.add_modifier.contains(Modifier::BOLD))
        );
        assert!(line.spans.iter().any(|span| span.content == "code"
            && span.style.fg == Some(desaturate_for_thinking(Some(THEME.md_inline_code)))));
        assert_eq!(line.style.fg, None);
        assert_ne!(desaturate_for_thinking(Some(ACCENT)), ACCENT);
    }

    #[test]
    fn thinking_code_keeps_a_compound_thinking_and_code_rail() {
        let mut code = None;
        let header = thinking_line("```markdown", 0, &mut code);
        let badge = header
            .spans
            .iter()
            .find(|span| span.style.bg.is_some())
            .unwrap();
        let original = assistant_line("```markdown", 0, &mut None);
        assert_eq!(
            badge.style.bg,
            Some(desaturate_for_thinking(original.spans[1].style.bg))
        );
        assert_eq!(badge.content, " markdown ");
        let line = thinking_line(
            "  nested source that should wrap inside the thought",
            1,
            &mut code,
        );

        assert_eq!(line.spans[0].content, "  ┊ │ ");
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
