use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::{Terminal, backend::TestBackend};
use serde_json::json;

use super::*;

fn mouse(kind: MouseEventKind, x: u16, y: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    })
}

#[test]
fn subagent_result_opens_in_inspector_without_inline_card_frame() {
    for width in [12, 40, 160] {
        let mut state = UiState::new("orbit::model");
        state
            .transcript
            .push(TranscriptItem::Tool(ToolRow {
                changes: Vec::new(),
                purpose: None,
                id: "orbit-report".into(),
                name: "SpawnAgent".into(),
                arguments: json!({"task":"检查 Orbit 报告"}).to_string(),
                status: ToolStatus::Finished,
                output: json!({
                    "agent_id": "00000000000000000000000000000001",
                    "status": "completed",
                    "summary": "# Orbit\n\n---\n\n```rust\n// Orbit 注释\nfn orbit() {}\n```"
                })
                .to_string(),
            }));
        super::super::toggle_transcript_item(&mut state, 0);
        let mut terminal = Terminal::new(TestBackend::new(width + 6, 50)).unwrap();
        terminal
            .draw(|frame| super::super::render::draw(frame, &mut state))
            .unwrap();
        let dialog = state.info_dialog.as_ref().unwrap();
        assert!(dialog.content.contains("fn orbit()"));
        assert!(!dialog.content.contains('╭'));
    }
}

#[test]
fn parent_navigation_has_hover_respects_selection_and_does_not_cover_the_editor() {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut state = UiState::new("orbit::model");
    state.subagent_parent = Some("orbit-parent".into());
    state
        .transcript
        .push(TranscriptItem::User("子任务".into()));
    terminal
        .draw(|frame| super::super::render::draw(frame, &mut state))
        .unwrap();
    let area = state.subagent_parent_area;
    assert_eq!(area.height, 1);
    assert!(area.bottom() <= state.input_area.y);
    assert!(area.y >= state.selection.viewport.bottom());
    assert_eq!(
        handle_parent(&mut state, &mouse(MouseEventKind::Moved, area.x, area.y)),
        Some(true)
    );
    assert!(state.subagent_parent_hovered);
    let down = mouse(MouseEventKind::Down(MouseButton::Left), area.x, area.y);
    let up = mouse(MouseEventKind::Up(MouseButton::Left), area.x, area.y);
    state.selection.viewport = Rect::new(0, 1, 80, 10);
    for event in [
        mouse(MouseEventKind::Down(MouseButton::Left), 1, 2),
        mouse(MouseEventKind::Drag(MouseButton::Left), 5, 2),
        mouse(MouseEventKind::Up(MouseButton::Left), 5, 2),
    ] {
        state.selection.handle(&event).unwrap();
    }

    assert!(state.selection.active());
    handle_parent(&mut state, &down);
    handle_parent(&mut state, &up);
    assert!(state.resume_request.is_none());
    handle_parent(&mut state, &down);
    handle_parent(&mut state, &up);
    assert_eq!(state.resume_request.take().as_deref(), Some("orbit-parent"));
    let key = Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
    state.show_help = true;
    assert!(handle_parent(&mut state, &key).is_none());
    assert!(state.resume_request.is_none());
    state.show_help = false;
    assert_eq!(handle_parent(&mut state, &key), Some(true));
    assert_eq!(state.resume_request.take().as_deref(), Some("orbit-parent"));
}
