use super::{
    app::{TranscriptItem, UiState},
    render,
};
use crossterm::event::{Event, MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Style},
    widgets::{Clear, Paragraph},
};

#[derive(Debug, Default)]
pub(super) struct Navigation {
    area: Rect,
    targets: Vec<(u16, usize, String)>,
    hovered: Option<u16>,
    first: usize,
    count: usize,
}

impl Navigation {
    pub(super) fn reset_area(&mut self) {
        self.area = Rect::default();
    }
}

/// None means normal input; Some(false) consumes motion without a redraw.
pub(super) fn handle(state: &mut UiState, event: &Event) -> Option<bool> {
    let Event::Mouse(mouse) = event else {
        return None;
    };
    let nav = &mut state.navigation;
    let inside = nav.area.contains((mouse.column, mouse.row).into());
    let hit = inside
        && (mouse.row == nav.area.bottom().saturating_sub(1)
            || nav.targets.iter().any(|(row, _, _)| *row == mouse.row));
    if mouse.kind == MouseEventKind::Moved {
        let hovered = hit.then_some(mouse.row);
        let changed = nav.hovered != hovered;
        nav.hovered = hovered;
        return Some(changed);
    }
    if !inside {
        nav.hovered = None;
        return None;
    }
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) if hit => {
            state.selection.clear();
            if mouse.row == nav.area.bottom() - 1 {
                state.follow_bottom = true;
                state.scroll = 0;
            } else if let Some((_, offset, _)) =
                nav.targets.iter().find(|(row, _, _)| *row == mouse.row)
            {
                state.follow_bottom = false;
                state.scroll = state.max_scroll.saturating_sub(*offset);
            }
            Some(true)
        }
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let slots = nav.area.height.saturating_sub(2) as usize;
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
    let slots = area.height.saturating_sub(2) as usize;
    if slots == 0 || anchors.is_empty() {
        nav.area = Rect::default();
        return;
    }
    nav.first = nav.first.min(anchors.len().saturating_sub(slots));
    let shown = (anchors.len() - nav.first).min(slots);
    let first_row = area.y + ((slots - shown) / 2) as u16;
    let active = anchors
        .partition_point(|(start, _)| *start <= offset)
        .saturating_sub(1);
    for (j, (start, text)) in anchors.iter().skip(nav.first).take(slots).enumerate() {
        let index = nav.first + j;
        let y = first_row + j as u16;
        let hovered = nav.hovered == Some(y);
        let symbol = if hovered {
            "◆"
        } else if index == active {
            "●"
        } else {
            "·"
        };
        let color = if hovered || index == active {
            Color::Rgb(218, 139, 109)
        } else {
            Color::Rgb(150, 150, 150)
        };
        frame.render_widget(
            Paragraph::new(symbol).style(Style::default().fg(color)),
            Rect::new(nav.area.x, y, 1, 1),
        );
        nav.targets.push((
            y,
            *start,
            format!(
                "第 {} 轮 · {}",
                index + 1,
                render::safe_output(&text.replace(['\n', '\r'], " "))
            ),
        ));
    }
    let bottom = nav.area.bottom() - 1;
    frame.render_widget(
        Paragraph::new("↓").style(Style::default().fg(if nav.hovered == Some(bottom) {
            Color::Rgb(218, 139, 109)
        } else {
            Color::Rgb(170, 170, 170)
        })),
        Rect::new(nav.area.x, bottom, 1, 1),
    );
    let tooltip = if nav.hovered == Some(bottom) {
        Some((bottom, "回到最新 · 继续跟随输出".to_owned()))
    } else {
        nav.targets
            .iter()
            .find(|(y, _, _)| nav.hovered == Some(*y))
            .map(|(y, _, text)| (*y, text.clone()))
    };
    if let Some((y, text)) = tooltip {
        let width = area.width.saturating_sub(5).min(52);
        let tooltip_area = Rect::new(nav.area.x.saturating_sub(width + 1), y, width, 1);
        frame.render_widget(Clear, tooltip_area);
        frame.render_widget(
            Paragraph::new(render::fit_label(&text, width as usize, false)).style(
                Style::default()
                    .fg(Color::Rgb(222, 218, 212))
                    .bg(Color::Rgb(48, 48, 52)),
            ),
            tooltip_area,
        );
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
        assert!(screen.replace(' ', "").contains("第2轮·问题1"));
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
}
