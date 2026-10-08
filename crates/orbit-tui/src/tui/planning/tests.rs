use super::*;
use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::{Terminal, backend::TestBackend};
fn fixture() -> State {
    let mut state = State::default();
    state.sync(serde_json::from_value(serde_json::json!({"generation":3,"mode":"plan","plan":{"id":"test","workspace":"/workspace","revision":1,"title":"方案","markdown":"## 目标\n保持一致的交互。","steps":["实现","验证"],"status":"review"}})).unwrap());
    state.open();
    state
}
fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, modifiers))
}
fn mouse(kind: MouseEventKind, x: u16, y: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    })
}
fn draw(state: &mut State) {
    Terminal::new(TestBackend::new(100, 35))
        .unwrap()
        .draw(|f| {
            state.draw(f);
        })
        .unwrap();
}
#[test]
fn plan_review_requires_focused_execution_button_and_ignores_press_repeat() {
    let mut state = fixture();
    draw(&mut state);
    assert!(
        state
            .handle(&key(KeyCode::Enter, KeyModifiers::NONE))
            .is_none()
    );
    draw(&mut state);
    let approve = state
        .hits
        .iter()
        .find(|(_, f)| *f == Focus::Approve)
        .unwrap()
        .0;
    state.handle(&mouse(MouseEventKind::Moved, approve.x, approve.y));
    assert!(
        state
            .handle(&key(KeyCode::Enter, KeyModifiers::NONE))
            .is_none()
    );
    state.handle(&key(KeyCode::Tab, KeyModifiers::NONE));
    state.handle(&key(KeyCode::Tab, KeyModifiers::NONE));
    assert_eq!(state.focus, Some(Focus::Approve));
    let mut repeat = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
    repeat.kind = KeyEventKind::Repeat;
    assert!(state.handle(&Event::Key(repeat)).is_none());
    assert_eq!(
        state.handle(&key(KeyCode::Enter, KeyModifiers::NONE)),
        Some(Action::Approve { generation: 3 })
    );
    assert!(state.visible());
    assert!(
        state
            .handle(&mouse(
                MouseEventKind::Down(MouseButton::Left),
                approve.x,
                approve.y
            ))
            .is_none()
    );
    state.succeeded();
    assert!(!state.visible());
}
#[test]
fn plan_feedback_focus_is_editor_mouse_geometry_does_not_leak_and_submit_closes_on_ack() {
    let mut state = fixture();
    draw(&mut state);
    let area = state
        .hits
        .iter()
        .find(|(_, f)| *f == Focus::Feedback)
        .unwrap()
        .0;
    state.handle(&mouse(
        MouseEventKind::Down(MouseButton::Left),
        area.x,
        area.y,
    ));
    assert_eq!(state.focus, Some(Focus::Editor));
    assert!(state.hits.is_empty());
    draw(&mut state);
    state.handle(&Event::Paste("  补充测试  ".into()));
    let editor = state.editor;
    state.handle(&mouse(MouseEventKind::Moved, editor.x + 1, editor.y + 1));
    assert_eq!(state.pointer.hovered(), Some(Focus::Editor));
    state.handle(&mouse(MouseEventKind::Moved, 0, 0));
    assert_eq!(state.pointer.hovered(), None);
    assert_eq!(
        state.handle(&key(KeyCode::Enter, KeyModifiers::CONTROL)),
        Some(Action::Feedback {
            generation: 3,
            text: "补充测试".into()
        })
    );
    state.failed("连接断开".into());
    assert_eq!(state.draft.text, "  补充测试  ");
    assert!(!state.sending);
    assert!(state.visible());
    state.succeeded();
    assert!(!state.visible());
    assert!(state.draft.text.is_empty());
}
#[test]
fn plan_remote_updates_do_not_steal_focus_or_accept_stale_state() {
    let mut state = fixture();
    state.open = false;
    let latest = state.snapshot.clone();
    state.sync(latest.clone());
    assert!(!state.visible());
    state.sync(Snapshot::default());
    assert_eq!(state.snapshot, latest);
    state.open();
    let mut approved = latest;
    approved.generation += 1;
    approved.mode = WorkMode::Build;
    approved.plan.as_mut().unwrap().status = Status::Approved;
    state.sync(approved);
    assert!(!state.visible());
}
#[test]
fn plan_review_handles_small_windows_and_caches_long_markdown() {
    let mut state = fixture();
    state.snapshot.plan.as_mut().unwrap().markdown = "中文 English\n\n".repeat(1000);
    for (width, height) in [(20, 5), (40, 15), (100, 35)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|f| {
                state.draw(f);
            })
            .unwrap();
        for (area, _) in &state.hits {
            assert!(area.right() <= width && area.bottom() <= height);
        }
        if width == 100 {
            let count = state.cached.as_ref().unwrap().2.len();
            state.offset = usize::MAX;
            terminal
                .draw(|f| {
                    state.draw(f);
                })
                .unwrap();
            assert_eq!(state.cached.as_ref().unwrap().2.len(), count);
            assert!(state.offset < count);
        }
    }
}
