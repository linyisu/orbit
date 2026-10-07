use std::time::Instant;

use crossterm::event::{Event, MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use unicode_width::UnicodeWidthStr;

use super::{
    app::{TranscriptItem, UiState},
    icons, render,
    theme::THEME,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Round(u16),
    Latest,
    Current,
}

#[derive(Debug, Default)]
pub(super) struct Navigation {
    area: Rect,
    current_area: Rect,
    tooltip_area: Rect,
    targets: Vec<(u16, usize, String)>,
    hovered: Option<Target>,
    preview_scroll: Option<(String, u16, Instant)>,
    preview_tick: u128,
    show_latest: bool,
    first: usize,
    count: usize,
    manual_scroll: bool,
}

impl Navigation {
    pub(super) fn reset_area(&mut self) {
        self.area = Rect::default();
        self.current_area = Rect::default();
        self.clear_hover();
    }

    pub(super) fn clear_hover(&mut self) {
        self.hovered = None;
        self.preview_scroll = None;
    }

    pub(super) fn advance_preview(&mut self) -> bool {
        let Some((_, _, since)) = &self.preview_scroll else {
            return false;
        };
        let tick = since.elapsed().as_millis().saturating_sub(100) / 120;
        let changed = tick != self.preview_tick;
        self.preview_tick = tick;
        changed
    }
}

/// Remove the tooltip from the previous frame before the transcript redraws
/// beneath it. Ratatui updates only changed cells, so merely omitting the new
/// tooltip would otherwise leave its old text in the terminal buffer.
pub(super) fn clear_previous_tooltip(frame: &mut Frame<'_>, state: &mut UiState) {
    let area = state.navigation.tooltip_area;

    if area.width != 0 && area.height != 0 {
        super::overlay::clear(frame, area);
    }

    state.navigation.tooltip_area = Rect::default();
}

/// None means normal input; Some(false) consumes motion without a redraw.
pub(super) fn handle(state: &mut UiState, event: &Event) -> Option<bool> {
    let Event::Mouse(mouse) = event else {
        return None;
    };

    let nav = &mut state.navigation;
    let point = (mouse.column, mouse.row).into();
    let inside_rail = nav.area.contains(point);
    let inside_current = nav.current_area.contains(point);
    let inside = inside_rail || inside_current;
    let bottom = nav.area.bottom().saturating_sub(1);
    let slots = nav.area.height.saturating_sub(1) as usize;
    let hit_bottom = (nav.show_latest || (nav.first + slots) < nav.count) && mouse.row == bottom;
    let target = if inside_current {
        Some(Target::Current)
    } else if inside_rail && hit_bottom {
        Some(Target::Latest)
    } else if inside_rail && nav.targets.iter().any(|(row, _, _)| *row == mouse.row) {
        Some(Target::Round(mouse.row))
    } else {
        None
    };

    if mouse.kind == MouseEventKind::Moved {
        let changed = nav.hovered != target;
        nav.hovered = target;
        if changed {
            nav.preview_scroll = None;
        }
        return Some(changed);
    }

    if !inside {
        nav.clear_hover();
        return None;
    }

    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) if target.is_some() => {
            if target == Some(Target::Current) {
                nav.manual_scroll = false;
            } else if target == Some(Target::Latest) {
                state.selection.clear();
                if nav.show_latest {
                    state.follow_bottom = true;
                    state.scroll = 0;
                    nav.manual_scroll = false;
                } else {
                    nav.manual_scroll = true;
                    nav.first = (nav.first + 5).min(nav.count.saturating_sub(slots));
                    if let Some((_, offset, _)) = nav.targets.last() {
                        state.follow_bottom = false;
                        state.scroll = state.max_scroll.saturating_sub(*offset);
                    }
                }
            } else if let Some((_, offset, _)) =
                nav.targets.iter().find(|(row, _, _)| *row == mouse.row)
            {
                state.selection.clear();
                state.follow_bottom = false;
                state.scroll = state.max_scroll.saturating_sub(*offset);
            }

            Some(true)
        }

        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            nav.manual_scroll = true;
            let slots = nav.area.height.saturating_sub(1) as usize;
            let old = nav.first;
            nav.first = if mouse.kind == MouseEventKind::ScrollUp {
                nav.first.saturating_sub(3)
            } else {
                (nav.first + 3).min(nav.count.saturating_sub(slots))
            };

            Some(old != nav.first)
        }

        MouseEventKind::Down(_) | MouseEventKind::Up(_) | MouseEventKind::Drag(_) => Some(false),
        _ => None,
    }
}

pub(super) fn draw(frame: &mut Frame<'_>, state: &mut UiState, area: Rect, offset: usize) {
    let anchors: Vec<_> = state
        .transcript
        .iter()
        .enumerate()
        .filter_map(|(i, item)| {
            if let TranscriptItem::User(text) | TranscriptItem::UserWithImages { text, .. } = item {
                Some((state.transcript_layout.item_offset(i)?, text))
            } else {
                None
            }
        })
        .collect();
    let nav = &mut state.navigation;
    let rail_width = area.width.min(2);
    nav.area = Rect::new(area.right() - rail_width, area.y, rail_width, area.height);
    nav.current_area = Rect::default();
    nav.targets.clear();
    nav.count = anchors.len();
    nav.show_latest = offset < state.max_scroll;
    let slots = area.height.saturating_sub(1) as usize;

    if slots == 0 || anchors.is_empty() || rail_width == 0 {
        nav.reset_area();
        return;
    }

    let active = if state.follow_bottom || anchors.is_empty() {
        anchors.len().saturating_sub(1)
    } else {
        anchors
            .partition_point(|(start, _)| *start <= offset)
            .saturating_sub(1)
    };

    if state.follow_bottom && !nav.manual_scroll {
        nav.first = anchors.len().saturating_sub(slots);
    } else if !nav.manual_scroll {
        nav.first = active
            .saturating_sub(slots / 2)
            .min(anchors.len().saturating_sub(slots));
    }

    nav.first = nav.first.min(anchors.len().saturating_sub(slots));
    let shown = (anchors.len() - nav.first).min(slots);
    let first_row = area.y + ((slots - shown) / 2) as u16;

    if anchors.len() > slots && rail_width > 1 {
        let thumb_height = (slots * slots / anchors.len()).max(1);
        let thumb_start = nav.first * (slots - thumb_height) / (anchors.len() - slots);
        for row in 0..slots {
            let in_thumb = (thumb_start..thumb_start + thumb_height).contains(&row);
            frame.render_widget(
                Paragraph::new(if in_thumb { "┃" } else { "│" }).style(Style::default().fg(
                    if in_thumb {
                        THEME.navigation_muted
                    } else {
                        THEME.border_muted
                    },
                )),
                Rect::new(nav.area.x + 1, area.y + row as u16, 1, 1),
            );
        }
    }

    for (j, (start, text)) in anchors.iter().skip(nav.first).take(slots).enumerate() {
        let index = nav.first + j;
        let y = first_row + j as u16;
        let hovered = nav.hovered == Some(Target::Round(y));
        let is_active = index == active;
        // Use the matching filled/outline circles: the small-circle glyph has
        // a different ink center in several Nerd Font faces.
        let symbol = if is_active {
            icons::CIRCLE
        } else if hovered {
            icons::RHOMBUS
        } else {
            icons::CIRCLE_OUTLINE
        };

        let style = if is_active {
            Style::default()
                .fg(THEME.accent)
                .add_modifier(Modifier::BOLD)
        } else if hovered {
            Style::default()
                .fg(crate::tui::theme::THEME.md_bold)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(THEME.navigation_palette[index % THEME.navigation_palette.len()])
        };

        frame.render_widget(
            Paragraph::new(symbol).style(style),
            Rect::new(nav.area.x, y, 1, 1),
        );
        nav.targets.push((
            y,
            *start,
            render::safe_output(&text.replace(['\n', '\r'], " ")),
        ));
    }

    let bottom = nav.area.bottom() - 1;
    let has_more_below = (nav.first + slots) < anchors.len();

    let current = format!("当前 {}/{}", active + 1, anchors.len());
    let range = format!("{}–{} · ", nav.first + 1, nav.first + shown);
    let available = area.width.saturating_sub(rail_width + 4) as usize;
    let label = if range.width() + current.width() <= available {
        Line::from(vec![
            Span::styled(range, Style::default().fg(THEME.navigation_muted)),
            Span::styled(current, Style::default().fg(THEME.accent)),
        ])
    } else {
        Line::from(Span::styled(
            render::fit_label(&current, available, false),
            Style::default().fg(THEME.accent),
        ))
    };
    let label_width = label.width() as u16;
    if label_width > 0 {
        nav.current_area = Rect::new(nav.area.x - label_width - 1, bottom, label_width, 1);
        frame.render_widget(Paragraph::new(label), nav.current_area);
    }

    if nav.show_latest || has_more_below {
        frame.render_widget(
            Paragraph::new(icons::ARROW_DOWN).style(Style::default().fg(
                if nav.hovered == Some(Target::Latest) {
                    crate::tui::theme::THEME.accent
                } else {
                    crate::tui::theme::THEME.secondary
                },
            )),
            Rect::new(nav.area.x, bottom, 1, 1),
        );
    } else if nav.hovered == Some(Target::Latest) {
        nav.hovered = None;
    }

    let tooltip = if nav.hovered == Some(Target::Current) {
        Some((
            bottom - 1,
            format!("当前第 {}/{} 轮 · 点击定位", active + 1, anchors.len()),
        ))
    } else if nav.show_latest && nav.hovered == Some(Target::Latest) {
        Some((bottom - 1, "回到最新 · 继续跟随输出".to_owned()))
    } else if has_more_below && nav.hovered == Some(Target::Latest) {
        Some((
            bottom - 1,
            format!(
                "下方还有 {} 轮对话 · 点击向下滚动",
                anchors.len().saturating_sub(nav.first + slots)
            ),
        ))
    } else {
        nav.targets
            .iter()
            .find(|(y, _, _)| nav.hovered == Some(Target::Round(*y)))
            .map(|(y, _, text)| (*y, text.clone()))
    };

    if let Some((y, text)) = tooltip {
        let width = (frame.area().width / 4)
            .min(area.width.saturating_sub(6))
            .min(text.width().min(u16::MAX as usize) as u16);
        if width == 0 {
            return;
        }
        let elapsed = if text.width() > width as usize {
            if !nav
                .preview_scroll
                .as_ref()
                .is_some_and(|(previous, old_width, _)| previous == &text && *old_width == width)
            {
                nav.preview_scroll = Some((text.clone(), width, Instant::now()));
                nav.preview_tick = 0;
            }

            nav.preview_scroll
                .as_ref()
                .map(|(_, _, since)| since.elapsed())
        } else {
            nav.preview_scroll = None;
            None
        };
        let tooltip_area = Rect::new(nav.area.x.saturating_sub(width + 1), y, width, 1);
        super::overlay::clear(frame, tooltip_area);
        frame.render_widget(
            Paragraph::new(render::scrolling_title(&text, width as usize, elapsed)).style(
                Style::default()
                    .fg(crate::tui::theme::THEME.overlay_foreground)
                    .bg(crate::tui::theme::THEME.overlay_background),
            ),
            tooltip_area,
        );
        nav.tooltip_area = tooltip_area;
    } else {
        nav.preview_scroll = None;
    }
}

#[cfg(test)]
mod tests;
