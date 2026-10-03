use super::{
    app::{TranscriptItem, UiState},
    render,
    theme::THEME,
};
use std::time::Instant;
use unicode_width::UnicodeWidthStr;

use crossterm::event::{Event, MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    widgets::Paragraph,
};

#[derive(Debug, Default)]
pub(super) struct Navigation {
    area: Rect,
    tooltip_area: Rect,
    targets: Vec<(u16, usize, String)>,
    hovered: Option<u16>,
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
    let inside = nav.area.contains((mouse.column, mouse.row).into());
    let bottom = nav.area.bottom().saturating_sub(1);
    let slots = nav.area.height.saturating_sub(1) as usize;
    let hit_bottom = (nav.show_latest || (nav.first + slots) < nav.count) && mouse.row == bottom;
    let hit = inside && (hit_bottom || nav.targets.iter().any(|(row, _, _)| *row == mouse.row));

    if mouse.kind == MouseEventKind::Moved {
        let hovered = hit.then_some(mouse.row);
        let changed = nav.hovered != hovered;
        nav.hovered = hovered;
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
        MouseEventKind::Down(MouseButton::Left) if hit => {
            state.selection.clear();

            if hit_bottom {
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
            if let TranscriptItem::User(text) = item {
                Some((state.transcript_layout.item_offset(i)?, text))
            } else {
                None
            }
        })
        .collect();
    let nav = &mut state.navigation;
    nav.area = Rect::new(area.right().saturating_sub(2), area.y, 2, area.height);
    nav.targets.clear();
    nav.count = anchors.len();
    nav.show_latest = offset < state.max_scroll;
    let slots = area.height.saturating_sub(1) as usize;

    if slots == 0 || anchors.is_empty() {
        nav.area = Rect::default();
        return;
    }

    let active = if state.follow_bottom || anchors.is_empty() {
        anchors.len().saturating_sub(1)
    } else {
        let focus = offset + (area.height as usize / 2);
        anchors
            .partition_point(|(start, _)| *start <= focus)
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

    for (j, (start, text)) in anchors.iter().skip(nav.first).take(slots).enumerate() {
        let index = nav.first + j;
        let y = first_row + j as u16;
        let hovered = nav.hovered == Some(y);
        let is_active = index == active;
        let symbol = if hovered {
            "◆"
        } else if is_active {
            "●"
        } else {
            "·"
        };

        let style = if hovered {
            Style::default()
                .fg(crate::tui::theme::THEME.md_bold)
                .add_modifier(Modifier::BOLD)
        } else if is_active {
            Style::default()
                .fg(THEME.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(crate::tui::theme::THEME.navigation_muted)
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

    if nav.show_latest || has_more_below {
        frame.render_widget(
            Paragraph::new("↓").style(Style::default().fg(if nav.hovered == Some(bottom) {
                crate::tui::theme::THEME.accent
            } else {
                crate::tui::theme::THEME.secondary
            })),
            Rect::new(nav.area.x, bottom, 1, 1),
        );
    } else if nav.hovered == Some(bottom) {
        nav.hovered = None;
    }

    let tooltip = if nav.show_latest && nav.hovered == Some(bottom) {
        Some((bottom, "回到最新 · 继续跟随输出".to_owned()))
    } else if has_more_below && nav.hovered == Some(bottom) {
        Some((
            bottom,
            format!(
                "下方还有 {} 轮对话 · 点击向下滚动",
                anchors.len().saturating_sub(nav.first + slots)
            ),
        ))
    } else {
        nav.targets
            .iter()
            .find(|(y, _, _)| nav.hovered == Some(*y))
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
mod tests {
    use super::*;
    use crossterm::event::{KeyModifiers, MouseEvent};
    use ratatui::{Terminal, backend::TestBackend};

    fn mouse(kind: MouseEventKind, x: u16, y: u16) -> Event {
        Event::Mouse(MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        })
    }

    #[test]
    fn hover_jump_and_latest_preserve_the_draft_and_stream_anchor() {
        let mut state = UiState::new("model");

        for i in 0..3 {
            state
                .transcript
                .push(TranscriptItem::User(format!("问题 {i}")));
            state
                .transcript
                .push(TranscriptItem::Assistant("answer\n".repeat(30)));
        }

        state.input.insert("保留草稿");
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render::draw(f, &mut state)).unwrap();
        let bottom = state.navigation.area.bottom() - 1;

        assert_eq!(
            terminal.backend().buffer()[(state.navigation.area.x, bottom)].symbol(),
            " "
        );

        let x = state.navigation.area.x;
        let (y, target, _) = state.navigation.targets[1].clone();
        let hover = mouse(MouseEventKind::Moved, x, y);

        assert_eq!(handle(&mut state, &hover), Some(true));
        assert_eq!(handle(&mut state, &hover), Some(false));

        terminal.draw(|f| render::draw(f, &mut state)).unwrap();
        let screen: String = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|c| c.symbol())
            .collect();

        assert!(screen.replace(' ', "").contains("问题1"));
        assert!(!screen.replace(' ', "").contains("第2轮"));
        assert_eq!(
            handle(
                &mut state,
                &mouse(MouseEventKind::Down(MouseButton::Left), x, y)
            ),
            Some(true)
        );

        terminal.draw(|f| render::draw(f, &mut state)).unwrap();

        assert_eq!(state.max_scroll - state.scroll, target);
        assert!(!state.follow_bottom);

        let last = state.transcript.len() - 1;

        if let TranscriptItem::Assistant(text) = &mut state.transcript[last] {
            text.push_str("more\n");
        }

        state.transcript_layout.invalidate_from(last);
        terminal.draw(|f| render::draw(f, &mut state)).unwrap();

        assert_eq!(state.max_scroll - state.scroll, target);

        let bottom = state.navigation.area.bottom() - 1;

        assert_eq!(terminal.backend().buffer()[(x, bottom)].symbol(), "↓");

        handle(
            &mut state,
            &mouse(MouseEventKind::Down(MouseButton::Left), x, bottom),
        );

        assert!(state.follow_bottom);
        assert_eq!(state.scroll, 0);
        assert_eq!(state.input.text, "保留草稿");
        assert_eq!(
            handle(&mut state, &mouse(MouseEventKind::Moved, 5, 5)),
            Some(true)
        );
        assert_eq!(
            handle(&mut state, &mouse(MouseEventKind::Moved, 6, 5)),
            Some(false)
        );
    }

    #[test]
    fn opening_an_overlay_clears_a_navigation_preview() {
        let mut state = UiState::new("model");
        state.transcript.push(TranscriptItem::User("first".into()));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render::draw(f, &mut state)).unwrap();
        let (row, _, _) = state.navigation.targets[0].clone();
        let column = state.navigation.area.x;

        assert_eq!(
            handle(&mut state, &mouse(MouseEventKind::Moved, column, row)),
            Some(true)
        );
        assert_eq!(state.navigation.hovered, Some(row));

        state.show_help = true;
        terminal.draw(|f| render::draw(f, &mut state)).unwrap();

        assert_eq!(state.navigation.hovered, None);
    }

    #[test]
    fn leaving_a_dot_erases_its_rendered_preview() {
        let mut state = UiState::new("model");
        state.transcript.push(TranscriptItem::User("first".into()));
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|f| render::draw(f, &mut state)).unwrap();
        let (row, _, _) = state.navigation.targets[0].clone();
        let column = state.navigation.area.x;
        handle(&mut state, &mouse(MouseEventKind::Moved, column, row));
        terminal.draw(|f| render::draw(f, &mut state)).unwrap();
        let shown = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        assert!(shown.replace(' ', "").contains("first"), "{shown}");
        assert!(!shown.replace(' ', "").contains("第1轮"), "{shown}");
        assert_ne!(state.navigation.tooltip_area, Rect::default());

        handle(&mut state, &mouse(MouseEventKind::Moved, 0, 0));
        terminal.draw(|f| render::draw(f, &mut state)).unwrap();

        assert_eq!(state.navigation.tooltip_area, Rect::default());
    }

    #[test]
    fn preview_is_at_most_one_quarter_of_window_and_scrolls_long_text() {
        let mut state = UiState::new("model");
        state.transcript.push(TranscriptItem::User(
            "abcdefghijklmnopqrstuvwxyz 中文预览内容".repeat(3),
        ));
        let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
        terminal
            .draw(|frame| render::draw(frame, &mut state))
            .unwrap();
        let (row, _, _) = state.navigation.targets[0].clone();
        let column = state.navigation.area.x;
        handle(&mut state, &mouse(MouseEventKind::Moved, column, row));
        terminal
            .draw(|frame| render::draw(frame, &mut state))
            .unwrap();
        let area = state.navigation.tooltip_area;
        let before = terminal.backend().buffer()[(area.x, area.y)]
            .symbol()
            .to_owned();

        assert_eq!(area.width, 22);
        state.navigation.preview_scroll.as_mut().unwrap().2 =
            Instant::now() - std::time::Duration::from_secs(1);
        assert!(state.navigation.advance_preview());
        terminal
            .draw(|frame| render::draw(frame, &mut state))
            .unwrap();
        assert_ne!(
            terminal.backend().buffer()[(area.x, area.y)].symbol(),
            before
        );

        state.navigation.clear_hover();
        assert!(!state.navigation.advance_preview());
    }

    #[test]
    fn crowded_navigation_reaches_all_rounds_and_resize_rebuilds_targets() {
        let mut state = UiState::new("model");
        state.transcript = (0..100)
            .map(|i| TranscriptItem::User(format!("round {i}")))
            .collect();
        let mut terminal = Terminal::new(TestBackend::new(50, 15)).unwrap();
        terminal.draw(|f| render::draw(f, &mut state)).unwrap();

        for _ in 0..40 {
            let area = state.navigation.area;
            handle(
                &mut state,
                &mouse(MouseEventKind::ScrollDown, area.x, area.y),
            );
            terminal.draw(|f| render::draw(f, &mut state)).unwrap();
        }

        assert!(
            state
                .navigation
                .targets
                .last()
                .unwrap()
                .2
                .contains("round 99")
        );

        let mut larger = Terminal::new(TestBackend::new(100, 30)).unwrap();
        larger.draw(|f| render::draw(f, &mut state)).unwrap();

        for (y, offset, _) in &state.navigation.targets {
            assert!(*y < state.navigation.area.bottom() - 1);
            assert!(*offset < state.transcript_layout.height());
        }
    }

    #[test]
    fn output_keeps_a_blank_column_before_navigation() {
        let mut state = UiState::new("model");
        state.transcript.push(TranscriptItem::User("x".repeat(200)));
        let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
        terminal.draw(|f| render::draw(f, &mut state)).unwrap();

        let gap = state.navigation.area.x - 1;

        for y in state.navigation.area.y..state.navigation.area.bottom() {
            assert_eq!(terminal.backend().buffer()[(gap, y)].symbol(), " ");
        }
    }

    #[test]
    fn navigation_auto_follows_latest_rounds_when_crowded() {
        let mut state = UiState::new("model");
        state.transcript = (0..50)
            .map(|i| TranscriptItem::User(format!("round {i}")))
            .collect();
        let mut terminal = Terminal::new(TestBackend::new(50, 15)).unwrap();
        terminal.draw(|f| render::draw(f, &mut state)).unwrap();

        assert!(
            state
                .navigation
                .targets
                .last()
                .unwrap()
                .2
                .contains("round 49")
        );
        assert_eq!(state.navigation.first, 50 - state.navigation.targets.len());
        assert_eq!(
            state.navigation.targets.len(),
            usize::from(state.navigation.area.height - 1)
        );
        assert_eq!(
            terminal.backend().buffer()[(state.navigation.area.x, state.navigation.area.y)]
                .symbol(),
            "·"
        );
        let area = state.navigation.area;
        for _ in 0..25 {
            handle(&mut state, &mouse(MouseEventKind::ScrollUp, area.x, area.y));
            terminal.draw(|f| render::draw(f, &mut state)).unwrap();
        }
        assert_eq!(state.navigation.first, 0);
        assert!(
            state
                .navigation
                .targets
                .first()
                .unwrap()
                .2
                .contains("round 0")
        );
    }

    #[test]
    fn active_dot_is_always_visible_in_navigation_targets() {
        let mut state = UiState::new("model");
        state.transcript = (0..60)
            .map(|i| TranscriptItem::User(format!("round {i}")))
            .collect();
        let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();

        terminal.draw(|f| render::draw(f, &mut state)).unwrap();
        let buffer = terminal.backend().buffer();
        let has_active = state
            .navigation
            .targets
            .iter()
            .any(|(y, _, _)| buffer[(state.navigation.area.x, *y)].symbol() == "●");
        assert!(has_active, "Active dot must be visible at bottom");

        state.follow_bottom = false;
        state.scroll = 100;
        terminal.draw(|f| render::draw(f, &mut state)).unwrap();
        let buffer = terminal.backend().buffer();
        let has_active = state
            .navigation
            .targets
            .iter()
            .any(|(y, _, _)| buffer[(state.navigation.area.x, *y)].symbol() == "●");
        assert!(
            has_active,
            "Active dot must be visible when scrolled to middle"
        );

        state.scroll = state.max_scroll;
        terminal.draw(|f| render::draw(f, &mut state)).unwrap();
        let buffer = terminal.backend().buffer();
        let has_active = state
            .navigation
            .targets
            .iter()
            .any(|(y, _, _)| buffer[(state.navigation.area.x, *y)].symbol() == "●");
        assert!(
            has_active,
            "Active dot must be visible when scrolled to top"
        );
    }
}
