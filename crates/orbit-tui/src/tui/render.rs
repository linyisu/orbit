//! Frame orchestration. Component renderers live beside this facade.
pub(super) use thinking::thinking_header;
mod links;
mod text;
mod thinking;
mod tool_cards;

pub(super) use links::handle_link_hover;
use links::{clear_previous_link_tooltip, draw_link_tooltip};

pub(super) use text::{
    compact_preview, parse_ansi_to_spans, safe_multiline_output, safe_output, sanitize_line,
    sanitize_linked_line,
};
pub(super) use thinking::{thinking_card_at_width, thinking_linked};
pub(super) use tool_cards::{default_tool_summary, read_file_summary, tool_card};

#[cfg(test)]
pub(super) use thinking::{assistant_line, thinking_line};
#[cfg(test)]
pub(super) use tool_cards::tool_summary;

use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Paragraph},
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

#[cfg(test)]
use super::app::ToolStatus;
use super::app::{RunStatus, TranscriptItem, UiState};
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
// Keep every frame within the middle two Braille rows so the animation stays
// visually centered in a single terminal cell, with equal space above/below.
const TAB_SPINNER_FRAMES: [&str; 4] = ["⠖", "⠲", "⠴", "⠦"];
const TAB_SPINNER_FRAME_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);
const MAX_SESSION_TAB_WIDTH_COLUMNS: usize = 22;
pub(super) const MAX_COMPLETION_ITEMS: usize = 8;

/// Notices paint after selection snapshots, including a frozen transcript.
/// They must never become part of the selectable/frozen conversation image.
pub(super) fn draw_frame(frame: &mut Frame<'_>, state: &mut UiState) {
    super::overlay::prepare(state);
    if !state.selection.draw_frozen(frame) {
        draw(frame, state);
        state.selection.paint(frame);
        state
            .cursor_motion
            .paint(frame.buffer_mut(), std::time::Instant::now());
    }
    state.feedback.draw(frame, frame.area());
}

pub fn draw(frame: &mut Frame<'_>, state: &mut UiState) {
    super::overlay::prepare(state);
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
        } else if state
            .menu_area
            .contains((mouse.column, mouse.row).into())
        {
            super::completion::select_with_mouse(state, &mouse);
        }

        draw_contents(frame, state);
    }

    let now = std::time::Instant::now();
    let editing = state.workspace_wizard.is_none()
        && state.ssh_prompt.is_none()
        && !state.dialog_open()
        && state.restore_picker.is_none()
        && state.input.selection().is_none()
        && !state.selection.active();

    state.cursor_motion.update(
        frame.area(),
        state.input_area,
        state.ime_cursor.filter(|_| editing),
        now,
    );
    if super::overlay::active(state) == Some(super::overlay::Layer::Management) {
        state.ime_cursor = None;
        if let Some(panel) = &mut state.management.active {
            panel.draw(frame);
        }
    }
    if state.media_preview.is_some()
        || state.workspace_wizard.is_some()
        || state.ssh_prompt.is_some()
    {
        state.ime_cursor = None;
    }
    if state.info_dialog.is_none()
        && let Some(preview) = &mut state.media_preview
    {
        preview.draw(frame);
    }
    if state.info_dialog.is_none()
        && let Some(wizard) = &mut state.workspace_wizard
    {
        wizard.draw(frame);
    }
    if state.info_dialog.is_none()
        && let Some(prompt) = &state.ssh_prompt
    {
        prompt.draw(frame);
    }
    if state.info_dialog.is_some() {
        super::dialogs::draw(frame, state);
    }
    if let Some(position) = state
        .ime_cursor
        .filter(|p| frame.area().contains(*p))
    {
        frame.set_cursor_position(position);
    }
    state.overlay_needs_layout = false;
}

fn draw_contents(frame: &mut Frame<'_>, state: &mut UiState) {
    let area = frame.area();
    state.selection.viewport = Rect::default();
    state.selection.footer = Rect::default();
    state.selection.approval = Rect::default();
    state.context_indicator_area = Rect::default();
    state.usage_indicator_area = Rect::default();
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
    state.subagent_parent_area = Rect::default();
    let home =
        state.is_home() && !state.tabs.is_empty() && state.approval.is_none() && area.height >= 18;
    let content_width = if home { area.width.min(86) } else { area.width };

    let controls_width = super::navigation::controls_width(state);
    let width = content_width.saturating_sub(6 + controls_width) as usize;
    state.input_width = width;
    let (input_lines, cursor) = state.input.visual_lines(width);
    let input_height = input_lines
        .len()
        .clamp(1, 6)
        .min(area.height.saturating_sub(8) as usize) as u16;
    let thumbnails =
        super::attachments::thumbnails(state, content_width.saturating_sub(4 + controls_width));
    let attachment_height =
        (thumbnails.rows.len() as u16).saturating_add(u16::from(!thumbnails.rows.is_empty()));
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
        || state.dialog_open()
        || state.approval.is_some()
        || state.resume_loading.is_some()
    {
        state.navigation.clear_hover();
        state.tab_hover = None;
        state.context_hovered = false;
        state.link_hovered = None;
        state.transcript_pointer = None;
    }

    let panel_height = if !menu.is_empty() {
        menu.len().min(MAX_COMPLETION_ITEMS) as u16
    } else if state.directory_completion_pending() {
        1
    } else {
        0
    };

    let approval_height = if state.approval.is_some() {
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
        Constraint::Length(u16::from(state.subagent_parent.is_some())),
        Constraint::Length(u16::from(!home)),
        Constraint::Length(input_height + attachment_height + details_height + 3),
        Constraint::Length(1),
    ])
    .split(area)
    .to_vec();
    areas.swap(4, 5);

    let mut home_logo = Rect::default();
    if home {
        let composer_height = input_height + attachment_height + details_height + 3;
        let header_height = u16::from(state.tabs.len() > 1);
        let available_height = area
            .height
            .saturating_sub(header_height);
        let logo_height = 7.min(available_height.saturating_sub(composer_height + 3));
        let group_height = logo_height + 1 + composer_height + 2;
        let x = area.x + area.width.saturating_sub(content_width) / 2;
        let y = area.y + header_height + available_height.saturating_sub(group_height) / 2;
        areas[0] = if state.tabs.len() > 1 {
            Rect::new(area.x, area.y, area.width, header_height)
        } else {
            Rect::default()
        };
        home_logo = Rect::new(x, y, content_width, logo_height);
        areas[1] = Rect::new(x, y + logo_height + 1, content_width, 0);
        areas[2] = Rect::default();
        areas[4] = Rect::new(x, areas[1].bottom(), content_width, composer_height);
        areas[5] = Rect::default();
        areas[6] = Rect::new(x, areas[4].bottom(), content_width, 1);
    }

    let pending_height = state
        .pending_input
        .height(areas[1].width)
        .min(areas[1].height / 3);
    let pending_height = if pending_height >= 2 {
        pending_height
    } else {
        0
    };
    let pending_area = Rect::new(
        areas[1].x,
        areas[1]
            .bottom()
            .saturating_sub(pending_height),
        areas[1].width,
        pending_height,
    );
    areas[1].height = areas[1]
        .height
        .saturating_sub(pending_height);
    let parent_area = areas[3];
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
    if state.is_home() && !state.tabs.is_empty() {
        super::navigation::clear_previous_tooltip(frame, state);
        clear_previous_link_tooltip(frame, state);
        state.navigation.reset_area();
        state.scroll = 0;
        state.max_scroll = 0;
    } else {
        draw_transcript(frame, state, areas[1]);
    }

    if state.approval.is_some() {
        super::approval::draw_in(frame, state, areas[2]);
        state.selection.approval = areas[2];
    }
    state
        .pending_input
        .draw(frame, pending_area);

    if menu_height == 0 {
        super::subagents::draw_parent(frame, state, parent_area);
    }
    draw_panel(frame, state, areas[3]);
    draw_composer(frame, state, areas[4], input_lines, cursor, thumbnails);
    super::navigation::draw_controls(frame, state, areas[4]);
    draw_status(frame, state, areas[5]);
    let footer = footer_layout(state, areas[6].width as usize);
    state.context_details_visible = footer.context_details_visible;

    if let Some(offset) = footer.context_offset {
        state.context_indicator_area =
            Rect::new(areas[6].x + offset, areas[6].y, footer.context_width, 1);
    }
    if let Some(offset) = footer.usage_offset {
        state.usage_indicator_area =
            Rect::new(areas[6].x + offset, areas[6].y, footer.usage_width, 1);
    }

    frame.render_widget(Paragraph::new(footer.line), areas[6]);
    state.selection.footer = areas[6];
    draw_context_tooltip(frame, state);
    state.planning.draw_badge(frame);
    if state.info_dialog.is_none() && state.planning.visible() {
        state.ime_cursor = state.planning.draw(frame);
        return;
    }
    state.user_input.draw_badge(frame);
    if state.info_dialog.is_none() && state.user_input.visible() {
        state.ime_cursor = state.user_input.draw(frame);
    } else if state.info_dialog.is_none() {
        super::dialogs::draw(frame, state);
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
    usage_offset: Option<u16>,
    usage_width: u16,
}

#[cfg(test)]
fn footer(state: &UiState, width: usize) -> Line<'static> {
    footer_layout(state, width).line
}

fn activity_spans(state: &UiState, width: usize) -> Vec<Span<'static>> {
    let label = if state.status == RunStatus::Cancelling {
        "正在取消"
    } else if state.approval.is_some() {
        "等待审批"
    } else {
        state.activity.label()
    };
    let hint = if width < 26 { "" } else { " · Ctrl+C 取消" };
    let label = fit_label(
        &safe_output(label),
        width.saturating_sub(hint.width()),
        false,
    );
    let length = label.chars().count().max(1);
    let animate = state.approval.is_none();
    let mut spans: Vec<_> = label
        .chars()
        .enumerate()
        .map(|(index, character)| {
            let color = if animate {
                super::theme::activity_color(THEME.secondary, state.active_elapsed(), index, length)
            } else {
                super::theme::THEME.secondary
            };
            Span::styled(character.to_string(), Style::default().fg(color))
        })
        .collect();
    if !hint.is_empty() {
        spans.push(Span::styled(
            hint,
            Style::default().fg(super::theme::THEME.muted_text),
        ));
    }
    spans
}

fn footer_layout(state: &UiState, width: usize) -> FooterLayout {
    let available = width.saturating_sub(4);
    let context = if state.input_context_tokens == 0 {
        "上下文 —".to_owned()
    } else {
        format!(
            "{}/{}",
            context_percent_label(state),
            compact_tokens(input_context_budget(state)),
        )
    };
    let context_details_visible = context.width() + 8 <= available;
    let right = if context_details_visible {
        context
    } else {
        context_percent_label(state)
    };

    let right = fit_label(&right, available, false);
    let quota = if right.width() + "用量".width() + 8 <= available {
        state
            .usage
            .indicator(available.saturating_sub(right.width() + 12))
    } else {
        Line::default()
    };
    let quota_gap = if quota.width() == 0 { "" } else { "  " };
    let quota_width = quota.width() + quota_gap.width();
    let path_width = available.saturating_sub(right.width() + quota_width + 2);
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

    let (left_spans, left_width) = {
        let w = path.width();
        (
            vec![Span::styled(
                path,
                Style::default().fg(crate::tui::theme::THEME.secondary),
            )],
            w,
        )
    };

    let gap = available.saturating_sub(left_width + right.width() + quota_width);
    let context_offset = (!right.is_empty()).then_some((2 + left_width + gap + quota_width) as u16);
    let context_width = right.width() as u16;
    let usage_offset = (quota.width() > 0).then_some((2 + left_width + gap) as u16);
    let usage_width = quota.width() as u16;

    let mut line_spans = vec![Span::raw("  ")];
    line_spans.extend(left_spans);
    line_spans.push(Span::raw(" ".repeat(gap)));
    line_spans.extend(quota.spans);
    line_spans.push(Span::raw(quota_gap));
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
        usage_offset,
        usage_width,
    }
}

fn composer_details(state: &UiState, width: usize) -> Vec<Line<'static>> {
    if width == 0 {
        return Vec::new();
    }

    let (provider, model) = state
        .model
        .split_once("::")
        .unwrap_or(("", &state.model));
    let accent = Style::default().fg(ACCENT);
    let mut fields = vec![
        Span::styled(
            state.planning.mode().label().to_owned(),
            accent.add_modifier(Modifier::BOLD),
        ),
        // Reserve four cells for the longest mode and one for its pending
        // marker so receipts never move or wrap the fields to the right.
        Span::styled(format!("{:<5}", state.permissions.label()), accent),
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
    orbit_client::usage::context_percent(state.context_used_tokens, state.input_context_tokens)
}

fn context_percent_label(state: &UiState) -> String {
    if state.resume_loading.is_some() || state.input_context_tokens == 0 {
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

    let offset = state
        .max_scroll
        .saturating_sub(state.scroll);
    let hit = |point| {
        state
            .transcript_layout
            .html_disclosure_at(state.selection.viewport, offset, point)
            .map(|(index, _)| index)
            .or_else(|| {
                state
                    .transcript_layout
                    .group_at(state.selection.viewport, offset, point)
            })
            .or_else(|| {
                state.transcript_layout.disclosure_at(
                    &state.transcript,
                    state.selection.viewport,
                    offset,
                    point,
                )
            })
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

    super::components::hint_above(frame, state.context_indicator_area, &lines.join("\n"));
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
    let cycle_items: Vec<&str> = graphemes
        .iter()
        .copied()
        .chain(separator)
        .collect();

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
    let hover = state
        .tab_hover
        .as_ref()
        .filter(|(action, _)| action == target)
        .map(|(_, since)| since.elapsed());
    if matches!(target, super::TabAction::New) {
        return if hover.is_some() {
            Style::default()
                .fg(THEME.foreground)
                .bg(THEME.tab_hover_background)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(ACCENT)
                .bg(THEME.tab_background)
        };
    }
    super::theme::control_style(hover)
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
    let active_index = state
        .tabs
        .iter()
        .position(|tab| tab.active)
        .unwrap_or(0);
    let budget = area.width.saturating_sub(8) as usize;
    let prefixes = state
        .tabs
        .iter()
        .map(|tab| {
            let running = (tab.active && state.status == RunStatus::Running)
                || tab.status == RunStatus::Running;
            if tab.waiting_approval {
                "待审批 ".to_owned()
            } else if running {
                let index = (tab.elapsed.as_millis() / TAB_SPINNER_FRAME_INTERVAL.as_millis())
                    as usize
                    % TAB_SPINNER_FRAMES.len();
                format!("{} ", TAB_SPINNER_FRAMES[index])
            } else {
                String::new()
            }
        })
        .collect::<Vec<_>>();
    let widths = titles
        .iter()
        .zip(&prefixes)
        .map(|(title, prefix)| {
            (title.width() + prefix.width() + 5)
                .min(MAX_SESSION_TAB_WIDTH_COLUMNS)
                .min(budget)
        })
        .collect::<Vec<_>>();
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

    for (index, tab) in state
        .tabs
        .iter()
        .enumerate()
        .skip(start)
    {
        let available = area.right().saturating_sub(x + 5) as usize;

        let tab_width = widths[index];
        if tab_width < 6 || available < tab_width {
            break;
        }

        let width = tab_width as u16;
        let spinner_prefix = &prefixes[index];
        let title_width = width.saturating_sub(5 + spinner_prefix.width() as u16) as usize;
        let elapsed = state
            .tab_hover
            .as_ref()
            .and_then(|(action, since)| {
                (*action == super::TabAction::Select(tab.id.clone())).then(|| since.elapsed())
            });
        let title = scrolling_title(&titles[index], title_width, elapsed);
        let display_title = format!("{spinner_prefix}{title}");
        let rect = Rect::new(x, area.y, width, 1);
        let hovered = state
            .tab_hover
            .as_ref()
            .is_some_and(|(action, _)| {
                matches!(
                    action,
                    super::TabAction::Select(id) | super::TabAction::Close(id) if id == &tab.id
                )
            });
        let style = if tab.active {
            Style::default()
                .fg(crate::tui::theme::THEME.on_accent)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD)
        } else if hovered {
            Style::default()
                .fg(crate::tui::theme::THEME.foreground)
                .bg(crate::tui::theme::THEME.tab_hover_background)
                .add_modifier(Modifier::BOLD)
        } else if tab.status == RunStatus::Failed {
            Style::default()
                .fg(crate::tui::theme::THEME.tab_error)
                .bg(crate::tui::theme::THEME.tab_background)
        } else {
            Style::default()
                .fg(crate::tui::theme::THEME.tab_inactive)
                .bg(crate::tui::theme::THEME.tab_background)
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
        if hovered {
            let action = super::TabAction::Close(tab.id.clone());
            let close_style = if state
                .tab_hover
                .as_ref()
                .is_some_and(|(target, _)| target == &action)
            {
                style.patch(tab_control_style(state, &action))
            } else {
                style
            };
            frame.render_widget(
                Paragraph::new(format!(" {} ", icons::CLOSE_THICK)).style(close_style),
                close,
            );
        }

        state
            .tab_areas
            .push((rect, tab.id.clone()));
        state
            .tab_close_areas
            .push((close, tab.id.clone()));
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
    let mut width = logo
        .iter()
        .map(Line::width)
        .max()
        .unwrap_or(0) as u16;

    if width > area.width.saturating_sub(4) || logo.len() > area.height as usize {
        logo = wordmark();
        width = logo
            .iter()
            .map(Line::width)
            .max()
            .unwrap_or(0) as u16;
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
    draw_transcript_in(frame, state, area, true);
}

pub(super) fn draw_transcript_in(
    frame: &mut Frame<'_>,
    state: &mut UiState,
    area: Rect,
    navigation: bool,
) {
    super::navigation::clear_previous_tooltip(frame, state);
    clear_previous_link_tooltip(frame, state);
    state.selection.viewport = if navigation {
        Rect {
            x: area.x + 2,
            // Keep a generous blank gap between wrapped output and the navigation dots
            // so double-width CJK characters never collide with or touch the dots.
            width: area.width.saturating_sub(6),
            ..area
        }
    } else {
        area
    };
    super::media::occlude(state.selection.viewport);
    if navigation
        && area.height > 1
        && state.transcript.iter().any(|item| {
            matches!(
                item,
                TranscriptItem::User(_) | TranscriptItem::UserWithImages { .. }
            )
        })
    {
        state.selection.viewport.height -= 1;
    }

    if state.transcript.is_empty() {
        state.navigation.reset_area();

        if state.resume_loading.is_some() {
            return;
        }

        if navigation {
            draw_welcome(frame, area);
        }
        return;
    }

    let width = state.selection.viewport.width;
    let mut media = state.media.clone();
    if !state.workspace.is_empty() {
        media.workspace = state.workspace.clone().into();
    }
    state.transcript_layout.set_media(media);
    let previous_max = state.max_scroll;
    let previous_offset = previous_max.saturating_sub(state.scroll);
    let scroll_anchor = (!state.follow_bottom)
        .then(|| {
            state
                .transcript_layout
                .anchor_at(previous_offset)
        })
        .flatten();
    state.transcript_layout.update_expanded(
        &state.transcript,
        &state.expanded,
        &state.expanded_tool_groups,
        width,
    );
    state.max_scroll = state
        .transcript_layout
        .height()
        .saturating_sub(state.selection.viewport.height as usize);

    if !state.follow_bottom {
        if let Some(offset) = scroll_anchor.and_then(|anchor| {
            state
                .transcript_layout
                .offset_for_anchor(anchor)
        }) {
            state.scroll = state
                .max_scroll
                .saturating_sub(offset.min(state.max_scroll));
        } else {
            state.scroll = state.scroll.saturating_add(
                state
                    .max_scroll
                    .saturating_sub(previous_max),
            );
        }
    }

    state.scroll = state.scroll.min(state.max_scroll);
    let offset = if state.follow_bottom {
        state.max_scroll
    } else {
        state
            .max_scroll
            .saturating_sub(state.scroll)
    };

    state
        .selection
        .transcript_view(offset, &state.transcript_layout);
    state
        .transcript_layout
        .draw(frame, state.selection.viewport, offset);
    if let Some(focus) = &state.footnote_focus {
        state
            .transcript_layout
            .paint_footnote_focus(frame, state.selection.viewport, offset, focus);
    }
    if !state.selection.dragging()
        && let Some(point) = state.transcript_pointer
        && state
            .transcript_layout
            .html_disclosure_at(state.selection.viewport, offset, point)
            .is_some()
    {
        let absolute = offset + usize::from(point.1 - state.selection.viewport.y);
        let decorations = state
            .transcript_layout
            .decorations(absolute);
        let columns = state.selection.viewport.x..state.selection.viewport.right();
        let first = columns.clone().find(|&x| {
            matches!(
                frame.buffer_mut()[(x, point.1)].symbol(),
                icons::CHEVRON_RIGHT | icons::CHEVRON_DOWN
            )
        });
        let last = columns.rev().find(|&x| {
            !decorations
                .iter()
                .any(|range| range.contains(&usize::from(x - state.selection.viewport.x)))
                && frame.buffer_mut()[(x, point.1)].symbol() != " "
        });

        if let (Some(first), Some(last)) = (first, last) {
            let end = last
                .saturating_add(
                    frame.buffer_mut()[(last, point.1)]
                        .symbol()
                        .width()
                        .max(1) as u16,
                )
                .min(state.selection.viewport.right());
            for x in first..end {
                let cell = &mut frame.buffer_mut()[(x, point.1)];
                cell.set_bg(THEME.accent)
                    .set_fg(THEME.on_accent);
            }
        }
    }

    if !state.selection.dragging()
        && let Some(point) = state.transcript_pointer
        && let Some(index) = state
            .transcript_layout
            .group_at(state.selection.viewport, offset, point)
            .or_else(|| {
                state.transcript_layout.disclosure_at(
                    &state.transcript,
                    state.selection.viewport,
                    offset,
                    point,
                )
            })
    {
        match state.transcript.get(index) {
            Some(TranscriptItem::Thinking(_)) => {
                if state
                    .transcript_layout
                    .item_offset(index)
                    == Some(offset + usize::from(point.1 - state.selection.viewport.y))
                {
                    for x in state.selection.viewport.x..state.selection.viewport.right() {
                        let cell = &mut frame.buffer_mut()[(x, point.1)];
                        if cell.symbol() != " " {
                            cell.set_fg(THINKING_BRIGHT);
                            cell.set_style(
                                cell.style()
                                    .add_modifier(Modifier::BOLD),
                            );
                        }
                    }
                }
            }

            Some(TranscriptItem::Tool(_)) => {
                let group_header = state
                    .transcript_layout
                    .group_at(state.selection.viewport, offset, point)
                    .is_some();
                let absolute = offset
                    + usize::from(
                        point
                            .1
                            .saturating_sub(state.selection.viewport.y),
                    );
                if let Some(header) = state
                    .transcript_layout
                    .tool_header_range(index, group_header)
                    .filter(|header| header.contains(&absolute))
                {
                    let visible = header.start.max(offset)
                        ..header
                            .end
                            .min(offset + usize::from(state.selection.viewport.height));
                    for row_abs in visible {
                        let y = state.selection.viewport.y + (row_abs - offset) as u16;
                        let decorations = state
                            .transcript_layout
                            .decorations(row_abs);
                        for x in state.selection.viewport.x..state.selection.viewport.right() {
                            if decorations.iter().any(|range| {
                                range.contains(&usize::from(x - state.selection.viewport.x))
                            }) {
                                continue;
                            }

                            let cell = &mut frame.buffer_mut()[(x, y)];
                            if cell.symbol() != " " {
                                cell.set_style(
                                    cell.style()
                                        .add_modifier(Modifier::BOLD),
                                );
                                if cell.fg == TOOL_FINISHED {
                                    cell.set_fg(THEME.tool_hover);
                                }
                            }
                        }
                    }
                }
            }

            _ => {}
        }
    }

    if navigation {
        super::navigation::draw(frame, state, area, offset);
    }
    draw_link_tooltip(frame, state, area, offset);
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
            TranscriptItem::User(text) | TranscriptItem::UserWithImages { text, .. } => {
                let text = text.trim();
                for (index, line) in text.lines().enumerate() {
                    lines.push(Line::from(vec![
                        Span::styled(
                            if index == 0 {
                                format!("{} ", icons::PROMPT)
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

                lines.extend(
                    super::markdown::document(text, width)
                        .into_iter()
                        .map(|linked| linked.line),
                );

                lines.push(Line::from(""));
            }

            TranscriptItem::Thinking(row) => {
                lines.extend(
                    thinking_card_at_width(row, expanded, width)
                        .into_iter()
                        .map(|linked| linked.line),
                );
            }

            TranscriptItem::Tool(row) => {
                let framed = expanded && width >= 10;
                if framed {
                    if lines
                        .last()
                        .is_some_and(|line| line.to_string().is_empty())
                    {
                        lines.pop();
                    }
                } else if index > 0
                    && lines
                        .last()
                        .is_some_and(|line| !line.to_string().trim().is_empty())
                    && !matches!(items.get(index - 1), Some(TranscriptItem::Tool(_)))
                {
                    lines.push(Line::from(""));
                }

                lines.extend(tool_card(row, expanded, width));

                if !framed {
                    lines.push(Line::from(""));
                }
            }

            TranscriptItem::CommandError(text) => {
                let text = orbit_client::directories::display_diagnostic(text);
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
                let text = orbit_client::directories::display_diagnostic(text);

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
fn draw_status(frame: &mut Frame<'_>, state: &mut UiState, area: Rect) {
    state.progress_area = if state.running() {
        area
    } else {
        Rect::default()
    };
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

    let spans = if state.running() {
        let mut spans = vec![Span::raw("  ")];
        let progress = state.activity.progress();
        if progress.is_empty() {
            spans.extend(activity_spans(state, area.width.saturating_sub(4) as usize));
        } else {
            spans.push(Span::styled(
                fit_label(
                    &safe_output(&format!("{} · {progress}", state.activity.label())),
                    area.width.saturating_sub(4) as usize,
                    false,
                ),
                Style::default().fg(THEME.secondary),
            ));
        }
        spans
    } else if state.status == RunStatus::Failed {
        vec![Span::styled(
            format!("  {} 操作未完成 · 请查看上方错误", icons::CLOSE),
            Style::default().fg(ACCENT),
        )]
    } else {
        Vec::new()
    };
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_composer(
    frame: &mut Frame<'_>,
    state: &mut UiState,
    area: Rect,
    lines: Vec<String>,
    cursor: super::input::InputPosition,
    strip: super::media::thumbnails::Strip,
) {
    let area = Rect {
        x: area.x + 2,
        width: area
            .width
            .saturating_sub(4 + super::navigation::controls_width(state)),
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
        height: inner
            .height
            .saturating_sub(1 + details_height),
        ..inner
    };
    let thumbnail_width = inner.width;
    let attachment_height = (strip.rows.len() as u16)
        .saturating_add(u16::from(!strip.rows.is_empty()))
        .min(inner.height.saturating_sub(1));

    if attachment_height > 0 {
        if state
            .attachment_cursor
            .is_some_and(|index| index >= state.attachments.len())
        {
            state.attachment_cursor = None;
        }

        let mut composer_media = state.media.clone();
        composer_media.files = None;
        let origin = (inner.x, inner.y);
        // When a short terminal cannot fit every row, keep the focused image visible.
        let selected_y = state
            .attachment_cursor
            .and_then(|index| strip.areas.get(index))
            .map_or(0, |area| area.bottom());
        let offset = selected_y.saturating_sub(attachment_height.saturating_sub(1));
        state
            .attachment_areas
            .resize(state.attachments.len(), Rect::default());
        for (index, slot) in strip.areas.iter().enumerate() {
            if slot.y < offset || slot.bottom() - offset > attachment_height.saturating_sub(1) {
                continue;
            }
            let chip = Rect::new(
                origin.0 + slot.x,
                origin.1 + slot.y - offset,
                slot.width,
                slot.height,
            );
            let style = if state.attachment_cursor == Some(index) {
                Style::default()
                    .fg(crate::tui::theme::THEME.on_accent)
                    .bg(ACCENT)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(ACCENT)
            };

            frame.render_widget(Block::default().style(style), chip);
            // Preserve attachment indexes even when some rows are clipped.
            state.attachment_areas[index] = chip;
        }
        for (row, line) in strip
            .rows
            .iter()
            .enumerate()
            .skip(usize::from(offset))
            .take(usize::from(attachment_height.saturating_sub(1)))
        {
            let y = origin.1 + row as u16 - offset;
            frame.render_widget(
                Paragraph::new(line.line.clone()).style(Style::default().fg(ACCENT)),
                Rect::new(origin.0, y, thumbnail_width, 1),
            );
            for request in &line.media_requests {
                composer_media.picture(&request.source, request.width, request.height, false);
            }
            for fragment in &line.graphics {
                let area = Rect::new(origin.0 + fragment.x, y, fragment.picture.size.width, 1);
                let focused = state
                    .attachment_cursor
                    .and_then(|index| state.attachment_areas.get(index))
                    .is_some_and(|chip| chip.contains((area.x, area.y).into()));
                super::media::schedule_selected(fragment, area, focused);
            }
        }
    }

    let prompt_area = Rect {
        y: inner.y + attachment_height,
        height: inner
            .height
            .saturating_sub(attachment_height),
        ..inner
    };

    draw_prompt(frame, state, prompt_area, lines, cursor);
}

pub(super) fn draw_prompt(
    frame: &mut Frame<'_>,
    state: &mut UiState,
    prompt_area: Rect,
    lines: Vec<String>,
    cursor: super::input::InputPosition,
) {
    frame.render_widget(
        Paragraph::new(Span::styled(icons::PROMPT, Style::default().fg(ACCENT))),
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
        .row
        .saturating_sub(text_area.height.saturating_sub(1) as usize);
    // The right padding also holds the caret before a newline after a full row.
    state.input_area = Rect {
        width: (text_area.width + 1).min(
            frame
                .area()
                .right()
                .saturating_sub(text_area.x),
        ),
        ..text_area
    };
    state.input_scroll = scroll;
    state.ime_cursor = None;
    // IME preedit belongs to the terminal; placeholder text underneath it is misleading.
    let text = Text::from(
        lines
            .into_iter()
            .skip(scroll)
            .map(Line::from)
            .collect::<Vec<_>>(),
    );

    frame.render_widget(Paragraph::new(text), text_area);

    for (row, columns) in state
        .input
        .selection_cells(state.input_width)
    {
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
        && !state.dialog_open()
        && state.workspace_wizard.is_none()
        && state.ssh_prompt.is_none()
        && state.attachment_cursor.is_none()
    {
        state.ime_cursor = cursor.in_viewport(state.input_area, scroll);
    }
}

fn draw_panel(frame: &mut Frame<'_>, state: &mut UiState, area: Rect) {
    let menu = state.menu();

    if menu.is_empty() && state.directory_completion_pending() {
        super::overlay::clear(frame, area);
        frame.render_widget(
            Paragraph::new("   读取目录…").style(Style::default().fg(THEME.secondary)),
            area,
        );
        return;
    }

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
            .max(
                state
                    .menu_index
                    .saturating_sub(visible.saturating_sub(1)),
            )
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
        let row = completion_row(
            frame,
            Rect::new(area.x, area.y + row as u16, area.width, 1),
            command
                .display
                .as_deref()
                .unwrap_or(&command.name),
            &command.description,
            selected,
        );
        state.menu_rows.push((row, index));
    }
}

pub(super) fn completion_row(
    frame: &mut Frame<'_>,
    area: Rect,
    label: &str,
    description: &str,
    selected: bool,
) -> Rect {
    let text = format!(
        "{:<width$} {}",
        safe_output(label),
        safe_output(description),
        width = super::commands::COMMAND_NAME_WIDTH,
    );
    let text = padded_label(text.trim_end(), area.width as usize, 3);
    let row = Rect {
        width: text.width().min(area.width as usize) as u16,
        ..area
    };
    frame.render_widget(
        Paragraph::new(text).style(if selected {
            Style::default()
                .fg(THEME.on_accent)
                .bg(ACCENT)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(MUTED)
        }),
        row,
    );
    row
}

#[cfg(test)]
mod tests;
