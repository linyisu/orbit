//! Text and image links share anchored tooltip placement and presentation.
use crossterm::event::{Event, MouseEventKind};
use ratatui::{Frame, layout::Rect, style::Style, widgets::Paragraph};
use unicode_width::UnicodeWidthStr;

use crate::tui::{app::UiState, overlay, subagent_link_id, theme::THEME};

/// Keep the actual pointer for hit testing after streaming, scrolling or resize,
/// but moving within one link does not change its hover presentation.
pub(in crate::tui) fn handle_link_hover(state: &mut UiState, event: &Event) -> Option<bool> {
    let Event::Mouse(mouse) = event else {
        return None;
    };
    if mouse.kind != MouseEventKind::Moved {
        return None;
    }

    let offset = if state.follow_bottom {
        state.max_scroll
    } else {
        state.max_scroll.saturating_sub(state.scroll)
    };
    let viewport = state.selection.viewport;
    let target = |point: (u16, u16)| {
        state
            .transcript_layout
            .link_hit(viewport, offset, point)
            .map(|hit| (hit.url, hit.area))
    };
    let point = (mouse.column, mouse.row);
    let before = state.link_hovered.and_then(target);
    let after = target(point);
    let changed = before != after;
    let hovered = after.is_some();
    state.link_hovered = hovered.then_some(point);

    Some(changed)
}

pub(super) fn clear_previous_link_tooltip(frame: &mut Frame<'_>, state: &mut UiState) {
    if !state.link_tooltip_area.is_empty() {
        overlay::clear(frame, state.link_tooltip_area);
    }
    state.link_tooltip_area = Rect::default();
}

pub(super) fn draw_link_tooltip(
    frame: &mut Frame<'_>,
    state: &mut UiState,
    surface: Rect,
    offset: usize,
) {
    let Some(point) = state.link_hovered else {
        return;
    };
    let viewport = state.selection.viewport;
    let Some(hit) = state.transcript_layout.link_hit(viewport, offset, point) else {
        state.link_hovered = None;
        return;
    };
    if crate::tui::markdown::footnotes::is_target(hit.url) {
        return;
    }

    let text = if subagent_link_id(hit.url).is_some() {
        "单击查看子代理"
    } else {
        "Ctrl + Click 跳转"
    };
    // Keep the hint below the entire subject, including the navigation footer
    // when the link itself is on the final visible row.
    let surface = Rect {
        height: surface.height,
        ..viewport
    };
    let Some(area) = tooltip_area(
        surface.intersection(frame.area()),
        hit.area,
        text.width() as u16 + 2,
    ) else {
        return;
    };

    overlay::clear(frame, area);
    frame.render_widget(
        Paragraph::new(super::padded_label(text, usize::from(area.width), 1)).style(
            Style::default()
                .fg(THEME.overlay_foreground)
                .bg(THEME.overlay_background),
        ),
        area,
    );
    state.link_tooltip_area = area;
}

fn tooltip_area(surface: Rect, anchor: Rect, width: u16) -> Option<Rect> {
    let width = width.min(surface.width);
    if width == 0 || surface.height == 0 {
        return None;
    }
    let x = anchor.x.clamp(surface.x, surface.right() - width);
    let area = Rect::new(x, anchor.bottom(), width, 1);
    // Other content may be covered temporarily. Never cover the hovered object
    // or switch above/beside it just because the viewport has run out of room.
    (area.intersection(surface) == area && area.intersection(anchor).is_empty()).then_some(area)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tooltip_does_not_switch_sides_at_the_bottom_edge() {
        let surface = Rect::new(4, 2, 32, 10);
        for anchor in [Rect::new(20, 7, 4, 1), Rect::new(8, 3, 26, 5)] {
            let hint = tooltip_area(surface, anchor, 20).unwrap();
            assert_eq!(hint.y, anchor.bottom());
            assert!(hint.intersection(anchor).is_empty());
        }

        for anchor in [Rect::new(20, 11, 4, 1), Rect::new(8, 3, 26, 9)] {
            assert!(tooltip_area(surface, anchor, 20).is_none());
        }
    }
}
