use crossterm::event::{KeyModifiers, MouseEvent};
use ratatui::{Terminal, backend::TestBackend};

use super::*;

fn mouse(kind: MouseEventKind, x: u16, y: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    })
}

fn row_text(buffer: &ratatui::buffer::Buffer, area: Rect) -> String {
    use ratatui::buffer::CellWidth;
    let mut text = String::new();
    let mut x = area.x;
    while x < area.right() {
        let cell = &buffer[(x, area.y)];
        text.push_str(cell.symbol());
        x += cell.cell_width().max(1);
    }

    text
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
    terminal
        .draw(|f| render::draw(f, &mut state))
        .unwrap();
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

    terminal
        .draw(|f| render::draw(f, &mut state))
        .unwrap();
    let screen: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|c| c.symbol())
        .collect();

    assert!(
        screen
            .replace(' ', "")
            .contains("问题1")
    );
    assert!(
        !screen
            .replace(' ', "")
            .contains("第2轮")
    );
    assert_eq!(
        handle(
            &mut state,
            &mouse(MouseEventKind::Down(MouseButton::Left), x, y)
        ),
        Some(true)
    );

    terminal
        .draw(|f| render::draw(f, &mut state))
        .unwrap();

    assert_eq!(state.max_scroll - state.scroll, target);
    assert!(!state.follow_bottom);

    let last = state.transcript.len() - 1;

    if let TranscriptItem::Assistant(text) = &mut state.transcript[last] {
        text.push_str("more\n");
    }

    state
        .transcript_layout
        .invalidate_from(last);
    terminal
        .draw(|f| render::draw(f, &mut state))
        .unwrap();

    assert_eq!(state.max_scroll - state.scroll, target);

    let bottom = state.navigation.area.bottom() - 1;

    assert_eq!(
        terminal.backend().buffer()[(x, bottom)].symbol(),
        icons::ARROW_DOWN
    );

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
    state
        .transcript
        .push(TranscriptItem::User("first".into()));
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|f| render::draw(f, &mut state))
        .unwrap();
    let (row, _, _) = state.navigation.targets[0].clone();
    let column = state.navigation.area.x;

    assert_eq!(
        handle(&mut state, &mouse(MouseEventKind::Moved, column, row)),
        Some(true)
    );
    assert_eq!(state.navigation.hovered, Some(Target::Round(row)));

    state.show_help = true;
    terminal
        .draw(|f| render::draw(f, &mut state))
        .unwrap();

    assert_eq!(state.navigation.hovered, None);
}

#[test]
fn leaving_a_dot_erases_its_rendered_preview() {
    let mut state = UiState::new("model");
    state
        .transcript
        .push(TranscriptItem::User("first".into()));
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|f| render::draw(f, &mut state))
        .unwrap();
    let (row, _, _) = state.navigation.targets[0].clone();
    let column = state.navigation.area.x;
    handle(&mut state, &mouse(MouseEventKind::Moved, column, row));
    terminal
        .draw(|f| render::draw(f, &mut state))
        .unwrap();
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
    terminal
        .draw(|f| render::draw(f, &mut state))
        .unwrap();

    assert_eq!(state.navigation.tooltip_area, Rect::default());
}

#[test]
fn preview_is_at_most_one_quarter_of_window_and_scrolls_long_text() {
    let mut state = UiState::new("model");
    state
        .transcript
        .push(TranscriptItem::User(
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
    state
        .navigation
        .preview_scroll
        .as_mut()
        .unwrap()
        .2 = Instant::now() - std::time::Duration::from_secs(1);
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
    terminal
        .draw(|f| render::draw(f, &mut state))
        .unwrap();

    for _ in 0..40 {
        let area = state.navigation.area;
        handle(
            &mut state,
            &mouse(MouseEventKind::ScrollDown, area.x, area.y),
        );
        terminal
            .draw(|f| render::draw(f, &mut state))
            .unwrap();
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
    larger
        .draw(|f| render::draw(f, &mut state))
        .unwrap();

    for (y, offset, _) in &state.navigation.targets {
        assert!(*y < state.navigation.area.bottom() - 1);
        assert!(*offset < state.transcript_layout.height());
    }
}

#[test]
fn output_keeps_a_blank_column_before_navigation() {
    let mut state = UiState::new("model");
    state
        .transcript
        .push(TranscriptItem::User("x".repeat(200)));
    let mut terminal = Terminal::new(TestBackend::new(40, 12)).unwrap();
    terminal
        .draw(|f| render::draw(f, &mut state))
        .unwrap();

    let gap = state.navigation.area.x - 1;

    for y in state.navigation.area.y..state.navigation.area.bottom() - 1 {
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
    terminal
        .draw(|f| render::draw(f, &mut state))
        .unwrap();

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
        terminal.backend().buffer()[(state.navigation.area.x, state.navigation.area.y)].symbol(),
        icons::CIRCLE_OUTLINE
    );
    let area = state.navigation.area;
    for _ in 0..25 {
        handle(&mut state, &mouse(MouseEventKind::ScrollUp, area.x, area.y));
        terminal
            .draw(|f| render::draw(f, &mut state))
            .unwrap();
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

    terminal
        .draw(|f| render::draw(f, &mut state))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let has_active = state
        .navigation
        .targets
        .iter()
        .any(|(y, _, _)| buffer[(state.navigation.area.x, *y)].symbol() == icons::CIRCLE);

    assert!(has_active, "Active dot must be visible at bottom");

    state.follow_bottom = false;
    state.scroll = 100;
    terminal
        .draw(|f| render::draw(f, &mut state))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let has_active = state
        .navigation
        .targets
        .iter()
        .any(|(y, _, _)| buffer[(state.navigation.area.x, *y)].symbol() == icons::CIRCLE);

    assert!(
        has_active,
        "Active dot must be visible when scrolled to middle"
    );

    state.scroll = state.max_scroll;
    terminal
        .draw(|f| render::draw(f, &mut state))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let has_active = state
        .navigation
        .targets
        .iter()
        .any(|(y, _, _)| buffer[(state.navigation.area.x, *y)].symbol() == icons::CIRCLE);

    assert!(
        has_active,
        "Active dot must be visible when scrolled to top"
    );
}

#[test]
fn scrolling_changes_navigation_and_current_location_can_be_found_without_moving_content() {
    let mut state = UiState::new("orbit-model");
    state.transcript = (0..1_000)
        .map(|i| TranscriptItem::User(format!("Orbit round {i}")))
        .collect();
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut state))
        .unwrap();
    let original = terminal.backend().buffer().clone();
    let original_targets = state.navigation.targets.clone();
    let rail = state.navigation.area;
    let viewport = state.selection.viewport;
    let caption = state.navigation.current_area;
    handle(&mut state, &mouse(MouseEventKind::ScrollUp, rail.x, rail.y));
    terminal
        .draw(|frame| render::draw(frame, &mut state))
        .unwrap();
    let scrolled = terminal.backend().buffer();
    for y in viewport.y..viewport.bottom() {
        for x in viewport.x..viewport.right() {
            assert_eq!(original[(x, y)], scrolled[(x, y)]);
        }
    }

    let old_caption = row_text(&original, caption);
    let new_caption = row_text(scrolled, state.navigation.current_area);
    assert_ne!(old_caption, new_caption);
    assert!(
        new_caption
            .replace(' ', "")
            .contains("当前1000/1000")
    );
    assert!(
        !state
            .navigation
            .targets
            .iter()
            .any(|(_, _, text)| text == "Orbit round 999")
    );
    for (row, _, text) in &state.navigation.targets {
        if let Some((old_row, _, _)) = original_targets
            .iter()
            .find(|(_, _, old)| old == text)
        {
            assert_eq!(original[(rail.x, *old_row)].fg, scrolled[(rail.x, *row)].fg);
        }
    }

    for pair in state.navigation.targets.windows(2) {
        let a = scrolled[(rail.x, pair[0].0)].fg;
        let b = scrolled[(rail.x, pair[1].0)].fg;
        assert_ne!(a, b);
        if let (ratatui::style::Color::Rgb(ar, ag, ab), ratatui::style::Color::Rgb(br, bg, bb)) =
            (a, b)
        {
            assert!(
                ar.abs_diff(br)
                    .max(ag.abs_diff(bg))
                    .max(ab.abs_diff(bb))
                    <= 24
            );
        }
    }

    for _ in 0..400 {
        handle(&mut state, &mouse(MouseEventKind::ScrollUp, rail.x, rail.y));
    }
    terminal
        .draw(|frame| render::draw(frame, &mut state))
        .unwrap();
    assert_eq!(state.navigation.first, 0);
    assert_eq!(
        terminal.backend().buffer()[(rail.x + 1, rail.y)].symbol(),
        "┃"
    );
    let current = state.navigation.current_area;
    handle(
        &mut state,
        &mouse(
            MouseEventKind::Down(MouseButton::Left),
            current.x,
            current.y,
        ),
    );
    terminal
        .draw(|frame| render::draw(frame, &mut state))
        .unwrap();
    assert!(state.follow_bottom);
    assert_eq!(state.scroll, 0);
    let (row, _, _) = state.navigation.targets.last().unwrap();
    let row = *row;
    handle(&mut state, &mouse(MouseEventKind::Moved, rail.x, row));
    terminal
        .draw(|frame| render::draw(frame, &mut state))
        .unwrap();
    assert_eq!(
        terminal.backend().buffer()[(rail.x, row)].symbol(),
        icons::CIRCLE
    );
    assert!(matches!(
        terminal.backend().buffer()[(rail.x + 1, row)].symbol(),
        "│" | "┃"
    ));
    assert_eq!(terminal.backend().buffer()[(rail.x, row)].fg, THEME.accent);
    let chosen = state.navigation.first + 2;
    let chosen_row = state.navigation.targets[2].0;
    handle(
        &mut state,
        &mouse(MouseEventKind::Down(MouseButton::Left), rail.x, chosen_row),
    );
    terminal
        .draw(|frame| render::draw(frame, &mut state))
        .unwrap();
    let current = state.navigation.current_area;
    let caption = row_text(terminal.backend().buffer(), current);
    assert!(
        caption
            .replace(' ', "")
            .contains(&format!("当前{}/1000", chosen + 1)),
        "chosen={chosen}, caption={caption:?}, offset={}, max={}",
        state.max_scroll - state.scroll,
        state.max_scroll
    );
    let mut small = Terminal::new(TestBackend::new(28, 18)).unwrap();
    small
        .draw(|frame| render::draw(frame, &mut state))
        .unwrap();
    assert!(state.navigation.current_area.right() < 28);
    assert!(state.navigation.current_area.y >= state.selection.viewport.bottom());
}
