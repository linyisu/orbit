use super::attachments::{focus_attachment_at, queue_attachment};
use super::*;
use crossterm::event::KeyEvent;

fn press(state: &mut UiState, code: KeyCode, modifiers: KeyModifiers, busy: bool) -> Action {
    keyboard_action(state, Event::Key(KeyEvent::new(code, modifiers)), busy)
}

#[test]
fn browser_links_require_safe_http_urls() {
    assert_eq!(
        browser_url("https://example.com/docs?from=orbit")
            .unwrap()
            .as_str(),
        "https://example.com/docs?from=orbit"
    );

    for value in [
        "file:///tmp/report.html",
        "mailto:person@example.com",
        "https://user:secret@example.com/",
    ] {
        assert!(browser_url(value).is_err(), "{value}");
    }
}

#[test]
fn argument_menu_completes_without_executing_incomplete_commands() {
    let mut state = UiState::new("one::fast");
    state.model_completions = vec!["one::fast".into(), "two::fast".into()];
    state.input.set("/model".into());

    assert_eq!(
        press(&mut state, KeyCode::Tab, KeyModifiers::NONE, false),
        Action::None
    );
    assert_eq!(state.input.text, "/model ");
    press(&mut state, KeyCode::Down, KeyModifiers::NONE, false);
    assert_eq!(
        press(&mut state, KeyCode::Enter, KeyModifiers::NONE, false),
        Action::None
    );
    assert_eq!(state.input.text, "/model two::");
    assert_eq!(
        press(&mut state, KeyCode::Enter, KeyModifiers::NONE, false),
        Action::Submit("/model two::fast".into())
    );

    state.input.set("/memory add f".into());
    assert_eq!(
        press(&mut state, KeyCode::Enter, KeyModifiers::NONE, false),
        Action::None
    );
    assert_eq!(state.input.text, "/memory add fact ");

    state
        .input
        .set("/permissions au".into());
    assert_eq!(
        press(&mut state, KeyCode::Enter, KeyModifiers::NONE, true),
        Action::Submit("/permissions auto".into())
    );
}

#[test]
fn menu_selects_commands_and_busy_enter_routes_commands_locally() {
    let mut state = UiState::new("mock");
    state.input.insert("/");
    press(&mut state, KeyCode::Down, KeyModifiers::NONE, false);
    press(&mut state, KeyCode::Down, KeyModifiers::NONE, false);
    press(&mut state, KeyCode::Down, KeyModifiers::NONE, false);

    press(&mut state, KeyCode::Down, KeyModifiers::NONE, false);

    assert_eq!(
        press(&mut state, KeyCode::Enter, KeyModifiers::NONE, false),
        Action::Submit("/changes".into())
    );

    state.input.insert("next question");

    assert_eq!(
        press(&mut state, KeyCode::Enter, KeyModifiers::NONE, true),
        Action::Steer("next question".into())
    );
    assert!(state.input.text.is_empty());

    state.input.insert("/model");

    assert_eq!(
        press(&mut state, KeyCode::Enter, KeyModifiers::NONE, true),
        Action::Submit("/model".into())
    );
    assert_eq!(
        press(&mut state, KeyCode::Esc, KeyModifiers::NONE, true),
        Action::None
    );
    assert_eq!(
        press(&mut state, KeyCode::Char('c'), KeyModifiers::CONTROL, true),
        Action::Cancel
    );
}

#[test]
fn mouse_scrolls_transcript_without_touching_prompt_history() {
    let mut state = UiState::new("mock");
    state.max_scroll = 30;
    state.input.insert("previous prompt");
    state.input.take();
    state.input.insert("draft");

    for kind in [MouseEventKind::ScrollUp, MouseEventKind::ScrollDown] {
        keyboard_action(
            &mut state,
            Event::Mouse(crossterm::event::MouseEvent {
                kind,
                column: 5,
                row: 5,
                modifiers: KeyModifiers::NONE,
            }),
            false,
        );

        assert_eq!(state.input.text, "draft");

        if kind == MouseEventKind::ScrollUp {
            assert_eq!(state.scroll, wheel::LINES);
            assert!(!state.follow_bottom);
        }
    }

    assert_eq!(state.scroll, 0);
    assert!(state.follow_bottom);

    press(&mut state, KeyCode::Up, KeyModifiers::NONE, false);

    assert_eq!(state.input.text, "previous prompt");
}

#[test]
fn selections_replace_text_copy_before_cancel_and_leave_attachments_alone() {
    let mut state = UiState::new("orbit-model");
    state.input.set("Orbit你好👩‍💻".into());
    press(&mut state, KeyCode::Left, KeyModifiers::SHIFT, false);
    assert_eq!(
        press(&mut state, KeyCode::Char('c'), KeyModifiers::CONTROL, true),
        Action::CopyInput("👩‍💻".into())
    );
    keyboard_action(&mut state, Event::Paste("编辑".into()), false);
    assert_eq!(state.input.text, "Orbit你好编辑");
    state
        .attachments
        .push(image("orbit.png"));
    press(&mut state, KeyCode::Char('a'), KeyModifiers::CONTROL, false);
    press(&mut state, KeyCode::Backspace, KeyModifiers::NONE, false);
    assert!(state.input.text.is_empty());
    assert_eq!(state.attachments.len(), 1);
    assert!(state.attachment_cursor.is_none());
}

#[test]
fn continuation_enter_only_removes_a_backslash_at_the_caret_including_when_busy() {
    for busy in [false, true] {
        let mut state = UiState::new("orbit-model");
        state.input.set("Orbit\\".into());
        assert_eq!(
            press(&mut state, KeyCode::Enter, KeyModifiers::NONE, busy),
            Action::None
        );
        assert_eq!(state.input.text, "Orbit\n");
        state.input.set("Orbit\\".into());
        state.input.set_cursor(2);
        let expected = if busy {
            Action::Steer("Orbit\\".into())
        } else {
            Action::Submit("Orbit\\".into())
        };

        assert_eq!(
            press(&mut state, KeyCode::Enter, KeyModifiers::NONE, busy),
            expected
        );
    }
}

#[test]
fn history_commands_do_not_capture_up_down_in_the_completion_menu() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("orbit-workspace")).unwrap();
    let mut state = UiState::new("orbit-model");
    state.workspace = directory
        .path()
        .to_string_lossy()
        .into_owned();
    state.input.history = vec!["Orbit earlier".into(), "/mode ask".into(), "/cd ".into()];
    state.input.set("Orbit draft".into());
    for (key, expected) in [
        (KeyCode::Up, "/cd "),
        (KeyCode::Up, "/mode ask"),
        (KeyCode::Up, "Orbit earlier"),
        (KeyCode::Down, "/mode ask"),
        (KeyCode::Down, "/cd "),
        (KeyCode::Down, "Orbit draft"),
    ] {
        assert_eq!(
            press(&mut state, key, KeyModifiers::NONE, false),
            Action::None
        );
        assert_eq!(state.input.text, expected);
    }
}

#[test]
fn explicit_history_navigation_keeps_command_drafts_out_of_the_menu() {
    let mut state = UiState::new("orbit-model");
    state.input.history = vec!["Orbit older".into(), "/mode ask".into()];
    state.input.set("/mode ".into());
    state.input.set_cursor(3);
    assert!(!state.menu().is_empty());
    press(&mut state, KeyCode::Up, KeyModifiers::CONTROL, false);
    assert_eq!(state.input.text, "/mode ask");
    assert!(state.menu().is_empty());
    press(&mut state, KeyCode::Up, KeyModifiers::NONE, false);
    assert_eq!(state.input.text, "Orbit older");
    press(&mut state, KeyCode::Down, KeyModifiers::NONE, false);
    press(&mut state, KeyCode::Down, KeyModifiers::CONTROL, false);
    assert_eq!(state.input.text, "/mode ");
    assert_eq!(state.input.cursor, 3);
    assert!(state.menu().is_empty());
    press(&mut state, KeyCode::Up, KeyModifiers::NONE, false);
    assert_eq!(state.input.text, "/mode ask");
}

#[test]
fn tab_opens_recalled_command_hints_before_completing_a_choice() {
    let mut state = UiState::new("orbit-model");
    state.input.history = vec!["/mode ".into()];
    press(&mut state, KeyCode::Up, KeyModifiers::NONE, false);
    assert!(state.menu().is_empty());
    press(&mut state, KeyCode::Tab, KeyModifiers::NONE, false);
    assert_eq!(state.input.text, "/mode ");
    assert!(!state.menu().is_empty());
    press(&mut state, KeyCode::Down, KeyModifiers::NONE, false);
    let choice = state.menu()[state.menu_index]
        .name
        .clone();
    press(&mut state, KeyCode::Tab, KeyModifiers::NONE, false);
    assert_eq!(state.input.text, choice);
}

#[test]
fn editing_a_recalled_command_reopens_hints_but_a_noop_delete_does_not() {
    for (code, modifiers, cursor, expected) in [
        (KeyCode::Backspace, KeyModifiers::NONE, 10, "/mode pla"),
        (KeyCode::Delete, KeyModifiers::NONE, 9, "/mode pla"),
        (KeyCode::Char('k'), KeyModifiers::CONTROL, 7, "/mode p"),
    ] {
        let mut state = UiState::new("orbit-model");
        state.input.history = vec!["Orbit older".into(), "/mode plan".into()];
        press(&mut state, KeyCode::Up, KeyModifiers::NONE, false);
        press(&mut state, KeyCode::Delete, KeyModifiers::NONE, false);
        assert!(state.menu().is_empty());
        assert_eq!(state.input.text, "/mode plan");
        state.input.set_cursor(cursor);
        press(&mut state, code, modifiers, false);
        assert_eq!(state.input.text, expected);
        assert!(!state.menu().is_empty());
    }
}

#[test]
fn recalled_directory_enter_uses_the_original_command_instead_of_a_hint() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::create_dir(directory.path().join("orbit-alpha")).unwrap();
    std::fs::create_dir(directory.path().join("orbit-beta")).unwrap();
    let mut state = UiState::new("orbit-model");
    state.workspace = directory
        .path()
        .to_string_lossy()
        .into_owned();
    let command = format!("/cd {}/", directory.path().display());
    state.input.set(command.clone());
    assert!(state.menu().len() >= 2);
    state.input.history = vec![command.clone()];
    state.input.set("Orbit draft".into());
    state.menu_index = 1;
    press(&mut state, KeyCode::Up, KeyModifiers::NONE, false);
    assert_eq!(
        press(&mut state, KeyCode::Enter, KeyModifiers::NONE, false),
        Action::Submit(command)
    );
}

#[test]
fn multiline_home_stays_in_text_and_explicit_history_restores_the_draft_cursor() {
    let mut state = UiState::new("orbit-model");
    state
        .input
        .history
        .push("Orbit history".into());
    state.input.set("first\nsecond".into());
    state
        .attachments
        .push(image("orbit.png"));
    press(&mut state, KeyCode::Home, KeyModifiers::NONE, false);
    assert_eq!(state.input.cursor, 6);
    assert!(state.attachment_cursor.is_none());
    press(&mut state, KeyCode::Up, KeyModifiers::CONTROL, false);
    assert_eq!(state.input.text, "Orbit history");
    press(&mut state, KeyCode::Down, KeyModifiers::CONTROL, false);
    assert_eq!(state.input.text, "first\nsecond");
    assert_eq!(state.input.cursor, 6);
}

#[test]
fn input_mouse_drag_and_shift_click_edit_without_using_transcript_selection() {
    let mut state = UiState::new("orbit-model");
    state
        .input
        .set("你好Orbit\nnext".into());
    state.input_area = ratatui::layout::Rect::new(4, 10, 11, 2);
    state.input_width = 10;
    for (kind, column, modifiers) in [
        (
            MouseEventKind::Down(MouseButton::Left),
            4,
            KeyModifiers::NONE,
        ),
        (
            MouseEventKind::Drag(MouseButton::Left),
            8,
            KeyModifiers::NONE,
        ),
        (MouseEventKind::Up(MouseButton::Left), 8, KeyModifiers::NONE),
    ] {
        assert!(handle_input_mouse(
            &mut state,
            &Event::Mouse(crossterm::event::MouseEvent {
                kind,
                column,
                row: 10,
                modifiers
            })
        ));
    }

    assert_eq!(state.input.selected_text(), Some("你好"));
    assert!(!state.selection.active());
    assert!(!state.input_dragging);
    press(&mut state, KeyCode::Char('x'), KeyModifiers::NONE, false);
    assert_eq!(state.input.text, "xOrbit\nnext");
    assert!(handle_input_mouse(
        &mut state,
        &Event::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 7,
            row: 10,
            modifiers: KeyModifiers::SHIFT,
        })
    ));
    assert_eq!(state.input.selected_text(), Some("Or"));
}

#[test]
fn plain_q_is_text_and_paste_does_not_submit() {
    let mut state = UiState::new("mock");

    assert_eq!(
        press(&mut state, KeyCode::Char('q'), KeyModifiers::NONE, false),
        Action::None
    );

    keyboard_action(&mut state, Event::Paste("\nhello\nworld".into()), false);

    assert_eq!(state.input.text, "q\nhello\nworld");
}

#[test]
fn pasted_or_dropped_image_paths_become_attachments() {
    let temp = tempfile::tempdir().unwrap();
    let image = temp.path().join("screen shot.png");
    std::fs::write(&image, b"not decoded until it is imported").unwrap();
    let mut state = UiState::new("mock");

    assert_eq!(
        keyboard_action(
            &mut state,
            Event::Paste(image.to_string_lossy().into_owned()),
            false,
        ),
        Action::AttachPaths(vec![image.clone()])
    );
    assert!(state.input.text.is_empty());

    let file_url = url::Url::from_file_path(&image)
        .unwrap()
        .to_string();

    assert_eq!(
        pasted_image_paths(&file_url, temp.path()),
        Some(vec![image])
    );
}

#[test]
fn absolute_paths_during_a_run_are_steering_not_commands() {
    let mut state = UiState::new("mock");
    state
        .input
        .set("/tmp/report.txt 请看看".into());
    assert_eq!(
        press(&mut state, KeyCode::Enter, KeyModifiers::NONE, true),
        Action::Steer("/tmp/report.txt 请看看".into())
    );
    state.input.set("/status".into());
    assert_eq!(
        press(&mut state, KeyCode::Enter, KeyModifiers::NONE, true),
        Action::Submit("/status".into())
    );
}

#[test]
fn clipboard_shortcuts_request_the_system_clipboard() {
    let mut state = UiState::new("mock");

    for modifiers in [KeyModifiers::CONTROL, KeyModifiers::ALT] {
        assert_eq!(
            press(&mut state, KeyCode::Char('v'), modifiers, false),
            Action::PasteClipboard
        );
    }
}

fn image(name: &str) -> orbit_model::types::Attachment {
    orbit_model::types::Attachment::Image {
        lease: None,
        name: name.into(),
        mime_type: "image/png".into(),
        path: PathBuf::from(name),
    }
}

#[test]
fn attachments_are_atomic_editable_input_items() {
    let mut state = UiState::new("mock");
    state.attachments = vec![image("first.png"), image("second.png")];
    state.input.set("caption".into());
    state.input.home();

    press(&mut state, KeyCode::Left, KeyModifiers::NONE, false);

    assert_eq!(state.attachment_cursor, Some(1));

    press(&mut state, KeyCode::Left, KeyModifiers::NONE, false);

    assert_eq!(state.attachment_cursor, Some(0));

    press(&mut state, KeyCode::Delete, KeyModifiers::NONE, false);

    assert_eq!(state.attachments, vec![image("second.png")]);
    assert_eq!(state.attachment_cursor, Some(0));

    press(&mut state, KeyCode::Right, KeyModifiers::NONE, false);

    assert_eq!(state.attachment_cursor, None);
    assert_eq!(state.input.cursor, 0);

    press(&mut state, KeyCode::Backspace, KeyModifiers::NONE, false);

    assert!(state.attachments.is_empty());

    press(&mut state, KeyCode::Right, KeyModifiers::NONE, false);

    assert_eq!(state.input.cursor, 1);
}

#[test]
fn attachment_click_focuses_it_and_image_only_messages_can_submit() {
    let mut state = UiState::new("mock");
    state
        .attachments
        .push(image("only.png"));
    state
        .attachment_areas
        .push(ratatui::layout::Rect::new(4, 5, 20, 1));

    assert!(focus_attachment_at(&mut state, 8, 5));
    assert_eq!(state.attachment_cursor, Some(0));
    assert_eq!(
        press(&mut state, KeyCode::Enter, KeyModifiers::NONE, false),
        Action::Submit(String::new())
    );
    assert_eq!(state.attachment_cursor, None);
}

#[test]
fn vertical_attachment_navigation_crosses_wrapped_rows_and_returns_to_the_draft() {
    let mut state = UiState::new("mock");
    state.input_width = 18;
    state.attachments = vec![image("first.png"), image("second.png"), image("third.png")];
    state
        .input
        .set("first line\nsecond line".into());
    press(&mut state, KeyCode::Up, KeyModifiers::NONE, false);
    assert!(state.attachment_cursor.is_none());
    assert_eq!(
        state
            .input
            .visual_lines(state.input_width)
            .1
            .row,
        0
    );
    let cursor = state.input.cursor;
    for index in [2, 1, 0] {
        press(&mut state, KeyCode::Up, KeyModifiers::NONE, false);
        assert_eq!(state.attachment_cursor, Some(index));
    }
    for index in [Some(1), Some(2), None] {
        press(&mut state, KeyCode::Down, KeyModifiers::NONE, false);
        assert_eq!(state.attachment_cursor, index);
    }
    assert_eq!(state.input.cursor, cursor);
    assert_eq!(state.input.text, "first line\nsecond line");
    state.input.set("/mo".into());
    state.attachment_cursor = Some(1);
    assert!(state.menu().is_empty());
    press(&mut state, KeyCode::Down, KeyModifiers::NONE, false);
    assert_eq!(state.attachment_cursor, Some(2));
}

#[test]
fn text_only_model_rejects_image_queue() {
    let mut state = UiState::new("mock");
    state.supports_vision = false;
    assert!(!queue_attachment(&mut state, image("test.png")));
    assert!(state.attachments.is_empty());
    state.supports_vision = true;
    assert!(queue_attachment(&mut state, image("test.png")));
    assert_eq!(state.attachments.len(), 1);
}
