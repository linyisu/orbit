use super::*;
use crossterm::event::KeyEvent;
use ratatui::{Terminal, backend::TestBackend};

fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, modifiers))
}

fn record(answer: &str, status: &str) -> orbit_protocol::SideQuestion {
    orbit_protocol::SideQuestion {
        thread_id: "thread".into(),
        model: "mock".into(),
        question: "解释 Orbit".into(),
        answer: answer.into(),
        status: status.into(),
        error: None,
    }
}

#[test]
fn command_is_available_during_a_main_run_and_does_not_steer_it() {
    assert_eq!(question("/btw 为什么？"), Some("为什么？"));
    assert_eq!(question("/btw"), Some(""));
    assert_eq!(question("/btw\nOrbit 问题"), Some("Orbit 问题"));
    assert_eq!(question("/btwhatever"), None);
    let mut state = super::super::app::UiState::new("orbit-fast");
    state.status = RunStatus::Running;
    state.input.set("/btw 为什么？".into());
    assert_eq!(
        super::super::keyboard_action(&mut state, key(KeyCode::Enter, KeyModifiers::NONE), true),
        super::super::Action::Submit("/btw 为什么？".into())
    );
    assert_eq!(state.status, RunStatus::Running);
    state.input.set("/bt".into());
    assert!(
        state
            .menu()
            .iter()
            .any(|candidate| candidate.name == "/btw")
    );
}

#[test]
fn a_side_dialog_stays_above_pending_approval_and_freezes_text_while_selected() {
    use super::super::app::UiState;
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use orbit_tools::approval::{ApprovalPreview, WritePreview};

    let mut state = UiState::new("orbit-fast");
    state.status = RunStatus::Running;
    state
        .transcript
        .push(TranscriptItem::User("Orbit 主任务".into()));
    state
        .btw
        .receive(Some(vec![record("中文回答", "running")]), true, Some(0));
    state.approval = Some(ApprovalPreview::Write(WritePreview {
        path: "orbit.rs".into(),
        before: None,
        after: "approval-body-marker".into(),
    }));
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|frame| super::super::render::draw(frame, &mut state))
        .unwrap();
    let screen: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();

    assert!(
        screen
            .replace(' ', "")
            .contains("旁路提问")
    );
    assert!(!screen.contains("approval-body-marker"));
    let area = state
        .btw
        .dialog
        .as_ref()
        .unwrap()
        .view
        .ui
        .selection
        .viewport;

    for (kind, x) in [
        (MouseEventKind::Down(MouseButton::Left), area.x + 2),
        (MouseEventKind::Drag(MouseButton::Left), area.x + 8),
        (MouseEventKind::Up(MouseButton::Left), area.x + 8),
    ] {
        state
            .btw
            .handle(&Event::Mouse(MouseEvent {
                kind,
                column: x,
                row: area.y + 2,
                modifiers: KeyModifiers::NONE,
            }))
            .unwrap();
    }

    assert!(
        state
            .btw
            .dialog
            .as_ref()
            .unwrap()
            .view
            .ui
            .selection
            .active()
    );
    state.btw.receive(
        Some(vec![record("中文回答追加内容", "completed")]),
        false,
        None,
    );
    assert!(matches!(
        &state
            .btw
            .dialog
            .as_ref()
            .unwrap()
            .view
            .ui
            .transcript[1],
        TranscriptItem::Assistant(text) if text == "中文回答"
    ));
    state
        .btw
        .dialog
        .as_mut()
        .unwrap()
        .view
        .ui
        .selection
        .clear();
    state.btw.poll();
    assert!(matches!(
        &state
            .btw
            .dialog
            .as_ref()
            .unwrap()
            .view
            .ui
            .transcript[1],
        TranscriptItem::Assistant(text) if text == "中文回答追加内容"
    ));
    assert_eq!(state.status, RunStatus::Running);
    assert!(state.approval.is_some());
}

#[test]
fn incoming_threads_navigate_and_cancel_without_executing_a_local_model() {
    let mut questions = Questions::default();
    let first = record("first answer", "completed");
    let mut second = record("second answer", "running");
    second.thread_id = "other".into();
    questions.receive(Some(vec![first, second]), true, Some(1));
    questions
        .handle(&key(KeyCode::Left, KeyModifiers::ALT))
        .unwrap();
    assert_eq!(questions.follow_up_index(), Some(0));
    assert!(matches!(
        &questions
            .dialog
            .as_ref()
            .unwrap()
            .view
            .ui
            .transcript[1],
        TranscriptItem::Assistant(text) if text == "first answer"
    ));
    questions
        .handle(&key(KeyCode::Right, KeyModifiers::ALT))
        .unwrap();
    questions
        .handle(&key(KeyCode::Char('c'), KeyModifiers::CONTROL))
        .unwrap();
    assert_eq!(questions.take_cancel(), Some(1));
    assert_eq!(questions.take_cancel(), None);
    questions
        .handle(&key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    assert!(!questions.visible());
    assert!(!questions.running);
    assert!(!questions.has_records());
    for (width, height) in [(100, 30), (30, 12), (10, 3), (1, 1)] {
        questions.receive(Some(vec![record("answer", "completed")]), false, Some(0));
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                let area = super::super::dialogs::centered_area(frame.area());
                let inner = ratatui::widgets::Block::bordered().inner(area);
                questions.draw(frame, area, inner, Default::default());
            })
            .unwrap();
    }
}
