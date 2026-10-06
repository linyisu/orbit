#[test]
fn running_context_is_not_overwritten_by_completed_history() {
    let mut state = UiState::new("mock");
    state.submit("new prompt".into());
    state.apply(AgentEvent::ContextUsage {
        estimated_input: 45000,
    });
    let live = state.context_used_tokens;
    refresh_context_usage(&mut state, &[], &ToolRegistry::new());
    assert_eq!(state.context_used_tokens, live);
    state.apply(AgentEvent::Completed {
        turns: 1,
        tool_calls: 0,
    });
    refresh_context_usage(&mut state, &[], &ToolRegistry::new());
    assert!(state.context_used_tokens < live);
}

use crossterm::event::KeyEvent;

use super::*;

#[test]
fn resume_ui_shell_preserves_permission_mode() {
    let mut state = UiState::new("test::model");
    state.permission_mode = crate::tui::permissions::Mode::Yolo;
    let restored = resume_ui_shell(&state);

    assert_eq!(
        restored.permission_mode,
        crate::tui::permissions::Mode::Yolo
    );

    state.permission_mode = crate::tui::permissions::Mode::Ask;
    let restored = resume_ui_shell(&state);

    assert_eq!(restored.permission_mode, crate::tui::permissions::Mode::Ask);
}

#[tokio::test]
async fn failed_restore_preserves_a_running_tab_and_allows_the_next_restore() {
    let directory = tempfile::tempdir().unwrap();
    let original = directory.path().join("orbit-original");
    let missing = directory.path().join("orbit-missing");
    let available = directory.path().join("orbit-available");
    for path in [&original, &missing, &available] {
        std::fs::create_dir(path).unwrap();
    }
    std::fs::write(available.join("marker.txt"), "Orbit restored workspace").unwrap();
    let config = directory.path().join("config.toml");
    std::fs::write(
        &config,
        r#"default_provider = "test"
[providers.test]
base_url = "http://127.0.0.1:1/v1"
auto_refresh_models = false
api_key = "test-only"
[[providers.test.models]]
id = "orbit-model"
"#,
    )
    .unwrap();
    let options = crate::WorkspaceOptions {
        initial_directory: original.clone(),
        config,
        database: directory.path().join("orbit.db"),
        auth_directory: directory.path().join("auth"),
        allow_mcp_start: false,
        home: None,
        config_home: None,
        instruction_dir: None,
        editor_command: "vi".into(),
    };
    let setup = crate::prepare_workspace(&options, &original, None)
        .await
        .unwrap();
    let mut session = setup.session;
    session.enable_shared();
    let original_id = session.id().to_owned();
    session
        .begin("Orbit original request".into(), "test::orbit-model".into())
        .await
        .unwrap();
    let original_run = session.active_run_id();
    let scratch = session.scratch.clone();
    let command_output = session.command_output.clone();
    let mut model = setup.resources.model;
    let mut state = UiState::new("test::orbit-model");
    state.submit("Orbit original request".into());
    state.input.set("Orbit draft".into());
    let mut history = vec![ChatMessage::user("Orbit original request")];
    let original_history = history.clone();
    let mut tabs = tabs::Tabs::new(&session);
    let mut saved_ids = Vec::new();
    for path in [&missing, &available] {
        let mut saved = SessionState::open_shared_in(
            options.database.clone(),
            None,
            "test::orbit-model".into(),
            path,
        )
        .await
        .unwrap();

        saved
            .begin("Orbit saved request".into(), "test::orbit-model".into())
            .await
            .unwrap();
        saved
            .finish(
                "completed",
                Some(vec![ChatMessage::user("Orbit saved request")]),
            )
            .await
            .unwrap();
        saved_ids.push(saved.id().to_owned());
    }

    std::fs::remove_dir(&missing).unwrap();

    let mut resources = None;
    for (index, id) in saved_ids.iter().enumerate() {
        let next = load_resume(
            session.store.clone(),
            session.snapshot.session.project_id.clone(),
            id.clone(),
            crate::instructions::Instructions::default(),
            crate::skills::Skills::default(),
        )
        .await
        .unwrap();
        let result = activate_resume(
            &options,
            &mut tabs,
            &mut session,
            &mut state,
            &mut model,
            &mut history,
            next,
        )
        .await;

        if index == 0 {
            let error = result.err().unwrap();
            assert!(format!("{error:#}").contains(missing.to_str().unwrap()));
            assert_eq!(session.id(), original_id);
            assert_eq!(session.active_run_id(), original_run);
            assert_eq!(session.audit_workspace.as_ref().unwrap().root(), original);
            assert!(std::sync::Arc::ptr_eq(&scratch, &session.scratch));
            assert!(std::sync::Arc::ptr_eq(
                &command_output,
                &session.command_output
            ));
            assert_eq!(state.input.text, "Orbit draft");
            assert!(state.running());
            assert_eq!(history, original_history);
            assert_eq!(model.config().model, "orbit-model");
            assert_eq!(
                tabs.prune_closed(&background::Runs::new()),
                vec![id.clone()]
            );
            assert!(!tabs.contains(id));
        } else {
            resources = Some(result.unwrap());
        }
    }

    assert_eq!(session.id(), saved_ids[1]);
    assert_eq!(session.audit_workspace.as_ref().unwrap().root(), available);
    let output = resources
        .unwrap()
        .tools
        .execute(
            orbit_model::types::ToolCall::function(
                "orbit-read",
                "read_file",
                serde_json::json!({"path": "marker.txt"}),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert!(output.content.contains("Orbit restored workspace"));
    assert!(tabs.restore(
        &original_id,
        &mut session,
        &mut state,
        &mut model,
        &mut history
    ));
    assert_eq!(session.active_run_id(), original_run);
    assert_eq!(state.input.text, "Orbit draft");
    session.finish("completed", Some(history)).await.unwrap();
}

#[test]
fn overview_budget_counts_only_matching_tool_results_in_effective_history() {
    use orbit_model::types::ToolCall;

    let call = ToolCall::function("map", "project_overview", serde_json::json!({}));
    let output = ChatMessage::tool("map", "repository map");
    let messages = vec![
        ChatMessage::user("project_overview"),
        ChatMessage::assistant_tool_calls(vec![call]),
        output.clone(),
        ChatMessage::tool("other", "unrelated output"),
    ];

    assert_eq!(
        tool_context_tokens(&messages, "project_overview"),
        orbit_agent::context::estimate_tokens(&[output], &[])
    );
    assert_eq!(tool_context_tokens(&[], "project_overview"), 0);
    assert_eq!(tool_context_tokens(&messages, "missing"), 0);
}

#[tokio::test]
async fn resuming_recomputes_usage_from_restored_history_and_current_tools() {
    let tmp = tempfile::tempdir().unwrap();
    let mut session =
        SessionState::open(tmp.path().join("sessions.db"), None, "test::model".into())
            .await
            .unwrap();
    let id = session.id().to_owned();
    session
        .begin("question".into(), "test::model".into())
        .await
        .unwrap();
    session
        .finish(
            "completed",
            Some(vec![
                ChatMessage::user("question"),
                ChatMessage::assistant("answer ".repeat(5_000)),
            ]),
        )
        .await
        .unwrap();
    crate::tui::session_actions::new_test_session(&mut session, "other".into())
        .await
        .unwrap();
    let scope = crate::project_state::MemoryScope::new(session.id().into());
    let mut model = ModelClient::new(Default::default()).unwrap();
    let options = vec![ModelOption {
        max_output_tokens: 32_000,
        provider: "test".into(),
        model: "model".into(),
        context_tokens: 232_000,
        input_context_tokens: 224_000,
        default_for_provider: true,
        reasoning_efforts: vec![],
        supports_vision: true,
        config: Default::default(),
    }];
    let mut tools = ToolRegistry::new();
    tools.register(orbit_agent::tool::ClosureTool::new(
        "test_tool",
        "test tool description",
        serde_json::json!({ "type": "object" }),
        |_, _| async { Ok("unused".into()) },
    ));
    let instructions = crate::instructions::Instructions::default();
    let skills = crate::skills::Skills::default();
    let mut history = Vec::new();
    let mut state = UiState::new("other");
    // Both the picker and `/resume <id>` call this entry point. It must
    // produce a valid count without a later command or model request.
    resume_session(
        &id,
        &mut state,
        &mut model,
        &mut history,
        &mut session,
        &instructions,
        &skills,
        &options,
        &scope,
        &tools,
    )
    .await
    .unwrap();

    assert_eq!(state.input_context_tokens, 224_000);
    assert_eq!(state.model, "test::model");
    assert!(state.context_used_tokens > 10_000);
    assert_eq!(
        state.context_used_tokens,
        orbit_agent::context::estimate_tokens(&history, &tools.definitions())
    );
    assert!(state.context_used_tokens > orbit_agent::context::estimate_tokens(&history, &[]));

    // Reopening the same saved session must not accumulate injected prompts.
    let before = state.context_used_tokens;
    resume_session(
        &id,
        &mut state,
        &mut model,
        &mut history,
        &mut session,
        &instructions,
        &skills,
        &options,
        &scope,
        &tools,
    )
    .await
    .unwrap();

    assert_eq!(state.context_used_tokens, before);
}

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
        Action::Submit("/model two::fast".into())
    );

    state.input.set("/memory add f".into());
    assert_eq!(
        press(&mut state, KeyCode::Enter, KeyModifiers::NONE, false),
        Action::None
    );
    assert_eq!(state.input.text, "/memory add fact ");

    state.input.set("/mode au".into());
    assert_eq!(
        press(&mut state, KeyCode::Enter, KeyModifiers::NONE, true),
        Action::Submit("/mode auto".into())
    );
}

#[test]
fn menu_selects_commands_and_busy_enter_routes_commands_locally() {
    let mut state = UiState::new("mock");
    state.input.insert("/");
    press(&mut state, KeyCode::Down, KeyModifiers::NONE, false);
    press(&mut state, KeyCode::Down, KeyModifiers::NONE, false);

    assert_eq!(
        press(&mut state, KeyCode::Enter, KeyModifiers::NONE, false),
        Action::Submit("/rename".into())
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
fn steering_keeps_the_interrupted_user_task_in_the_next_request() {
    let history = vec![ChatMessage::system("rules")];
    let history = steering_history(&history, Some(ChatMessage::user("build the parser")));

    assert_eq!(history.len(), 2);
    assert!(matches!(
        &history[1],
        ChatMessage::User { content, .. } if content == "build the parser"
    ));
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
    state.attachments.push(image("orbit.png"));
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
    state.workspace = directory.path().to_string_lossy().into_owned();
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
    let choice = state.menu()[state.menu_index].name.clone();
    press(&mut state, KeyCode::Tab, KeyModifiers::NONE, false);
    assert_eq!(state.input.text, choice);
}

#[test]
fn editing_a_recalled_command_reopens_hints_but_a_noop_delete_does_not() {
    for (code, modifiers, cursor, expected) in [
        (KeyCode::Backspace, KeyModifiers::NONE, 9, "/mode as"),
        (KeyCode::Delete, KeyModifiers::NONE, 8, "/mode as"),
        (KeyCode::Char('k'), KeyModifiers::CONTROL, 7, "/mode a"),
    ] {
        let mut state = UiState::new("orbit-model");
        state.input.history = vec!["Orbit older".into(), "/mode ask".into()];
        press(&mut state, KeyCode::Up, KeyModifiers::NONE, false);
        press(&mut state, KeyCode::Delete, KeyModifiers::NONE, false);
        assert!(state.menu().is_empty());
        assert_eq!(state.input.text, "/mode ask");
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
    state.workspace = directory.path().to_string_lossy().into_owned();
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
    state.input.history.push("Orbit history".into());
    state.input.set("first\nsecond".into());
    state.attachments.push(image("orbit.png"));
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
    state.input.set("你好Orbit\nnext".into());
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

    let file_url = url::Url::from_file_path(&image).unwrap().to_string();

    assert_eq!(
        pasted_image_paths(&file_url, temp.path()),
        Some(vec![image])
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
    state.attachments.push(image("only.png"));
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

#[tokio::test]
async fn text_only_model_blocks_queuing_and_attaching_images() {
    let mut state = UiState::new("mock");
    state.supports_vision = false;

    assert!(!queue_attachment(&mut state, image("test.png")));
    assert!(state.attachments.is_empty());

    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("orbit.db");
    let session = SessionState::open(db_path, None, "test::model".into())
        .await
        .unwrap();

    attach_paths(&mut state, &session, vec![PathBuf::from("test.png")]).unwrap();

    assert!(state.attachments.is_empty());
    assert!(state.transcript.is_empty());
    assert!(
        state
            .info_dialog
            .as_ref()
            .unwrap()
            .content
            .contains("当前模型未启用图片输入，未添加图片附件")
    );

    state.supports_vision = true;

    assert!(queue_attachment(&mut state, image("test.png")));
    assert_eq!(state.attachments.len(), 1);
}
