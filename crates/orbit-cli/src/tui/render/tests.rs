use super::*;
use crate::tui::app::SessionTab;
use crate::tui::theme::desaturate_for_thinking;
use ratatui::{Terminal, backend::TestBackend};

#[test]
fn tools_expand_independently_and_commands_expand_in_full() {
    let mut state = UiState::new("mock");
    for i in 0..2 {
        state
            .transcript
            .push(TranscriptItem::Tool(super::super::app::ToolRow {
                id: i.to_string(),
                name: "run_command".into(),
                status: ToolStatus::Finished,
                arguments:
                    serde_json::json!({ "command": format!("echo {} end-{i}", "long ".repeat(30)) })
                        .to_string(),
                output: "one\ntwo\nthree\nfour".into(),
            }));
    }
    render(&mut state, 100, 40);

    super::super::toggle_transcript_item(&mut state, 0);
    super::super::toggle_transcript_item(&mut state, 1);
    assert_eq!(state.expanded, [0, 1].into_iter().collect());
    let TranscriptItem::Tool(row) = &state.transcript[0] else {
        unreachable!()
    };
    let full = tool_card(row, true, 80)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(full.contains("run_command"));
    assert!(full.contains('╭') && full.contains('╰'));
    assert!(full.contains("end-0"));
    assert_eq!(full.matches("end-0").count(), 1);

    super::super::toggle_transcript_item(&mut state, 0);
    assert_eq!(state.expanded, [1].into_iter().collect());
}

#[test]
fn parse_ansi_to_spans_strips_charset_and_preserves_clean_text() {
    let raw = "\x1b[31m-\x1b(B\x1b[m\x1b[32m+    let cell = &mut frame;\x1b(B\x1b[m";
    let spans = parse_ansi_to_spans(raw);
    let joined: String = spans.iter().map(|s| s.content.as_ref()).collect();
    assert!(!joined.contains("\u{1b}"));
    assert!(!joined.contains("(B"));
    assert_eq!(joined, "-+    let cell = &mut frame;");
}

#[test]
fn completed_thinking_drops_trailing_blank_rows_without_changing_content() {
    let mut row: super::super::app::ThinkingRow = "thought\n\n  \n".to_owned().into();
    row.finished = true;
    row.elapsed = Some(std::time::Duration::ZERO);
    let lines = transcript_lines(&[TranscriptItem::Thinking(row.clone())], true);

    assert!(lines[0].to_string().contains("<0.1s"));
    assert_eq!(lines.len(), 3);
    assert_eq!(row.content, "thought\n\n  \n");
}

#[test]
fn completed_reasoning_collapses_and_header_hover_and_click_reveal_it() {
    use crossterm::event::{Event, KeyModifiers, MouseEvent, MouseEventKind};
    use orbit_agent::events::AgentEvent;
    use orbit_model::types::StreamEvent;

    let mut state = UiState::new("model");
    state.apply(AgentEvent::Model(StreamEvent::ReasoningDelta(
        "private working text".into(),
    )));
    assert!(render(&mut state, 90, 30).contains("private working text"));

    state.apply(AgentEvent::Model(StreamEvent::TextDelta(
        "final response".into(),
    )));
    let TranscriptItem::Thinking(row) = &mut state.transcript[0] else {
        unreachable!()
    };
    row.elapsed = Some(std::time::Duration::from_millis(7500));
    let collapsed = render(&mut state, 90, 30);
    assert!(collapsed.contains("7.5s"), "{collapsed}");
    assert!(
        transcript_lines(&state.transcript[..1], false)[0]
            .to_string()
            .contains("思考 7.5s")
    );
    assert!(!collapsed.contains("private working text"));
    assert!(collapsed.contains("final response"));

    let area = state.selection.viewport;
    let offset = state.max_scroll.saturating_sub(state.scroll);
    let point = (area.y..area.bottom())
        .find_map(|y| {
            (area.x..area.right()).find_map(|x| {
                (state
                    .transcript_layout
                    .disclosure_at(&state.transcript, area, offset, (x, y))
                    == Some(0))
                .then_some((x, y))
            })
        })
        .expect("thought header is clickable");
    let moved = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Moved,
        column: point.0,
        row: point.1,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(handle_transcript_hover(&mut state, &moved), Some(true));
    state.thinking_hover_started =
        Some(std::time::Instant::now() - std::time::Duration::from_millis(150));
    let mut terminal = Terminal::new(TestBackend::new(90, 30)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    assert!(
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .any(|cell| cell.symbol() == "思" && cell.fg == THINKING_BRIGHT)
    );

    assert!(super::super::toggle_transcript_at(&mut state, point));
    assert!(render(&mut state, 90, 30).contains("private working text"));
    assert!(super::super::toggle_transcript_at(&mut state, point));
    assert!(!render(&mut state, 90, 30).contains("private working text"));

    let leave = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Moved,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    });
    assert_eq!(handle_transcript_hover(&mut state, &leave), Some(true));
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    assert!(
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .any(|cell| cell.symbol() == "思" && cell.fg == THINKING_MUTED)
    );
}

#[test]
fn collapsed_tool_cards_fit_width_and_bound_the_preview() {
    use super::super::app::ToolRow;

    for status in [
        ToolStatus::Running,
        ToolStatus::Finished,
        ToolStatus::Failed,
    ] {
        let item = TranscriptItem::Tool(ToolRow {
            id: "call".into(),
            name: "run_command".into(),
            status,
            arguments: serde_json::json!({ "command": format!("中文\t{}", "command ".repeat(50)) })
                .to_string(),
            output: "hidden result".into(),
        });

        for width in [1, 8, 24, 80] {
            let lines = transcript_lines_at_width(std::slice::from_ref(&item), false, width);
            assert!(lines.len() <= 9);
            assert!(
                lines.iter().all(|line| line.width() <= usize::from(width)),
                "{lines:?}"
            );
            assert!(lines.last().unwrap().to_string().is_empty());
        }
    }
}

#[test]
fn hovered_title_scrolls_without_splitting_wide_or_combining_characters() {
    let title = "中文e\u{301}标题长到需要滚动显示最后内容";
    let initial = scrolling_title(title, 12, None);

    assert!(initial.ends_with('…'));

    let frames: Vec<_> = (0..50)
        .map(|tick| {
            scrolling_title(
                title,
                12,
                Some(std::time::Duration::from_millis(tick * 120)),
            )
        })
        .collect();

    assert!(frames.iter().all(|frame| frame.width() <= 12));
    assert!(frames.iter().any(|frame| frame.ends_with("最后内容")));
    assert!(frames.iter().all(|frame| !frame.starts_with('\u{301}')));
}

#[test]
fn new_tab_button_changes_on_hover_and_restores_on_leave() {
    let mut state = UiState::new("model");
    let plain = tab_control_style(&state, &super::super::TabAction::New);
    state.tab_hover = Some((super::super::TabAction::New, std::time::Instant::now()));

    assert_ne!(
        tab_control_style(&state, &super::super::TabAction::New),
        plain
    );

    state.tab_hover = None;

    assert_eq!(
        tab_control_style(&state, &super::super::TabAction::New),
        plain
    );
}

#[cfg(unix)]
#[tokio::test]
async fn editor_returns_to_home_until_the_draft_is_sent() {
    let directory = tempfile::tempdir().unwrap();
    let editor = super::super::editor::Editor::new("/bin/true".into());
    let mut state = UiState::new("model");
    state.tabs.push(SessionTab {
        waiting_approval: false,
        elapsed: std::time::Duration::ZERO,
        id: "new".into(),
        title: "新会话".into(),
        status: RunStatus::Idle,
        active: true,
    });
    state.input.set("/editor".into());
    state.input.take();
    let text = editor
        .edit("尚未发送的草稿\n第二行", directory.path())
        .await
        .unwrap();

    state.restore_editor_draft(text);
    let screen = render(&mut state, 140, 40);

    assert!(state.transcript.is_empty());
    assert_eq!(state.status, RunStatus::Idle);
    assert!(state.input_area.x > 20);
    assert!(state.input_area.y < 30);
    assert!(screen.replace(' ', "").contains("尚未发送的草稿"));
    assert_eq!(state.tabs.len(), 1);

    let prompt = state.input.take().unwrap();
    state.submit(prompt);
    render(&mut state, 140, 40);
    assert!(state.input_area.x < 20);
    assert!(!state.transcript.is_empty());
}

#[test]
fn home_centers_composer_and_conversation_exposes_new_session_button() {
    let mut state = UiState::new("provider::model");
    state.tabs.push(SessionTab {
        waiting_approval: false,
        elapsed: std::time::Duration::ZERO,
        id: "first".into(),
        title: "New session".into(),
        status: RunStatus::Idle,
        active: true,
    });

    render(&mut state, 140, 40);
    let home_input = state.input_area;

    assert!(home_input.x > 20);
    assert!(home_input.y > 10 && home_input.y < 30);
    assert!(state.tab_areas.is_empty());
    assert_eq!(state.new_tab_area, Rect::default());

    state.transcript.push(TranscriptItem::User("Hello".into()));
    render(&mut state, 140, 40);

    assert!(state.input_area.y > home_input.y);
    assert!(state.input_area.width > home_input.width);
    assert_eq!(state.tab_areas.len(), 1);
    assert_eq!(state.new_tab_area.height, 1);
    assert!(state.new_tab_area.x >= state.tab_areas[0].0.right());
    assert!(!render(&mut state, 140, 40).contains(icons::CLOSE_THICK));

    state.tab_hover = Some((
        super::super::TabAction::Select("first".into()),
        std::time::Instant::now(),
    ));

    assert!(render(&mut state, 140, 40).contains(icons::CLOSE_THICK));

    state.tab_hover = None;

    assert!(!render(&mut state, 140, 40).contains(icons::CLOSE_THICK));
}

#[tokio::test]
async fn home_changes_are_silent_and_queries_do_not_enter_the_conversation() {
    use super::super::{CommandContext, ModelOption, dialogs, run_command};
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    use orbit_model::types::ReasoningEffort;

    let directory = tempfile::tempdir().unwrap();
    let mut session = crate::session::SessionState::open_in(
        directory.path().join("orbit.db"),
        None,
        "orbit-model".into(),
        directory.path(),
    )
    .await
    .unwrap();
    let mut state = UiState::new("orbit-model");
    state.tabs.push(SessionTab {
        waiting_approval: false,
        elapsed: std::time::Duration::ZERO,
        id: session.id().into(),
        title: "Orbit session".into(),
        status: RunStatus::Idle,
        active: true,
    });
    render(&mut state, 140, 40);
    let input_area = state.input_area;
    let mut model = orbit_model::client::ModelClient::new(Default::default()).unwrap();
    let mut history = Vec::new();
    let tools = orbit_agent::tool::ToolRegistry::new();
    let instructions = crate::instructions::Instructions::default();
    let skills = crate::skills::Skills::default();
    let options = [ModelOption {
        provider: "orbit".into(),
        model: "fast".into(),
        max_output_tokens: 8_000,
        context_tokens: 72_000,
        input_context_tokens: 64_000,
        default_for_provider: true,
        reasoning_efforts: vec![ReasoningEffort::Low, ReasoningEffort::High],
        supports_vision: true,
        config: Default::default(),
    }];
    let context = || CommandContext {
        tools: &tools,
        instructions: &instructions,
        skills: &skills,
        mcp_statuses: &[],
        model_options: &options,
    };
    for command in [
        "/mode ask",
        "/rename Orbit home",
        "/model orbit::fast",
        "/reasoning high",
        "/tasks add Orbit task",
        "/memory add fact Orbit memory",
    ] {
        run_command(
            command,
            &mut state,
            &mut model,
            &mut history,
            &mut session,
            context(),
        )
        .await
        .unwrap();
        let screen = render(&mut state, 140, 40);
        assert!(state.transcript.is_empty(), "{command}");
        assert!(state.info_dialog.is_none(), "{command}");
        assert_eq!(state.input_area, input_area, "{command}");
        assert!(!screen.contains('→'), "{command}");
    }
    assert_eq!(session.store.tui_mode(session.id()).await.unwrap(), "ask");
    let saved = session.store.load(session.id()).await.unwrap();
    assert_eq!(saved.session.title, "Orbit home");
    assert_eq!(saved.session.model, "orbit::fast");
    assert_eq!(saved.session.reasoning_effort.as_deref(), Some("high"));
    assert_eq!(state.reasoning_effort, Some(ReasoningEffort::High));
    assert_eq!(state.input_context_tokens, 64_000);
    let tasks = session
        .store
        .tasks(&session.snapshot.session.project_id, false)
        .await
        .unwrap();
    assert_eq!(tasks[0].title, "Orbit task");
    let memories = session
        .store
        .session_memories(session.id(), false)
        .await
        .unwrap();
    assert_eq!(memories[0].content, "Orbit memory");
    for command in [
        "/mode",
        "/status",
        "/context",
        "/skills",
        "/mcp",
        "/tasks",
        "/memory",
        "/memory search Orbit",
        "/changes",
        "/diff",
        "/diff orbit.rs",
        "/diagnostics",
        "/orbit-unknown",
        "/model orbit::missing",
        "/reasoning missing",
        "/rename",
    ] {
        run_command(
            command,
            &mut state,
            &mut model,
            &mut history,
            &mut session,
            context(),
        )
        .await
        .unwrap();
        render(&mut state, 140, 40);
        assert!(
            !state.info_dialog.as_ref().unwrap().content.is_empty(),
            "{command}"
        );
        if command == "/context" {
            let content = &state.info_dialog.as_ref().unwrap().content;
            assert!(content.contains("原始上下文估算"));
            assert!(content.contains("校准后总计"));
        }
        assert!(state.transcript.is_empty(), "{command}");
        assert_eq!(state.status, RunStatus::Idle);
        assert_eq!(state.selection.viewport.height, 0);
        dialogs::handle(
            &mut state,
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
        );
        render(&mut state, 140, 40);
        assert_eq!(state.input_area, input_area);
        assert!(state.is_home());
    }
    for (width, height) in [(28, 18), (20, 10)] {
        render(&mut state, width, height);
        assert_eq!(state.selection.viewport.height, 0);
        assert_eq!(state.max_scroll, 0);
    }
    state.submit("Orbit message".into());
    let screen = render(&mut state, 140, 40);
    assert!(state.has_conversation());
    assert!(state.input_area.x < 20);
    assert_eq!(state.new_tab_area.height, 1);
    assert!(!screen.contains("未知命令"));
    run_command(
        "/mode auto",
        &mut state,
        &mut model,
        &mut history,
        &mut session,
        context(),
    )
    .await
    .unwrap();
    assert!(matches!(
        state.transcript.last(),
        Some(TranscriptItem::CommandResult(_))
    ));
    assert!(state.info_dialog.is_none());
}

#[test]
fn composer_metadata_is_below_input_without_a_background_or_sidebar() {
    for width in [30, 140] {
        let mut state = UiState::new("provider::test-model");
        state.reasoning_effort = Some(orbit_model::types::ReasoningEffort::High);
        state.input.set("draft".into());
        let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();

        terminal.draw(|frame| draw(frame, &mut state)).unwrap();

        let buffer = terminal.backend().buffer();
        let metadata: String = (state.input_area.bottom()..22)
            .flat_map(|y| (0..width).map(move |x| buffer[(x, y)].symbol()))
            .collect();

        for label in ["auto", "test-model", "provider", "high"] {
            assert!(metadata.contains(label), "{width}: {metadata:?}");
        }

        assert!(state.selection.viewport.right() > width - 10);
        assert!(
            buffer.content.iter().all(|cell| {
                cell.bg == Color::Reset || cell.bg == crate::tui::theme::THEME.caret
            })
        );
        assert!(
            composer_details(&state, width.saturating_sub(7) as usize)
                .iter()
                .all(|line| line.width() <= width.saturating_sub(7) as usize)
        );
    }
}

#[test]
fn selected_tab_remains_visible_and_clickable_at_narrow_width() {
    let mut state = UiState::new("model");
    state.tabs = (0..5)
        .map(|index| SessionTab {
            waiting_approval: false,
            elapsed: std::time::Duration::ZERO,
            id: format!("session-{index}"),
            title: format!("Conversation {index}"),
            status: RunStatus::Idle,
            active: index == 4,
        })
        .collect();
    let mut terminal = Terminal::new(TestBackend::new(40, 16)).unwrap();

    terminal.draw(|frame| draw(frame, &mut state)).unwrap();

    assert!(state.tab_areas.iter().any(|(_, id)| id == "session-4"));
    assert!(state.tab_areas.iter().all(|(area, _)| area.right() <= 40));
}

#[test]
fn tool_previews_bound_single_line_payloads_without_changing_results() {
    use super::super::app::ToolRow;

    let output = "中文".repeat(20000);
    let item = TranscriptItem::Tool(ToolRow {
        id: "write".into(),
        name: "write_file".into(),
        arguments: serde_json::json!({ "path": "report.md", "content": output }).to_string(),
        status: ToolStatus::Finished,
        output: output.clone(),
    });
    let short = transcript_lines(std::slice::from_ref(&item), false);

    assert!(
        short[0]
            .to_string()
            .contains("report.md · 1 行 · 120000 字节")
    );
    assert!(short.len() <= 9);
    assert!(short.last().unwrap().to_string().is_empty());
    assert!(!short.iter().any(|line| line.to_string().contains("[展开]")));
    let expanded = transcript_lines(std::slice::from_ref(&item), true);
    assert!(expanded.len() > short.len());
    let TranscriptItem::Tool(row) = &item else {
        unreachable!()
    };
    assert_eq!(row.output, output);
    assert!(
        tool_summary(&serde_json::json!({ "command": "x".repeat(10000) }).to_string())
            .chars()
            .count()
            < 170
    );
}

#[test]
fn tool_card_collapsed_single_line_and_expanded_box_click_behavior() {
    use super::super::app::ToolRow;
    use ratatui::layout::Rect;

    let row = ToolRow {
        id: "write_1".into(),
        name: "write_file".into(),
        arguments: serde_json::json!({
            "path": "hello.py",
            "content": "print(\"Hello, world!\")"
        })
        .to_string(),
        status: ToolStatus::Finished,
        output: serde_json::json!({ "written_bytes": 22, "created": true }).to_string(),
    };

    // Collapsed state: strictly single line command
    let collapsed = tool_card(&row, false, 80);
    assert_eq!(collapsed.len(), 1);
    assert!(
        collapsed[0]
            .to_string()
            .starts_with("  write_file hello.py")
    );
    assert!(!collapsed[0].to_string().contains(icons::CHECK));
    assert!(!collapsed[0].to_string().contains(icons::LOADING));
    assert!(!collapsed[0].to_string().contains(icons::CLOSE));

    // Expanded state: rounded box with breathing blank line and line-numbered code
    let expanded = tool_card(&row, true, 80);
    assert!(expanded.len() >= 4);
    assert!(expanded[0].to_string().starts_with('╭') && expanded[0].to_string().ends_with('╮'));
    assert!(expanded[1].to_string().contains("write_file hello.py"));
    assert!(expanded[2].to_string().starts_with("│ ") && expanded[2].to_string().ends_with(" │"));
    assert!(
        expanded[3]
            .to_string()
            .contains("1 │ print(\"Hello, world!\")")
    );
    let bottom = &expanded[expanded.len() - 1];
    assert!(bottom.to_string().starts_with('╰') && bottom.to_string().ends_with('╯'));

    for line in &expanded {
        assert_eq!(line.width(), 80);
    }

    // Click testing: any position inside the card (top border, header, blank line, code, bottom border) returns Some(0)
    let items = vec![TranscriptItem::Tool(row)];
    let mut layout = super::super::transcript::TranscriptLayout::default();
    let area = Rect::new(0, 0, 80, 40);

    layout.update(&items, Some(0), 80);
    let card_height = layout.height() - 1;
    for y in 0..card_height {
        assert_eq!(
            layout.disclosure_at(&items, area, 0, (0, y as u16)),
            Some(0)
        );
        assert_eq!(
            layout.disclosure_at(&items, area, 0, (2, y as u16)),
            Some(0)
        );
        assert_eq!(
            layout.disclosure_at(&items, area, 0, (30, y as u16)),
            Some(0)
        );
        assert_eq!(
            layout.disclosure_at(&items, area, 0, (70, y as u16)),
            Some(0)
        );
        assert_eq!(
            layout.disclosure_at(&items, area, 0, (79, y as u16)),
            Some(0)
        );
        assert_eq!(layout.disclosure_at(&items, area, 0, (80, y as u16)), None);
    }
}

#[test]
fn tool_card_edit_file_renders_diff_backgrounds_across_row() {
    use super::super::app::ToolRow;

    let row = ToolRow {
        id: "edit_1".into(),
        name: "edit_file".into(),
        arguments: serde_json::json!({
            "path": "hello.py",
            "old_text": "x = 1",
            "new_text": "x = 2"
        })
        .to_string(),
        status: ToolStatus::Finished,
        output: "success".into(),
    };

    let card = tool_card(&row, true, 80);
    for line in &card {
        assert_eq!(line.width(), 80);
    }

    let rem_line = card
        .iter()
        .find(|l| l.to_string().contains("-   1 │ x = 1"))
        .expect("removed line found");
    let add_line = card
        .iter()
        .find(|l| l.to_string().contains("+   1 │ x = 2"))
        .expect("added line found");

    let rem_bg = Some(crate::tui::theme::THEME.diff_removed_bg);
    assert_eq!(rem_line.spans[0].content, "│");
    assert_eq!(rem_line.spans[1].style.bg, rem_bg);
    assert_eq!(rem_line.spans.last().unwrap().content, "│");
    assert_eq!(rem_line.spans[rem_line.spans.len() - 2].style.bg, rem_bg);

    let add_bg = Some(crate::tui::theme::THEME.diff_added_bg);
    assert_eq!(add_line.spans[0].content, "│");
    assert_eq!(add_line.spans[1].style.bg, add_bg);
    assert_eq!(add_line.spans.last().unwrap().content, "│");
    assert_eq!(add_line.spans[add_line.spans.len() - 2].style.bg, add_bg);
}

#[test]
fn file_previews_keep_markdown_source_while_fetched_documents_render_structure() {
    use super::super::app::ToolRow;
    let content = "# Orbit Guide\n\n- **Orbit** uses `Rust`.\n\n| Item | Link |\n| --- | --- |\n| Docs | [Orbit](https://orbit.example/docs) |\n\n```rust\nlet url = \"https://orbit.example/code\";\n```\n\n---";
    let numbered = content
        .lines()
        .enumerate()
        .map(|(index, line)| format!("{}: {line}\n", index + 40))
        .collect::<String>();
    for name in ["read_file", "write_file", "web_fetch", "read_document"] {
        let row = ToolRow {
            id: "orbit-preview".into(),
            name: name.into(),
            arguments: serde_json::json!({ "path": "docs/Orbit Guide.MD", "content": content })
                .to_string(),
            status: ToolStatus::Finished,
            output: serde_json::json!({
                "path": "docs/Orbit Guide.MD",
                "format": "pdf",
                "content": if name == "read_file" { &numbered } else { content },
                "start_line": 40,
                "end_line": 53,
                "start_char": 0,
                "total_chars": content.chars().count(),
                "truncated": false
            })
            .to_string(),
        };
        let original = row.output.clone();
        for width in [10, 16, 32, 80, 180] {
            let card = tool_card_with_links(&row, true, width);
            assert!(
                card.iter()
                    .all(|line| line.line.width() == usize::from(width)),
                "{name} {width}"
            );
            let text = card
                .iter()
                .map(|linked| linked.line.to_string())
                .collect::<Vec<_>>()
                .join("\n");
            let source = matches!(name, "read_file" | "write_file");
            let links: Vec<_> = card.iter().flat_map(|line| &line.links).collect();
            assert!(
                width == 10
                    || links
                        .iter()
                        .any(|link| link.url == "https://orbit.example/docs"),
                "{name} {width}"
            );
            assert!(
                width == 10
                    || links
                        .iter()
                        .any(|link| link.url == "https://orbit.example/code"),
                "{name} {width}"
            );
            for linked in &card {
                for link in &linked.links {
                    let text = linked.line.to_string();
                    let label = &text[link.start..link.end];
                    assert!(
                        !label.is_empty() && !label.contains('│'),
                        "{name} {width}: {label}"
                    );
                }
            }
            if width >= 80 {
                assert!(text.contains("Orbit Guide"));
                if source {
                    assert!(text.contains("# Orbit Guide"));
                    assert!(text.contains("- **Orbit** uses `Rust`."));
                    assert!(!text.contains('┬'));
                } else {
                    assert!(text.contains("• Orbit uses Rust."));
                    assert!(text.contains('┬'));
                    assert!(!text.contains("https://orbit.example/docs"));
                }
            }
            if !source && width >= 12 {
                assert!(
                    card.iter()
                        .any(|linked| linked.line.spans.iter().any(|span| {
                            span.style.fg == Some(super::super::theme::THEME.md_rule)
                                && span.content == "─".repeat(usize::from(width) - 8)
                        })),
                    "{name} {width}"
                );
            }
        }
        assert_eq!(row.output, original);
    }
}

#[test]
fn web_fetch_title_is_not_a_link_but_wrapped_body_urls_keep_the_full_target() {
    use super::super::app::ToolRow;
    let url = "https://orbit.example/docs/rendering/";
    let row = ToolRow {
        id: "orbit-fetch".into(), name: "web_fetch".into(),
        arguments: serde_json::json!({"url":url}).to_string(),
        status: ToolStatus::Finished,
        output: serde_json::json!({"url":url, "content":"[Orbit 文档](https://orbit.example/docs/rendering/)", "total_chars":64}).to_string(),
    };
    for width in [16, 32, 80] {
        let collapsed = tool_card_with_links(&row, false, width);
        assert!(collapsed.iter().all(|line| line.links.is_empty()));
        let expanded = tool_card_with_links(&row, true, width);
        let blank = expanded
            .iter()
            .position(|line| line.line.to_string().trim_matches(['│', ' ']).is_empty())
            .unwrap();
        assert!(expanded[..blank].iter().all(|line| line.links.is_empty()));
        let targets: Vec<_> = expanded[blank..]
            .iter()
            .flat_map(|line| &line.links)
            .collect();
        assert!(targets.len() > 1);
        assert!(targets.iter().all(|link| link.url == url));
        assert!(expanded.iter().any(|line| line.links.iter().any(|link| {
            line.line
                .to_string()
                .get(link.start..link.end)
                .is_some_and(|label| label.contains("Orbit"))
        })));
    }
}

#[test]
fn source_colors_and_multiline_state_match_across_code_previews() {
    use super::super::{app::ToolRow, approval_diff, markdown};
    let content =
        "/* Orbit block\nfn inside_comment() {}\n*/\nfn main() {\n    let name = \"orbit\";\n}";
    let samples: Vec<_> = content.lines().collect();
    let numbered = samples
        .iter()
        .enumerate()
        .map(|(index, line)| format!("{}: {line}\n", index + 1))
        .collect::<String>();
    let tools: Vec<_> = ["read_file", "write_file", "edit_file"].into_iter().map(|name| ToolRow {
        id: "orbit-code".into(),
        name: name.into(),
        arguments: serde_json::json!({ "path": "src/Orbit Code.RS", "content": content, "old_text": "", "new_text": content }).to_string(),
        status: ToolStatus::Finished,
        output: serde_json::json!({ "path": "src/Orbit Code.RS", "content": numbered, "end_line": samples.len() }).to_string(),
    }).collect();
    let cards: Vec<_> = tools.iter().map(|row| tool_card(row, true, 100)).collect();
    let message = markdown::document(&format!("```rust\n{content}\n```"), 100);
    let diff = approval_diff::Cache::new("", content, (100, 100), true, "src/Orbit Code.RS");
    let colors = |line: &Line<'static>, source: &str| {
        let start = line.to_string().find(source).unwrap();
        let colors: Vec<_> = line
            .spans
            .iter()
            .flat_map(|span| std::iter::repeat_n(span.style.fg, span.content.len()))
            .collect();
        colors[start..start + source.len()].to_vec()
    };
    for (index, sample) in samples.iter().enumerate() {
        let expected = colors(&message[index + 1].line, sample);
        for card in &cards {
            assert_eq!(colors(&card[index + 3], sample), expected, "{sample}");
        }
        assert_eq!(colors(&diff.right[index], sample), expected, "{sample}");
    }
    let comment_colors = colors(&message[2].line, samples[1]);
    assert!(
        comment_colors
            .iter()
            .all(|color| *color == Some(Color::Rgb(163, 173, 190)))
    );
    assert_eq!(
        colors(&message[4].line, samples[3])[0],
        Some(Color::Rgb(198, 120, 221))
    );
}

#[test]
fn collapsed_tools_keep_one_blank_line_between_them() {
    use super::super::app::ToolRow;

    let row1 = ToolRow {
        id: "1".into(),
        name: "run_command".into(),
        arguments: serde_json::json!({ "command": "git status" }).to_string(),
        status: ToolStatus::Finished,
        output: "clean".into(),
    };
    let row2 = ToolRow {
        id: "2".into(),
        name: "read_file".into(),
        arguments: serde_json::json!({ "path": "main.rs" }).to_string(),
        status: ToolStatus::Finished,
        output: "fn main() {}".into(),
    };
    let items = vec![TranscriptItem::Tool(row1), TranscriptItem::Tool(row2)];
    let lines = transcript_lines(&items, false);
    assert_eq!(lines.len(), 4);
    assert!(lines[0].to_string().contains("run_command"));
    assert!(lines[1].to_string().is_empty());
    assert!(lines[2].to_string().contains("read_file"));
    assert!(lines[3].to_string().is_empty());

    let mut layout = super::super::transcript::TranscriptLayout::default();
    layout.update(&items, None, 80);
    assert_eq!(layout.height(), 4);
}

#[test]
fn hovering_expanded_thinking_content_preserves_its_syntax_style() {
    let mut thinking =
        super::super::app::ThinkingRow::from("```rust\nlet orbit = 1;\n```".to_owned());
    thinking.finished = true;
    let mut state = UiState::new("orbit-model");
    state.transcript.push(TranscriptItem::Thinking(thinking));
    state.expanded.insert(0);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let before = terminal.backend().buffer().clone();
    let area = state.selection.viewport;
    let y = (area.y..area.bottom())
        .find(|y| {
            (area.x..area.right())
                .map(|x| before[(x, *y)].symbol())
                .collect::<String>()
                .contains("let orbit")
        })
        .unwrap();
    let point = (area.x + 8, y);
    assert_eq!(
        state.transcript_layout.disclosure_at(
            &state.transcript,
            area,
            state.max_scroll.saturating_sub(state.scroll),
            point,
        ),
        Some(0)
    );
    state.transcript_pointer = Some(point);
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    for x in area.x..area.right() {
        assert_eq!(terminal.backend().buffer()[(x, y)], before[(x, y)]);
    }
}

#[test]
fn command_preview_skips_a_shell_safety_preamble() {
    let summary = tool_summary(
        &serde_json::json!({ "command": "set -e\ncargo test --workspace --all-features" })
            .to_string(),
    );

    assert!(summary.starts_with("cargo test --workspace"), "{summary}");
    assert!(!summary.starts_with("set -e"), "{summary}");
}

#[test]
fn command_menu_keeps_composer_anchored_and_highlights_the_entire_selection() {
    for home in [false, true] {
        let mut state = UiState::new("model");

        if home {
            state.tabs.push(SessionTab {
                waiting_approval: false,
                elapsed: std::time::Duration::ZERO,
                id: "test".into(),
                title: "新会话".into(),
                active: true,
                status: RunStatus::Idle,
            });
        }

        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let input = state.input_area;
        let viewport = state.selection.viewport;
        let scroll = state.max_scroll;
        state.input.set("/".into());

        terminal.draw(|frame| draw(frame, &mut state)).unwrap();

        assert_eq!(state.input_area, input);
        assert_eq!(state.selection.viewport, viewport);
        assert_eq!(state.max_scroll, scroll);
        let buffer = terminal.backend().buffer();
        let selected = buffer
            .content
            .iter()
            .filter(|cell| cell.bg == ACCENT)
            .map(|cell| cell.symbol().width())
            .sum::<usize>();
        let selected_area = state
            .menu_rows
            .iter()
            .find(|(_, index)| *index == state.menu_index)
            .unwrap()
            .0;
        assert_eq!(selected, selected_area.width as usize);
        assert!(selected < if home { 82 } else { 96 });
    }
}

#[test]
fn command_menu_limits_visible_items_and_tracks_selection() {
    let mut state = UiState::new("model");
    state.input.set("/".into());
    let first_page = render(&mut state, 100, 20);

    assert!(first_page.contains("/new"));
    assert!(!first_page.contains("/tasks"));

    assert!(!first_page.contains("Tab 补全"));

    state.menu_index = 6;
    let middle_page = render(&mut state, 100, 20);

    assert!(middle_page.contains("  /resume"));
    assert!(middle_page.contains("/new"));
    assert_eq!(state.menu_offset, 0);

    state.menu_index = state.menu().len() - 1;
    let last_page = render(&mut state, 100, 20);

    assert!(last_page.contains("/exit"));
    assert!(!last_page.contains("/new"));
    assert!(!last_page.contains("Esc 关闭"));
}

fn render(state: &mut UiState, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| draw(frame, state)).unwrap();

    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn reasoning_is_visible_and_expansion_only_affects_one_visible_collapsible() {
    use super::super::app::ToolRow;

    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    let mut state = UiState::new("model");
    state.apply(orbit_agent::events::AgentEvent::Model(
        orbit_model::types::StreamEvent::ReasoningDelta("reasoning visible now".into()),
    ));

    assert!(render(&mut state, 100, 30).contains("reasoning visible now"));

    for (id, name) in [("older", "read_file"), ("latest", "search_text")] {
        state.transcript.push(TranscriptItem::Tool(ToolRow {
            id: id.into(),
            name: name.into(),
            arguments: "{}".into(),
            status: ToolStatus::Finished,
            output: format!("first\nsecond\nthird\n{id}-detail"),
        }));
    }

    let collapsed = render(&mut state, 100, 30);

    assert!(!collapsed.contains("latest-detail"));
    assert!(!collapsed.contains("ctrl+o"));

    super::super::keyboard_action(
        &mut state,
        Event::Key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)),
        false,
    );
    let expanded = render(&mut state, 100, 30);

    assert!(expanded.contains("latest-detail"));
    assert!(!expanded.contains("older-detail"));
    assert!(expanded.contains("reasoning visible now"));

    super::super::keyboard_action(
        &mut state,
        Event::Key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)),
        false,
    );

    assert!(!render(&mut state, 100, 30).contains("latest-detail"));

    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let buffer = terminal.backend().buffer();
    let footer: String = (0..100).map(|x| buffer[(x, 29)].symbol()).collect();

    assert!(footer.contains("/96k"));
    assert!(!footer.contains("model") && !footer.contains(state.permission_mode.label()));
    assert!(!footer.contains("Shift") && !footer.contains("恢复"));
}

#[test]
fn clicking_memory_hint_toggles_that_memory_instead_of_the_last_tool() {
    use super::super::app::ToolRow;

    use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    let mut state = UiState::new("model");
    state.transcript.push(TranscriptItem::Tool(ToolRow {
        id: "memory".into(),
        name: "memory_save".into(),
        status: ToolStatus::Finished,
        arguments: r#"{"title":"审查记录"}"#.into(),
        output: serde_json::json!({
            "memory": { "category": "fact", "content": "long text ".repeat(40) }
        })
        .to_string(),
    }));
    state.transcript.push(TranscriptItem::Tool(ToolRow {
        id: "later".into(),
        name: "other_tool".into(),
        status: ToolStatus::Finished,
        arguments: "{}".into(),
        output: "first\nsecond\nthird".into(),
    }));
    let mut terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();

    for expected in [Some(0), None] {
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let point = (0..40)
            .find_map(|y| {
                (0..100).find_map(|x| {
                    state
                        .transcript_layout
                        .disclosure_at(
                            &state.transcript,
                            state.selection.viewport,
                            state.max_scroll.saturating_sub(state.scroll),
                            (x, y),
                        )
                        .filter(|index| *index == 0)
                        .map(|_| (x, y))
                })
            })
            .expect("memory toggle is visible");
        let down = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: point.0,
            row: point.1,
            modifiers: KeyModifiers::NONE,
        });
        let up = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: point.0,
            row: point.1,
            modifiers: KeyModifiers::NONE,
        });
        state.selection.handle(&down).unwrap();

        assert_eq!(state.selection.click(&up), Some(point));
        assert!(super::super::toggle_transcript_at(&mut state, point));
        assert_eq!(state.expanded, expected.into_iter().collect());
    }

    assert!(!super::super::toggle_transcript_at(&mut state, (0, 0)));
}

#[test]
fn tool_controls_expand_read_search_and_generic_outputs_and_keep_the_draft() {
    use super::super::app::ToolRow;

    let cases = [
        (
            "read_file",
            serde_json::json!({ "start_line": 1, "end_line": 1, "content": format!("1: {}", "中文".repeat(80)) })
            .to_string(),
        ),
        (
            "search_text",
            serde_json::json!({ "matches": [{ "path": "src/main.rs", "line": 2, "text": "hit", "context": [{ "line": 1, "text": "context" }] }] })
            .to_string(),
        ),
        (
            "run_command",
            serde_json::json!({ "stdout": "first\nsecond\nthird\n", "stderr": "", "exit_code": 0 })
            .to_string(),
        ),
        ("mcp_external", "result ".repeat(80)),
        ("failed_tool", "error detail\nmore\nlast".into()),
    ];

    for (name, output) in cases {
        let mut state = UiState::new("model");
        state.input.set("unfinished draft".into());
        state.transcript.push(TranscriptItem::Tool(ToolRow {
            id: "first".into(),
            name: name.into(),
            arguments: "{}".into(),
            status: if name == "failed_tool" {
                ToolStatus::Failed
            } else {
                ToolStatus::Finished
            },
            output: output.clone(),
        }));

        for expected in [Some(0), None] {
            render(&mut state, 90, 32);
            let area = state.selection.viewport;
            let offset = state.max_scroll.saturating_sub(state.scroll);
            let point = (area.y..area.bottom())
                .find_map(|y| {
                    (area.x..area.right()).find_map(|x| {
                        state
                            .transcript_layout
                            .disclosure_at(&state.transcript, area, offset, (x, y))
                            .map(|_| (x, y))
                    })
                })
                .unwrap_or_else(|| panic!("missing control for {name}"));

            assert!(super::super::toggle_transcript_at(&mut state, point));
            assert_eq!(state.expanded, expected.into_iter().collect());
            assert_eq!(state.input.text, "unfinished draft");

            let TranscriptItem::Tool(row) = &state.transcript[0] else {
                unreachable!()
            };

            assert_eq!(row.output, output);
        }
    }
}

#[test]
fn shortcut_collapses_scrolled_tool_with_clickable_output() {
    use super::super::app::ToolRow;

    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

    let mut state = UiState::new("model");

    for index in 0..3 {
        state.transcript.push(TranscriptItem::Tool(ToolRow {
            id: index.to_string(),
            name: format!("tool_{index}"),
            arguments: "{}".into(),
            status: ToolStatus::Finished,
            output: (0..100).map(|line| format!("detail {line}\n")).collect(),
        }));
    }

    render(&mut state, 90, 24);
    super::super::toggle_transcript_item(&mut state, 1);
    let after_tool = state.transcript_layout.item_offset(2).unwrap();
    state.scroll = state.max_scroll.saturating_sub(after_tool - 5);
    render(&mut state, 90, 24);
    let area = state.selection.viewport;
    let offset = state.max_scroll.saturating_sub(state.scroll);
    assert!((area.y..area.bottom()).any(|y| {
        state
            .transcript_layout
            .disclosure_at(&state.transcript, area, offset, (area.x + 4, y))
            == Some(1)
    }));
    super::super::keyboard_action(
        &mut state,
        Event::Key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)),
        false,
    );
    assert!(state.expanded.is_empty());
    assert!(render(&mut state, 90, 24).contains("tool_1"));

    super::super::toggle_transcript_item(&mut state, 1);
    state.scroll = 0;
    render(&mut state, 90, 24);
    super::super::keyboard_action(
        &mut state,
        Event::Key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL)),
        true,
    );

    assert_eq!(
        state.expanded,
        Default::default(),
        "must collapse tool_1 instead of expanding tool_2"
    );
}

#[test]
fn model_change_uses_the_same_result_style_as_other_commands() {
    let item = TranscriptItem::CommandResult("qwen::old → qwen::new".into());
    let lines = transcript_lines(&[item], false);

    assert_eq!(lines[0].to_string(), "  qwen::old → qwen::new");
}

#[test]
fn command_results_use_a_distinct_result_rail() {
    let lines = transcript_lines(
        &[
            TranscriptItem::CommandResult("状态\n细节".into()),
            TranscriptItem::CommandError("未知命令：/stauts。是否想输入 /status？".into()),
        ],
        false,
    );

    assert_eq!(lines[0].to_string(), "  状态");
    assert_eq!(lines[1].to_string(), "  细节");
    assert!(
        lines[3]
            .to_string()
            .starts_with(&format!("{} 未知命令", icons::CIRCLE))
    );
    assert!(
        lines
            .iter()
            .all(|line| !line.to_string().contains("/model asd"))
    );
}

#[test]
fn welcome_commands_and_small_terminal_render() {
    let mut state = UiState::new("mock-model");
    let screen = render(&mut state, 100, 30);

    assert!(screen.contains("████"));
    assert!(screen.contains("mock-model"));
    assert!(!screen.contains("开始一段新对话"));
    assert!(!screen.contains("/help"));

    state.input.insert("/");

    assert!(render(&mut state, 100, 30).contains("/new"));

    for (width, height) in [(20, 5), (10, 3), (1, 1)] {
        assert!(!render(&mut state, width, height).contains("请放大终端"));
    }
}

#[test]
fn running_status_shows_the_actual_cancel_shortcut() {
    let mut state = UiState::new("mock-model");
    state.begin_run();
    let screen = render(&mut state, 100, 24);
    let compact = screen.replace(' ', "");

    assert!(compact.contains("Ctrl+C取消"));
    assert!(!compact.contains("esc取消"));
    assert!(!compact.contains("第1轮"));
    assert!(!compact.contains("0.0s"));

    state.status = RunStatus::Completed;
    state.started_at = None;
    state.elapsed = std::time::Duration::from_millis(7500);
    let completed = render(&mut state, 100, 24);
    assert!(!completed.contains("7.5s"));
}

#[test]
fn pending_images_render_as_chips_above_the_prompt() {
    let mut state = UiState::new("mock");
    state
        .attachments
        .push(orbit_model::types::Attachment::Image {
            name: "capture.png".into(),
            mime_type: "image/png".into(),
            path: "/tmp/unused.png".into(),
        });
    state.input.insert("describe this image");
    let screen = render(&mut state, 80, 24);

    assert!(!screen.contains("capture.png"));
    assert!(screen.contains("describe this image"));
    assert!(state.input_area.y > 0);
    assert_eq!(state.attachment_areas.len(), 1);

    state.attachment_cursor = Some(0);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();

    assert!(!terminal.backend().cursor_visible());

    let chip = state.attachment_areas[0];

    assert_eq!(terminal.backend().buffer()[(chip.x, chip.y)].symbol(), "图");
    assert_eq!(
        terminal.backend().buffer()[(chip.x + 2, chip.y)].symbol(),
        "片"
    );
    assert_eq!(
        terminal.backend().buffer()[(chip.x + 5, chip.y)].symbol(),
        "1"
    );

    assert_eq!(terminal.backend().buffer()[(chip.x, chip.y)].bg, ACCENT);
}

#[test]
fn details_hover_fills_spaces_and_wide_glyphs_without_underlining_the_caption() {
    let mut state = UiState::new("mock");
    state.transcript.push(TranscriptItem::Assistant(
        "<details><summary>点击展开 / 折叠</summary>正文</details>".into(),
    ));
    let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
    terminal.draw(|f| draw(f, &mut state)).unwrap();
    let area = state.selection.viewport;
    let point = (area.y..area.bottom())
        .find_map(|y| {
            (area.x..area.right()).find_map(|x| {
                state
                    .transcript_layout
                    .html_disclosure_at(area, 0, (x, y))
                    .map(|_| (x, y))
            })
        })
        .unwrap();
    state.transcript_pointer = Some(point);
    let mut first = 0;
    terminal
        .draw(|f| {
            draw(f, &mut state);
            // Inspect the frame: terminal diffs omit the continuation cells
            // of wide glyphs, whose style is carried by the leading cell.
            let buffer = f.buffer_mut();
            first = (area.x..area.right())
                .find(|&x| buffer[(x, point.1)].symbol() == icons::CHEVRON_RIGHT)
                .unwrap();
            let end = first + 2 + "点击展开 / 折叠".width() as u16;
            for x in first..end {
                assert_eq!(buffer[(x, point.1)].bg, THEME.tab_hover_background);
                assert!(!buffer[(x, point.1)].modifier.contains(Modifier::UNDERLINED));
            }
            assert_eq!(buffer[(first - 1, point.1)].bg, Color::Reset);
        })
        .unwrap();
    state.transcript_pointer = None;
    terminal.draw(|f| draw(f, &mut state)).unwrap();
    assert_eq!(
        terminal.backend().buffer()[(first, point.1)].bg,
        Color::Reset
    );
}

#[test]
fn file_approval_keeps_conversation_and_draft_visible() {
    let mut state = UiState::new("mock");
    state.transcript.push(TranscriptItem::Assistant(
        "Conversation remains visible".into(),
    ));
    state.input.insert("my draft");
    state.approval = Some(orbit_tools::approval::ApprovalPreview::Write(
        orbit_tools::approval::WritePreview {
            path: "price.c".into(),
            before: Some("return subtotal > 10000;\n".into()),
            after: "return subtotal >= 10000;\n".into(),
        },
    ));
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();

    assert!(text.contains("Conversation remains visible"));
    assert_eq!(state.input.text, "my draft");
    assert!(state.selection.viewport.height > 0);
    assert!(state.input_area.height > 0);
    assert!(text.contains("price.c"));
    assert!(text.contains("return subtotal > 10000;"));
    assert!(text.contains("return subtotal >= 10000;"));
    assert!(text.replace(' ', "").contains("允许"), "{text}");
    assert!(state.approval_shown);
}

#[test]
fn transcript_and_footer_selection_keeps_the_native_input_cursor() {
    use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    let mut state = UiState::new("orbit-model");
    state.input.insert("你好 draft");
    state.submit("Orbit".into());
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    for footer in [false, true] {
        state.selection.clear();
        terminal
            .draw(|frame| {
                draw(frame, &mut state);
                state.selection.paint(frame);
            })
            .unwrap();
        let caret = state.ime_cursor.unwrap();
        let cursor = state.input.cursor;
        let area = if footer {
            state.selection.footer
        } else {
            state.selection.viewport
        };
        for (kind, column) in [
            (MouseEventKind::Down(MouseButton::Left), area.x),
            (MouseEventKind::Drag(MouseButton::Left), area.right() - 1),
            (MouseEventKind::Up(MouseButton::Left), area.right() - 1),
        ] {
            state
                .selection
                .handle(&Event::Mouse(MouseEvent {
                    kind,
                    column,
                    row: if footer || matches!(kind, MouseEventKind::Down(_)) {
                        area.y
                    } else {
                        area.bottom() - 1
                    },
                    modifiers: KeyModifiers::NONE,
                }))
                .unwrap();
        }
        terminal
            .draw(|frame| {
                assert!(state.selection.draw_frozen(frame));
                frame.set_cursor_position(caret);
            })
            .unwrap();
        assert_eq!(state.input.cursor, cursor);
        assert!(terminal.backend().cursor_visible());
        terminal.backend_mut().assert_cursor_position(caret);
        if footer {
            assert!(state.selection.text().contains("/96k"));
        } else {
            assert!(state.selection.text().contains("Orbit"));
        }
    }
}

#[test]
fn native_cursor_tracks_unicode_input_streaming_and_dialog_focus() {
    let mut state = UiState::new("model");
    state.workspace = "/tmp/中文项目".into();
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();

    for (input, column) in [("", 4), ("你好", 8), ("你e\u{301}", 7), ("第一行\n你好", 8)] {
        state.input.set(input.into());
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let position = state.ime_cursor.unwrap();

        assert_eq!(position.x, column);

        terminal.backend_mut().assert_cursor_position(position);

        assert!(terminal.backend().cursor_visible());
        assert_eq!(
            terminal.backend().buffer()[(position.x, position.y)].bg,
            Color::Reset
        );

        // Streaming output must not leave the IME attached to the transcript.
        state.apply(orbit_agent::events::AgentEvent::Model(
            orbit_model::types::StreamEvent::TextDelta("输出中".into()),
        ));
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        terminal.backend_mut().assert_cursor_position(position);
    }

    state.picker = Some(crate::tui::dialogs::SessionPicker::new(Vec::new()));
    crate::tui::dialogs::handle(
        &mut state,
        crossterm::event::Event::Paste("中文".repeat(80)),
    );
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let position = state.ime_cursor.unwrap();
    terminal.backend_mut().assert_cursor_position(position);

    assert_eq!(
        terminal.backend().buffer()[(position.x, position.y)].symbol(),
        " "
    );
    assert!(terminal.backend().cursor_visible());
}

#[test]
fn native_caret_does_not_overwrite_wide_characters_or_the_end_of_full_rows() {
    let mut state = UiState::new("orbit-model");
    state.input.insert("你好世界中文");
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    while !state.input.text.is_empty() {
        state.input.backspace();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let caret = state.ime_cursor.unwrap();
        assert!(terminal.backend().cursor_visible());
        terminal.backend_mut().assert_cursor_position(caret);
        assert_eq!(
            terminal.backend().buffer()[(caret.x, caret.y)].symbol(),
            " "
        );
        assert_eq!(
            terminal.backend().buffer()[(caret.x + 1, caret.y)].bg,
            Color::Reset
        );
    }
    state.input.set(format!("{}\nnext", "a".repeat(74)));
    state.input.set_cursor(74);
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let caret = state.ime_cursor.unwrap();
    assert_eq!(caret.x, state.input_area.right() - 1);
    assert_eq!(
        terminal.backend().buffer()[(caret.x - 1, caret.y)].symbol(),
        "a"
    );
    assert_eq!(
        terminal.backend().buffer()[(caret.x, caret.y)].symbol(),
        " "
    );
}

#[test]
fn frozen_copy_selection_excludes_transient_cursor_effects() {
    use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    let mut state = UiState::new("orbit-model");
    state.cursor_motion.enabled = true;
    state.submit("Orbit".into());
    state.input.set("draft".into());
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    for text in ["draft", "draft Orbit"] {
        state.input.set(text.into());
        terminal
            .draw(|frame| {
                draw(frame, &mut state);
                state.selection.paint(frame);
                state
                    .cursor_motion
                    .paint(frame.buffer_mut(), std::time::Instant::now());
            })
            .unwrap();
    }
    assert!(state.cursor_motion.active(std::time::Instant::now()));
    let area = state.selection.viewport;
    state
        .selection
        .handle(&Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: area.x,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        }))
        .unwrap();
    state.cursor_motion.clear();
    terminal
        .draw(|frame| {
            assert!(state.selection.draw_frozen(frame));
            let input = state.input_area;
            for x in input.x..input.right() {
                assert_eq!(frame.buffer_mut()[(x, input.y)].bg, Color::Reset);
            }
        })
        .unwrap();
}

#[test]
fn input_selection_paints_whole_graphemes_and_does_not_freeze_the_transcript() {
    let mut state = UiState::new("orbit-model");
    state.input.set("你好👩‍💻 Orbit".into());
    state.input.begin_selection(0);
    state.input.extend_selection("你好👩‍💻".len());
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| {
            draw(frame, &mut state);
            let area = state.input_area;
            let buffer = frame.buffer_mut();
            for x in area.x..area.x + 6 {
                assert_eq!(buffer[(x, area.y)].bg, THEME.selection);
            }
            assert_eq!(buffer[(area.x + 6, area.y)].bg, Color::Reset);
            assert!(!state.selection.draw_frozen(frame));
        })
        .unwrap();
    assert_eq!(state.input.selected_text(), Some("你好👩‍💻"));
}

#[test]
fn footer_keeps_project_and_context_with_unicode_paths() {
    let mut state = UiState::new("deepseek");
    state.workspace = format!("/tmp/{}/项目", "长目录/".repeat(40));
    state.context_used_tokens = 51_200;

    for width in [24, 40, 80] {
        let line = footer(&state, width);

        assert!(line.width() <= width);

        let text = line.to_string();

        if width >= 40 {
            assert!(text.contains("项目"));
        }

        if width >= 80 {
            assert!(!text.contains("󰧑"));
            assert!(!text.contains("思考"));
            assert!(text.contains("53%/96k"));
            assert!(!text.contains(state.permission_mode.label()));
            assert!(!text.contains('·'));
        }

        assert!(text.ends_with("  "));
    }

    assert_eq!(fit_label("你好e\u{301}", 3, true), "…e\u{301}");
    assert_eq!(compact_tokens(999), "999");
    assert_eq!(compact_tokens(1_999), "1k");
    assert_eq!(compact_tokens(1_000_000), "1M");
    assert_eq!(compact_tokens(1_500_000), "1.5M");
}

#[test]
fn context_indicator_reveals_its_budget_on_hover() {
    use crossterm::event::{Event, KeyModifiers, MouseEvent, MouseEventKind};

    let mut state = UiState::new("model");
    state.context_used_tokens = 51_200;
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let indicator = state.context_indicator_area;

    assert_eq!(indicator.width, 7);

    let hover = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Moved,
        column: indicator.x,
        row: indicator.y,
        modifiers: KeyModifiers::NONE,
    });

    assert_eq!(handle_context_hover(&mut state, &hover), Some(true));

    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let content: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();

    assert!(
        content.contains("53%/96k") && !content.contains("上下文窗口（本地估算）"),
        "{content}"
    );

    let outside = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Moved,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    });

    assert_eq!(handle_context_hover(&mut state, &outside), Some(true));
}

#[test]
fn small_nonempty_context_is_not_displayed_as_zero() {
    let mut state = UiState::new("model");
    state.input_context_tokens = 224_000;

    for (tokens, expected) in [
        (0, "0%/224k"),
        (1, "<1%/224k"),
        (2_239, "<1%/224k"),
        (2_240, "1%/224k"),
        (224_000, "100%/224k"),
        (322_560, "144%/224k"),
    ] {
        state.context_used_tokens = tokens;

        assert!(footer(&state, 120).to_string().contains(expected));
    }
}

#[test]
fn link_hover_shows_and_clears_its_ctrl_click_hint() {
    use crossterm::event::{Event, KeyModifiers, MouseEvent, MouseEventKind};

    let mut state = UiState::new("model");
    state.transcript.push(TranscriptItem::Assistant(
        "打开 https://example.com/docs 查看文档。".into(),
    ));
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let offset = state.max_scroll;
    let target = (state.selection.viewport.y..state.selection.viewport.bottom()).find_map(|y| {
        (state.selection.viewport.x..state.selection.viewport.right()).find_map(|x| {
            state
                .transcript_layout
                .link_at(state.selection.viewport, offset, (x, y))
                .map(|_| (x, y))
        })
    });
    let (x, y) = target.expect("bare URL has a hit target");
    let hover = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Moved,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    });

    assert_eq!(handle_link_hover(&mut state, &hover), Some(true));

    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let shown: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();

    assert!(shown.replace(' ', "").contains("Ctrl+Click跳转"), "{shown}");

    let leave = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Moved,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    });

    assert_eq!(handle_link_hover(&mut state, &leave), Some(true));

    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let hidden: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();

    assert!(
        !hidden.replace(' ', "").contains("Ctrl+Click跳转"),
        "{hidden}"
    );
}

#[test]
fn repository_url_in_a_table_has_hover_feedback_after_wrapping_and_resize() {
    use crossterm::event::{Event, KeyModifiers, MouseEvent, MouseEventKind};
    use ratatui::buffer::CellWidth;
    let url = "https://github.com/linyisu/orbit";
    let mut state = UiState::new("model");
    state.transcript.push(TranscriptItem::Assistant(format!(
        "Orbit 示例项目\n\n| 项 | 值 |\n| --- | --- |\n| 项目 | Orbit |\n| 仓库 | {url} |\n| 提交署名 | Orbit 测试身份（虚构） |\n| Windows 用户名 | orbit（示例路径 C:\\Users\\orbit\\） |\n| 会话 ID | orbit-test-session |\n| 用途 | TUI 链接测试 |\n"
    )));
    for width in [100, 64, 30] {
        let mut terminal = Terminal::new(TestBackend::new(width, 80)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let area = state.selection.viewport;
        let mut points = Vec::new();
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                if state
                    .transcript_layout
                    .link_at(area, state.max_scroll, (x, y))
                    == Some(url)
                {
                    points.push((x, y));
                }
            }
        }
        assert_eq!(points.len(), url.len(), "width {width}: {points:?}");
        for &(x, y) in [points.first().unwrap(), points.last().unwrap()] {
            let hover = Event::Mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                column: x,
                row: y,
                modifiers: KeyModifiers::NONE,
            });
            handle_link_hover(&mut state, &hover);
            terminal.draw(|frame| draw(frame, &mut state)).unwrap();
            let hint = state.link_tooltip_area;
            assert!(hint.width > 0, "width {width}, point ({x}, {y})");
            let mut text = String::new();
            let mut column = hint.x;
            while column < hint.right() {
                let cell = &terminal.backend().buffer()[(column, hint.y)];
                text.push_str(cell.symbol());
                column += cell.cell_width().max(1);
            }
            assert!(text.replace(' ', "").contains("Ctrl+Click跳转"), "{text}");
        }
        state.link_hovered = None;
    }
}

#[test]
fn output_control_sequences_are_visible_text_not_terminal_commands() {
    assert_eq!(
        safe_output("a\x1b]52;c;secret\x07"),
        "a\\u{1b}]52;c;secret\\u{7}"
    );
}

#[test]
fn thinking_uses_markdown_with_a_subdued_continuous_rail() {
    let mut code = None;
    let line = thinking_line("- **重点** 与 `code`", 0, &mut code);

    assert_eq!(line.spans[0].content, "  ┊ ");
    assert!(
        line.spans
            .iter()
            .any(|span| span.content == "重点" && span.style.add_modifier.contains(Modifier::BOLD))
    );
    assert!(line.spans.iter().any(|span| span.content == "code"
        && span.style.fg == Some(desaturate_for_thinking(Some(THEME.md_inline_code)))));
    assert_eq!(line.style.fg, None);
    assert_ne!(desaturate_for_thinking(Some(ACCENT)), ACCENT);
}

#[test]
fn thinking_code_keeps_a_compound_thinking_and_code_rail() {
    let mut code = None;
    let header = thinking_line("```markdown", 0, &mut code);
    let badge = header
        .spans
        .iter()
        .find(|span| span.style.bg.is_some())
        .unwrap();
    let original = assistant_line("```markdown", 0, &mut None);
    assert_eq!(
        badge.style.bg,
        Some(desaturate_for_thinking(original.spans[1].style.bg))
    );
    assert_eq!(badge.content, " markdown ");
    let line = thinking_line(
        "  nested source that should wrap inside the thought",
        1,
        &mut code,
    );

    assert_eq!(line.spans[0].content, "  ┊ ");
    assert_eq!(line.spans[0].style.fg, Some(THEME.thinking_rail));
    assert_eq!(line.spans[1].content, "│ ");
    let color = super::super::theme::code_label_color("markdown");
    assert_eq!(
        line.spans[1].style.fg,
        Some(desaturate_for_thinking(Some(color)))
    );
    let closing = thinking_line("```", 2, &mut code);
    assert_eq!(closing.spans[0].style.fg, Some(THEME.thinking_rail));
    assert_eq!(closing.spans[1].content, "╰─");
    assert_eq!(closing.spans[1].style.fg, line.spans[1].style.fg);
}

#[test]
fn scrolling_during_streaming_keeps_the_visible_anchor() {
    use orbit_agent::events::AgentEvent;

    use orbit_model::types::StreamEvent;

    let mut state = UiState::new("mock");
    state.apply(AgentEvent::Model(StreamEvent::TextDelta(
        (0..200).map(|i| format!("line {i:03}\n")).collect(),
    )));
    render(&mut state, 80, 24);
    state.scroll_up(30);
    let before = render(&mut state, 80, 24);
    let offset = state.max_scroll - state.scroll;
    state.apply(AgentEvent::Model(StreamEvent::TextDelta(
        "more output\n".repeat(20),
    )));

    assert_eq!(before, render(&mut state, 80, 24));
    assert_eq!(state.max_scroll - state.scroll, offset);

    state.scroll_down(3);
    render(&mut state, 80, 24);

    assert_eq!(state.max_scroll - state.scroll, offset + 3);

    state.scroll_down(usize::MAX);

    assert!(state.follow_bottom);
    assert!(render(&mut state, 80, 24).contains("more output"));
}

#[test]
fn follows_wrapped_answer_to_last_line() {
    let mut state = UiState::new("mock");
    state.transcript.push(TranscriptItem::Assistant(format!(
        "{}\nTHE END",
        "中文长回答".repeat(200)
    )));
    let screen = render(&mut state, 40, 16);

    assert!(
        screen.contains("THE END"),
        "scroll {} screen {screen:?}",
        state.max_scroll
    );
    assert!(state.max_scroll > 10);

    state.scroll_up(8);

    assert!(!render(&mut state, 40, 16).contains("THE END"));
}

#[test]
fn resize_preserves_the_current_source_anchor() {
    let mut state = UiState::new("mock");
    state.transcript.push(TranscriptItem::Assistant(
        (0..80)
            .map(|index| format!("段落 {index:02} {}\n", "中文内容 words ".repeat(8)))
            .collect(),
    ));
    render(&mut state, 90, 18);
    state.scroll_up(70);
    render(&mut state, 90, 18);
    let old_offset = state.max_scroll - state.scroll;
    let anchor = state.transcript_layout.anchor_at(old_offset).unwrap();

    render(&mut state, 48, 18);
    let expected = state
        .transcript_layout
        .offset_for_anchor(anchor)
        .unwrap()
        .min(state.max_scroll);

    assert_eq!(state.max_scroll - state.scroll, expected);
    assert!(!state.follow_bottom);
}
