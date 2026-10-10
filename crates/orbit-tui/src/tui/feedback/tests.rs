use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use ratatui::{Terminal, backend::TestBackend};

use super::super::app::{TranscriptItem, UiState};

fn mouse(kind: MouseEventKind, x: u16, y: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    })
}

fn draw(state: &mut UiState, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| super::super::render::draw_frame(frame, state))
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn short_command_feedback_is_nonblocking_and_opens_its_full_details() {
    for home in [true, false] {
        let mut state = UiState::new("model");
        if !home {
            state.submit("question".into());
        }
        state.input.set("draft".into());
        state.follow_bottom = false;
        let history = state.transcript.len();
        let text = format!("没有发现 Skill。{}", "安装路径说明".repeat(30));
        state.show_output("Skills", text.clone());
        let screen = draw(&mut state, 100, 30);
        assert!(screen.replace(' ', "").contains("没有发现Skill"));
        assert!(state.feedback.area.height < 6);
        assert!(state.info_dialog.is_none());
        assert!(state.ime_cursor.is_some());
        assert_eq!(state.transcript.len(), history);
        assert_eq!(state.input.text, "draft");
        assert!(!state.follow_bottom);
        let area = state.feedback.area;
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            state.handle_feedback(&mouse(kind, area.x + 2, area.y + 1));
        }
        let dialog = state.info_dialog.as_ref().unwrap();
        assert_eq!(dialog.title, "Skills");
        assert_eq!(dialog.content, text);
        assert_eq!(state.transcript.len(), history);
    }
}

#[test]
fn command_reports_and_local_notices_never_pollute_an_active_conversation() {
    let mut state = UiState::new("model");
    state.submit("question".into());
    state.input.set("keep editing".into());
    let history = state.transcript.len();
    state.report_local(TranscriptItem::Notice("正在重新连接…".into()));
    assert!(
        draw(&mut state, 100, 30)
            .replace(' ', "")
            .contains("正在重新连接")
    );
    let diagnostics = format!("会话诊断\n{}末条记录", "运行详情\n".repeat(100));
    state.show_output("诊断记录", diagnostics.clone());
    draw(&mut state, 100, 30);
    assert_eq!(state.transcript.len(), history);
    assert_eq!(state.input.text, "keep editing");
    let dialog = state.info_dialog.as_ref().unwrap();
    assert_eq!(dialog.title, "诊断记录");
    assert_eq!(dialog.content, diagnostics);
    super::super::dialogs::handle(
        &mut state,
        Event::Key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)),
    );
    assert!(
        draw(&mut state, 100, 30)
            .replace(' ', "")
            .contains("末条记录")
    );
    super::super::dialogs::handle(
        &mut state,
        Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
    );
    assert!(state.info_dialog.is_none());
    assert_eq!(state.input.text, "keep editing");
}

#[test]
fn errors_stay_visible_without_moving_history_or_interrupting_the_draft() {
    for home in [true, false] {
        let mut state = UiState::new("model");
        if !home {
            state.submit("question".into());
            state
                .transcript
                .push(TranscriptItem::Assistant("history\n".repeat(100)));
        }
        state.input.set("keep editing".into());
        draw(&mut state, 100, 30);
        state.scroll = state.max_scroll;
        state.follow_bottom = false;
        let scroll = state.scroll;
        let history = state.transcript.len();
        for _ in 0..9 {
            state.report_local(TranscriptItem::Error("Cannot open link".into()));
        }
        let screen = draw(&mut state, 100, 30);
        assert!(screen.contains("Cannot open link"));
        assert!(screen.contains("9 次"));
        assert_eq!(state.transcript.len(), history);
        assert_eq!(state.scroll, scroll);
        assert!(!state.follow_bottom);
        assert_eq!(state.input.text, "keep editing");
        assert!(state.ime_cursor.is_some());
        assert!(!state.dialog_open());
        assert!(!state.handle_feedback(&Event::Key(KeyEvent::new(
            KeyCode::Char('x'),
            KeyModifiers::NONE
        ))));
        assert_eq!(state.feedback.notices.len(), 1);
        if !home {
            let area = state.selection.viewport;
            state
                .selection
                .handle(&mouse(
                    MouseEventKind::Down(MouseButton::Left),
                    area.x + 3,
                    area.y + 1,
                ))
                .unwrap();
            assert!(state.selection.active());
            assert!(draw(&mut state, 100, 30).contains("Cannot open link"));
            state.feedback.error("New error while selecting");
            assert!(draw(&mut state, 100, 30).contains("New error while selecting"));
            let close = state.feedback.close;
            state.handle_feedback(&mouse(
                MouseEventKind::Down(MouseButton::Left),
                close.x,
                close.y,
            ));
            state.handle_feedback(&mouse(
                MouseEventKind::Up(MouseButton::Left),
                close.x,
                close.y,
            ));
            let screen = draw(&mut state, 100, 30);
            assert!(!screen.contains("New error while selecting"));
            assert!(screen.contains("Cannot open link"));
            assert!(state.selection.active());
            assert_eq!(state.scroll, scroll);
        }
    }
}

#[test]
fn notices_are_bounded_sanitized_and_dismiss_only_on_a_matching_release() {
    let mut state = UiState::new("model");
    for n in 0..12 {
        state.feedback.error(format!("Error {n}"));
    }
    assert_eq!(state.feedback.notices.len(), 8);
    state.feedback.error("Error 8");
    assert_eq!(state.feedback.notices[0].count, 2);
    state
        .feedback
        .error(format!("\u{1b}[31m{}", "文".repeat(9000)));
    assert!(!state.feedback.notices[0].text.contains('\u{1b}'));
    assert!(state.feedback.notices[0].text.ends_with("已截断"));
    for (width, height) in [(141, 41), (28, 18), (10, 3), (1, 1)] {
        draw(&mut state, width, height);
        let area = state.feedback.area;
        assert_eq!(area.intersection(Rect::new(0, 0, width, height)), area);
        assert!(state.info_dialog.is_none());
    }
    draw(&mut state, 100, 30);
    let close = state.feedback.close;
    assert!(state.handle_feedback(&mouse(
        MouseEventKind::Up(MouseButton::Left),
        close.x,
        close.y
    )));
    assert_eq!(state.feedback.notices.len(), 8);
    state.handle_feedback(&mouse(
        MouseEventKind::Down(MouseButton::Left),
        close.x,
        close.y,
    ));
    state.handle_feedback(&mouse(MouseEventKind::Up(MouseButton::Left), 0, 29));
    assert_eq!(state.feedback.notices.len(), 8);
    state.handle_feedback(&mouse(
        MouseEventKind::Down(MouseButton::Left),
        close.x,
        close.y,
    ));
    state.handle_feedback(&mouse(
        MouseEventKind::Up(MouseButton::Left),
        close.x,
        close.y,
    ));
    assert_eq!(state.feedback.notices.len(), 7);
    assert!(state.info_dialog.is_none());
}

#[test]
fn asynchronous_errors_invalidate_previous_pointer_targets() {
    let mut state = UiState::new("model");
    state.feedback.error("first");
    draw(&mut state, 100, 30);
    let area = state.feedback.area;
    state.handle_feedback(&mouse(
        MouseEventKind::Down(MouseButton::Left),
        area.x,
        area.y,
    ));
    state.feedback.error("second");
    draw(&mut state, 100, 30);
    state.handle_feedback(&mouse(
        MouseEventKind::Up(MouseButton::Left),
        area.x,
        area.y,
    ));
    assert_eq!(state.feedback.notices.len(), 2);
    assert!(state.info_dialog.is_none());
}

#[test]
fn details_use_the_existing_scrollable_dialog_above_other_surfaces() {
    let mut state = UiState::new("model");
    state.show_help = true;
    state.feedback.error(format!(
        "Detailed error\n{}\nEnd of details",
        "context\n".repeat(80)
    ));
    draw(&mut state, 100, 30);
    let area = state.feedback.area;
    state.handle_feedback(&mouse(
        MouseEventKind::Down(MouseButton::Left),
        area.x + 2,
        area.y + 2,
    ));
    state.handle_feedback(&mouse(
        MouseEventKind::Up(MouseButton::Left),
        area.x + 2,
        area.y + 2,
    ));
    assert_eq!(
        super::super::overlay::active(&state),
        Some(super::super::overlay::Layer::Info)
    );
    let screen = draw(&mut state, 100, 30);
    assert!(screen.contains("Detailed error"), "{screen}");
    super::super::dialogs::handle(
        &mut state,
        Event::Key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)),
    );
    assert!(draw(&mut state, 100, 30).contains("End of details"));
    state.feedback.error("New error above details");
    assert!(draw(&mut state, 100, 30).contains("New error above details"));
    let close = state.feedback.close;
    state.handle_feedback(&mouse(
        MouseEventKind::Down(MouseButton::Left),
        close.x,
        close.y,
    ));
    state.handle_feedback(&mouse(
        MouseEventKind::Up(MouseButton::Left),
        close.x,
        close.y,
    ));
    assert!(state.info_dialog.is_some());
    super::super::dialogs::handle(
        &mut state,
        Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
    );
    assert!(state.info_dialog.is_none());
    assert!(state.show_help);
    assert!(state.feedback.notices.is_empty());
}
