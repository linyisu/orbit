use ratatui::{Terminal, backend::TestBackend, buffer::CellWidth};

use super::*;
use crate::tui::app::SessionTab;
use crate::tui::theme::desaturate_for_thinking;

#[test]
fn tool_path_labels_preserve_original_arguments_and_image_targets() {
    use super::super::app::ToolRow;
    let raw = r"\\?\D:\generated\sample.txt";
    let visible = r"D:\generated\sample.txt";
    for name in [
        "ReadFile",
        "ReadImage",
        "ReadDocument",
        "RequestAccess",
        "WriteFile",
    ] {
        let row = ToolRow {
            id: "generated-call".into(),
            name: name.into(),
            purpose: None,
            arguments: serde_json::json!({"path": raw, "access": "read"}).to_string(),
            output: serde_json::json!({"path": raw}).to_string(),
            status: ToolStatus::Finished,
            changes: Vec::new(),
        };
        let original = row.arguments.clone();
        for expanded in [false, true] {
            let lines = super::tool_cards::tool_title_with_links(&row, expanded, 160);
            let text = lines
                .iter()
                .map(|line| line.text())
                .collect::<String>();
            assert!(text.contains(visible), "{name}: {text}");
            assert!(!text.contains(r"\\?\"));
        }
        assert_eq!(row.arguments, original);
        if name == "ReadImage" {
            let lines = crate::tui::tool_display::linked_lines(&row, true, 160).unwrap();
            assert_eq!(lines[0].text(), visible);
            assert_eq!(
                lines[0].media[0].source,
                crate::tui::media::Source::Image(raw.into())
            );
        }
    }
}

#[test]
fn path_presentation_does_not_rewrite_command_or_query_text() {
    use super::super::app::ToolRow;
    let command = r"type \\?\D:\generated\sample.txt";
    let row = ToolRow {
        id: "generated-command".into(),
        name: "Exec".into(),
        purpose: None,
        arguments: serde_json::json!({"command": command}).to_string(),
        output: String::new(),
        status: ToolStatus::Finished,
        changes: Vec::new(),
    };
    let lines = super::tool_cards::tool_title_with_links(&row, true, 160);
    assert!(lines[0].text().contains(command));
    let arguments = serde_json::json!({"query": command, "path": r"\\?\D:\generated"});
    let summary = crate::tui::tool_display::search_summary(&arguments.to_string());
    assert!(summary.contains(command));
    assert!(summary.ends_with(r"(D:\generated)"));
}

#[test]
fn browsing_without_an_execution_budget_shows_unknown_context() {
    let mut state = UiState::new("generated-model");
    state.input_context_tokens = 0;
    state.context_used_tokens = 42_000;
    assert_eq!(context_percent_label(&state), "—");
    assert!(
        footer(&state, 100)
            .to_string()
            .contains("上下文 —")
    );
}

#[test]
fn tool_inspectors_switch_calls_without_mutating_the_transcript() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let mut state = UiState::new("fixture");
    for index in 0..2 {
        state
            .transcript
            .push(TranscriptItem::Tool(super::super::app::ToolRow {
            id: index.to_string(),
            name: "Exec".into(),
            purpose: None,
            arguments:
                serde_json::json!({"command": format!("echo {} end-{index}", "long ".repeat(30))})
                    .to_string(),
            output: "first\nsecond".into(),
            status: ToolStatus::Finished,
            changes: vec![],
        }));
    }
    super::super::toggle_transcript_item(&mut state, 0);
    for _ in 0..2 {
        super::super::dialogs::handle(
            &mut state,
            Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)),
        );
    }
    assert_eq!(
        state
            .info_dialog
            .as_ref()
            .unwrap()
            .content,
        "first\nsecond"
    );
    super::super::dialogs::handle(
        &mut state,
        Event::Key(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT)),
    );
    assert!(
        matches!(&state.info_dialog.as_ref().unwrap().source, Some(super::super::dialogs::ReportSource::Tool(id)) if id == "1")
    );
    assert!(state.expanded.is_empty());
    assert_eq!(state.transcript.len(), 2);
}

#[test]
fn parse_ansi_to_spans_strips_charset_and_preserves_clean_text() {
    let raw = "\x1b[31m-\x1b(B\x1b[m\x1b[32m+    let cell = &mut frame;\x1b(B\x1b[m";
    let spans = parse_ansi_to_spans(raw);
    let joined: String = spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect();
    assert!(!joined.contains("\u{1b}"));
    assert!(!joined.contains("(B"));
    assert_eq!(joined, "-+    let cell = &mut frame;");
}

#[test]
fn completed_thinking_drops_trailing_blank_rows_without_changing_content() {
    let mut row: super::super::app::ThinkingRow = "thought\n\n  \n".to_owned().into();
    row.finished = true;
    for (millis, expected) in [
        (None, "思考"),
        (Some(0), "思考 0ms"),
        (Some(75), "思考 75ms"),
        (Some(999), "思考 999ms"),
        (Some(1_000), "思考 1.0s"),
        (Some(7_500), "思考 7.5s"),
    ] {
        row.elapsed = millis.map(std::time::Duration::from_millis);
        let lines = transcript_lines(&[TranscriptItem::Thinking(row.clone())], true);
        assert_eq!(lines[0].to_string().trim(), format!("⌄ {expected}"));
        assert_eq!(lines.len(), 3);
    }
    assert_eq!(row.content, "thought\n\n  \n");
}

#[test]
fn completed_reasoning_collapses_and_header_hover_and_click_reveal_it() {
    use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use orbit_client::protocol::events::AgentEvent;
    use orbit_client::protocol::model::StreamEvent;

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
    row.elapsed = Some(std::time::Duration::from_millis(7_500));
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
    let offset = state
        .max_scroll
        .saturating_sub(state.scroll);
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
    terminal
        .draw(|frame| draw(frame, &mut state))
        .unwrap();
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
    let blank = (state.selection.viewport.right() - 1, point.1 + 1);
    let mouse = |kind, (column, row)| {
        Event::Mouse(MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        })
    };
    state
        .selection
        .handle(&mouse(MouseEventKind::Down(MouseButton::Left), blank))
        .unwrap();
    let released = mouse(MouseEventKind::Up(MouseButton::Left), blank);
    state
        .selection
        .handle(&mouse(
            MouseEventKind::Drag(MouseButton::Left),
            (blank.0 - 1, blank.1),
        ))
        .unwrap();
    assert!(
        state
            .selection
            .click(&released)
            .is_none()
    );
    assert!(state.expanded.contains(&0));
    state.selection.clear();

    state
        .selection
        .handle(&mouse(MouseEventKind::Down(MouseButton::Left), blank))
        .unwrap();
    let clicked = state
        .selection
        .click(&released)
        .unwrap();
    assert!(super::super::toggle_transcript_at(&mut state, clicked));
    assert!(!render(&mut state, 90, 30).contains("private working text"));

    let leave = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Moved,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    });

    assert_eq!(handle_transcript_hover(&mut state, &leave), Some(true));
    terminal
        .draw(|frame| draw(frame, &mut state))
        .unwrap();
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

    assert!(
        frames
            .iter()
            .all(|frame| frame.width() <= 12)
    );
    assert!(
        frames
            .iter()
            .any(|frame| frame.ends_with("最后内容"))
    );
    assert!(
        frames
            .iter()
            .all(|frame| !frame.starts_with('\u{301}'))
    );
}

#[test]
fn new_tab_button_changes_on_hover_and_restores_on_leave() {
    let mut state = UiState::new("model");
    let plain = tab_control_style(&state, &super::super::TabAction::New);
    assert_eq!(plain.bg, Some(THEME.tab_background));
    state.tab_hover = Some((super::super::TabAction::New, std::time::Instant::now()));

    let hovered = tab_control_style(&state, &super::super::TabAction::New);
    assert_ne!(hovered, plain);
    assert_eq!(hovered.bg, Some(THEME.tab_hover_background));

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
    let editor = crate::platform::editor::Editor::new("/bin/true".into());
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
    assert!(
        screen
            .replace(' ', "")
            .contains("尚未发送的草稿")
    );
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

    state
        .transcript
        .push(TranscriptItem::User("Hello".into()));
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

#[test]
fn composer_metadata_is_below_input_without_a_background_or_sidebar() {
    for width in [30, 140] {
        let mut state = UiState::new("provider::test-model");
        state.reasoning_effort = Some(orbit_client::protocol::model::ReasoningEffort::High);
        state.input.set("draft".into());
        let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();

        terminal
            .draw(|frame| draw(frame, &mut state))
            .unwrap();

        let buffer = terminal.backend().buffer();
        let metadata: String = (state.input_area.bottom()..22)
            .flat_map(|y| (0..width).map(move |x| buffer[(x, y)].symbol()))
            .collect();

        for label in ["build", "test-model", "provider", "high"] {
            assert!(metadata.contains(label), "{width}: {metadata:?}");
        }

        assert!(state.selection.viewport.right() > width - 10);
        for y in 0..24 {
            for x in 0..width {
                let cell = &buffer[(x, y)];
                if state
                    .new_tab_area
                    .contains((x, y).into())
                {
                    assert_eq!(cell.bg, THEME.tab_background);
                } else {
                    assert!(cell.bg == Color::Reset || cell.bg == THEME.caret);
                }
            }
        }
        assert!(
            composer_details(&state, width.saturating_sub(7) as usize)
                .iter()
                .all(|line| line.width() <= width.saturating_sub(7) as usize)
        );
    }
}

#[test]
fn short_tabs_fit_their_titles_while_long_tabs_are_capped() {
    let mut state = UiState::new("synthetic-model");
    state.tabs = [
        "Draft",
        "短签",
        "A deliberately lengthy independent session title",
    ]
    .into_iter()
    .enumerate()
    .map(|(index, title)| SessionTab {
        id: format!("generated-{index}"),
        title: title.into(),
        active: index == 2,
        status: RunStatus::Idle,
        waiting_approval: false,
        elapsed: std::time::Duration::ZERO,
    })
    .collect();
    state
        .transcript
        .push(TranscriptItem::User("Generated prompt".into()));
    let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
    terminal
        .draw(|frame| draw(frame, &mut state))
        .unwrap();
    assert_eq!(
        state
            .tab_areas
            .iter()
            .map(|(area, _)| area.width)
            .collect::<Vec<_>>(),
        [10, 9, MAX_SESSION_TAB_WIDTH_COLUMNS as u16]
    );
}

#[test]
fn selected_tab_remains_visible_and_clickable_at_narrow_width() {
    use crate::tui::{TabAction, tab_target};
    use crossterm::event::{Event, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

    let mut state = UiState::new("model");
    state.tabs = (0..5)
        .map(|index| SessionTab {
            waiting_approval: false,
            elapsed: std::time::Duration::ZERO,
            id: format!("session-{index}"),
            title: if index == 3 {
                "独立生成的长标题用于验证窄窗口布局".into()
            } else {
                format!("Tab {index}")
            },
            status: RunStatus::Idle,
            active: index == 4,
        })
        .collect();
    state
        .transcript
        .push(TranscriptItem::User("Hello".into()));
    for width in [40, 100] {
        let mut terminal = Terminal::new(TestBackend::new(width, 16)).unwrap();
        terminal
            .draw(|frame| draw(frame, &mut state))
            .unwrap();
        assert!(
            state
                .tab_areas
                .iter()
                .any(|(_, id)| id == "session-4")
        );
        assert!(
            state
                .tab_areas
                .windows(2)
                .all(
                    |tabs| tabs[0].0.width <= MAX_SESSION_TAB_WIDTH_COLUMNS as u16
                        && tabs[1].0.width <= MAX_SESSION_TAB_WIDTH_COLUMNS as u16
                )
        );
        let click = |x, y| {
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: x,
                row: y,
                modifiers: KeyModifiers::NONE,
            })
        };
        for (area, id) in &state.tab_areas {
            assert!(area.right() <= width);
            assert_eq!(area.height, 1);
            assert!(state.selection.viewport.y >= area.bottom());
            for x in area.x..area.right() - 3 {
                assert_eq!(
                    tab_target(&click(x, area.y), &state),
                    Some(TabAction::Select(id.clone()))
                );
            }
            assert_eq!(tab_target(&click(area.right(), area.y), &state), None);
        }
        for (area, id) in &state.tab_close_areas {
            assert_eq!(area.height, 1);
            assert_eq!(
                tab_target(&click(area.x, area.y), &state),
                Some(TabAction::Close(id.clone()))
            );
        }
        let hovered_id = state.tab_areas[0].1.clone();
        for hover in [
            None,
            Some(TabAction::Select(hovered_id.clone())),
            Some(TabAction::Close(hovered_id.clone())),
            None,
        ] {
            state.tab_hover = hover
                .clone()
                .map(|action| (action, std::time::Instant::now()));
            terminal
                .draw(|frame| draw(frame, &mut state))
                .unwrap();
            for (area, id) in &state.tab_areas {
                let expected = if id == "session-4" {
                    ACCENT
                } else if hover.is_some() && id == &hovered_id {
                    crate::tui::theme::THEME.tab_hover_background
                } else {
                    crate::tui::theme::THEME.tab_background
                };
                let mut next_glyph = area.x;
                for x in area.x..area.right() {
                    // A terminal paints a wide glyph as one unit; its trailing
                    // cells do not have an independently emitted background.
                    if x < next_glyph {
                        continue;
                    }
                    let cell = &terminal.backend().buffer()[(x, area.y)];
                    next_glyph = x + cell.cell_width().max(1);
                    assert!(
                        !cell
                            .modifier
                            .contains(Modifier::UNDERLINED)
                    );
                    if x < area.right() - 3 || hover != Some(TabAction::Close(id.clone())) {
                        assert_eq!(cell.bg, expected);
                    }
                }
                assert!(
                    !terminal.backend().buffer()[(area.right(), area.y)]
                        .modifier
                        .contains(Modifier::UNDERLINED)
                );
            }
        }
    }
}

#[test]
fn hovering_expanded_thinking_content_preserves_its_syntax_style() {
    let mut thinking =
        super::super::app::ThinkingRow::from("```rust\nlet orbit = 1;\n```".to_owned());
    thinking.finished = true;
    let mut state = UiState::new("orbit-model");
    state
        .transcript
        .push(TranscriptItem::Thinking(thinking));
    state.expanded.insert(0);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| draw(frame, &mut state))
        .unwrap();
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
            state
                .max_scroll
                .saturating_sub(state.scroll),
            point,
        ),
        Some(0)
    );
    state.transcript_pointer = Some(point);
    terminal
        .draw(|frame| draw(frame, &mut state))
        .unwrap();
    for x in area.x..area.right() {
        assert_eq!(terminal.backend().buffer()[(x, y)], before[(x, y)]);
    }
}

#[test]
fn tool_rows_remain_compact_at_narrow_widths() {
    for width in [28, 70] {
        for status in [
            ToolStatus::Running,
            ToolStatus::Finished,
            ToolStatus::Failed,
        ] {
            let mut state = UiState::new("fixture");
            state
                .transcript
                .push(TranscriptItem::Tool(super::super::app::ToolRow {
                    id: "compact".into(),
                    name: "ReadFile".into(),
                    purpose: Some("Inspect generated source ".repeat(30)),
                    arguments: "{}".into(),
                    output: "RESULT_MARKER".into(),
                    status,
                    changes: vec![],
                }));
            state.expanded_tool_groups.insert(0);
            let screen = render(&mut state, width, 30);
            assert_eq!(screen.matches("ReadFile ·").count(), 1);
            assert!(!screen.contains("RESULT_MARKER"));
            assert_eq!(state.transcript_layout.height(), 3);
            super::super::toggle_transcript_item(&mut state, 0);
            assert!(
                state
                    .info_dialog
                    .as_ref()
                    .unwrap()
                    .content
                    .contains("RESULT_MARKER")
            );
        }
    }
}

#[test]
fn collapsed_multiline_command_titles_fill_the_row_instead_of_stopping_at_quotes() {
    use super::super::app::ToolRow;
    let command = format!(
        "python3 -c \"\n    import pathlib\n    target = '{}'\n    print(target)\n\"",
        "中文 e\u{301} long_value_".repeat(40)
    );
    let arguments = serde_json::json!({
        "command": command
    })
    .to_string();
    let row = ToolRow {
        changes: Vec::new(),
        purpose: None,
        id: "multiline".into(),
        name: "Exec".into(),
        arguments: arguments.clone(),
        output: String::new(),
        status: ToolStatus::Finished,
    };
    for width in [48, 240] {
        let standalone = super::tool_cards::tool_title_with_links(&row, false, width);
        assert_eq!(standalone.len(), 1);
        {
            let header = &standalone[0];
            let text = header.text();
            assert!(text.contains("import"), "{text}");
            assert!(!text.contains("\\n") && !text.contains('\n'));
            assert!(text.contains('…'));
            assert!(
                (usize::from(width) - 1..=usize::from(width)).contains(&header.line.width()),
                "{text}"
            );
        }
    }
    assert_eq!(row.arguments, arguments);
}

#[test]
fn command_preview_skips_a_shell_safety_preamble() {
    let summary = tool_summary(
        &serde_json::json!({
            "command": "set -e\ncargo test --workspace --all-features"
        })
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
        } else {
            state
                .transcript
                .push(TranscriptItem::User("Generated layout fixture".into()));
        }

        assert_eq!(state.is_home(), home);

        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        terminal
            .draw(|frame| draw(frame, &mut state))
            .unwrap();
        let input = state.input_area;
        let viewport = state.selection.viewport;
        let scroll = state.max_scroll;
        state.input.set("/".into());

        terminal
            .draw(|frame| draw(frame, &mut state))
            .unwrap();

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

    assert!(first_page.contains("/btw"));
    assert!(!first_page.contains("/tasks"));

    assert!(!first_page.contains("Tab 补全"));

    state.menu_index = 6;
    let middle_page = render(&mut state, 100, 20);

    assert!(middle_page.contains("  /compact"));
    assert!(middle_page.contains("/btw"));
    assert_eq!(state.menu_offset, 0);

    state.menu_index = state.menu().len() - 1;
    let last_page = render(&mut state, 100, 20);

    assert!(last_page.contains("/wsl"));
    assert!(!last_page.contains("/btw"));
    assert!(!last_page.contains("Esc 关闭"));
}

fn render(state: &mut UiState, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| draw(frame, state))
        .unwrap();

    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn opening_details_does_not_change_reasoning_or_composer() {
    use orbit_client::protocol::{
        events::AgentEvent,
        model::{StreamEvent, ToolCall},
    };
    let mut state = UiState::new("fixture");
    state
        .input
        .set("unfinished draft".into());
    state.apply(AgentEvent::Model(StreamEvent::ReasoningDelta(
        "generated reasoning".into(),
    )));
    let call = ToolCall::function("call", "ReadFile", serde_json::json!({"path": "sample.rs"}));
    state.apply(AgentEvent::ToolStarted { call: call.clone() });
    state.apply(AgentEvent::ToolFinished {
        outcome: None,
        call,
        content: "generated output".into(),
        changes: vec![],
        files: None,
    });
    super::super::toggle_transcript_item(&mut state, 0);
    assert!(render(&mut state, 100, 30).contains("generated reasoning"));
    super::super::toggle_transcript_item(&mut state, 1);
    assert!(
        state
            .info_dialog
            .as_ref()
            .unwrap()
            .content
            .contains("generated output")
    );
    assert!(state.expanded.contains(&0));
    assert_eq!(state.input.text, "unfinished draft");
}

#[test]
fn clicking_a_tool_row_opens_that_call_instead_of_the_last_one() {
    let mut state = UiState::new("fixture");
    for index in 0..2 {
        state
            .transcript
            .push(TranscriptItem::Tool(super::super::app::ToolRow {
                id: format!("call-{index}"),
                name: "SaveMemory".into(),
                purpose: None,
                arguments: "{}".into(),
                output: format!("generated result {index}"),
                status: ToolStatus::Finished,
                changes: vec![],
            }));
    }
    state.expanded_tool_groups.insert(0);
    render(&mut state, 100, 40);
    let area = state.selection.viewport;
    let offset = state
        .max_scroll
        .saturating_sub(state.scroll);
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
        .unwrap();
    assert!(super::super::toggle_transcript_at(&mut state, point));
    assert!(
        state
            .info_dialog
            .as_ref()
            .unwrap()
            .content
            .contains("generated result 0")
    );
}

#[test]
fn tool_inspectors_show_returned_content_and_keep_the_draft() {
    use super::super::app::ToolRow;

    let cases = [
        (
            "ReadFile",
            serde_json::json!({
                "start_line": 1,
                "end_line": 1,
                "content": format!("1: {}", "中文".repeat(80))
            })
            .to_string(),
            vec!["中文".repeat(80)],
        ),
        (
            "Grep",
            serde_json::json!({
                "matches": [{
                    "path": "src/main.rs",
                    "line": 2,
                    "text": "hit",
                    "context": [{
                        "line": 1,
                        "text": "context"
                    }]
                }]
            })
            .to_string(),
            vec!["src/main.rs".into(), "context".into(), "hit".into()],
        ),
        (
            "Exec",
            serde_json::json!({
                "stdout": "first\nsecond\nthird\n",
                "stderr": "",
                "exit_code": 0,
                "elapsed": {"secs": 0, "nanos": 0},
                "timed_out": false,
                "cancelled": false,
                "output_truncated": false,
                "stdout_total_bytes": 19,
                "stderr_total_bytes": 0
            })
            .to_string(),
            vec!["first\nsecond\nthird\n".into()],
        ),
        (
            "MCP_Example_Echo",
            "result ".repeat(80),
            vec!["result ".repeat(80)],
        ),
        (
            "FailingTool",
            "error detail\nmore\nlast".into(),
            vec!["error detail\nmore\nlast".into()],
        ),
    ];

    for (name, output, expected) in cases {
        let mut state = UiState::new("model");
        state
            .input
            .set("unfinished draft".into());
        state
            .transcript
            .push(TranscriptItem::Tool(ToolRow {
                changes: Vec::new(),
                purpose: None,
                id: "first".into(),
                name: name.into(),
                arguments: "{}".into(),
                status: if name == "FailingTool" {
                    ToolStatus::Failed
                } else {
                    ToolStatus::Finished
                },
                output: output.clone(),
            }));

        state.expanded_tool_groups.insert(0);
        render(&mut state, 90, 32);
        super::super::toggle_transcript_item(&mut state, 0);
        let dialog = state
            .info_dialog
            .as_ref()
            .expect("tool inspector opened");
        for text in expected {
            assert!(dialog.content.contains(&text), "{name}: {text}");
        }
        assert_eq!(state.input.text, "unfinished draft");
        let TranscriptItem::Tool(row) = &state.transcript[0] else {
            unreachable!()
        };
        assert_eq!(row.output, output);
    }
}

#[test]
fn tool_modal_scroll_is_independent_of_transcript_and_draft() {
    use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
    let mut state = UiState::new("fixture");
    state.input.set("retained draft".into());
    state
        .transcript
        .push(TranscriptItem::Tool(super::super::app::ToolRow {
            id: "scroll".into(),
            name: "external_tool".into(),
            purpose: None,
            arguments: "{}".into(),
            output: (0..200)
                .map(|i| format!("generated row {i}\n"))
                .collect(),
            status: ToolStatus::Finished,
            changes: vec![],
        }));
    state
        .transcript
        .push(TranscriptItem::Assistant("answer\n".repeat(100)));
    render(&mut state, 90, 24);
    let scroll = state.scroll;
    super::super::keyboard_action(
        &mut state,
        Event::Key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::ALT)),
        false,
    );
    render(&mut state, 90, 24);
    super::super::dialogs::handle(
        &mut state,
        Event::Key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)),
    );
    assert!(render(&mut state, 90, 24).contains("generated row 199"));
    super::super::dialogs::handle(
        &mut state,
        Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
    );
    assert!(state.info_dialog.is_none());
    assert_eq!(state.input.text, "retained draft");
    assert_eq!(state.scroll, scroll);
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

    assert!(render(&mut state, 100, 30).contains("/btw"));

    for (width, height) in [(20, 5), (10, 3), (1, 1)] {
        assert!(!render(&mut state, width, height).contains("请放大终端"));
    }
}

#[test]
fn permission_mode_changes_only_repaint_the_mode_field() {
    use crate::tui::permissions::Mode;

    for width in [24, 40, 70, 120] {
        let mut state = UiState::new("provider::test-model");
        state.workspace = "/project".into();
        state.input.set("未发送的草稿".into());
        let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
        terminal
            .draw(|frame| draw(frame, &mut state))
            .unwrap();
        let baseline = terminal.backend().buffer().clone();
        let start = baseline
            .content
            .windows(4)
            .position(|cells| {
                cells
                    .iter()
                    .map(|cell| cell.symbol())
                    .eq(["a", "u", "t", "o"])
            })
            .expect("permission mode is visible");
        let (x, y) = (start % width as usize, start / width as usize);

        for confirmed in [Mode::Ask, Mode::Auto, Mode::Yolo] {
            state.permissions.apply(confirmed, 1);
            for pending in [Some(Mode::Ask), Some(Mode::Auto), Some(Mode::Yolo), None] {
                state.permissions.pending = pending;
                terminal
                    .draw(|frame| draw(frame, &mut state))
                    .unwrap();
                for (index, (before, after)) in baseline
                    .content
                    .iter()
                    .zip(&terminal.backend().buffer().content)
                    .enumerate()
                {
                    if before != after {
                        assert!(
                            index / width as usize == y
                                && (x..x + 5).contains(&(index % width as usize)),
                            "mode change moved or repainted another field: width={width}, \
                                confirmed={confirmed:?}, pending={pending:?}, cell={index}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn environment_labels_never_appear_in_composer_details_or_workspace_footer() {
    for label in ["本地", "SSH 204", "WSL Ubuntu"] {
        let mut state = UiState::new("provider::model");
        state.connection_label = Some(label.into());
        state.workspace = "/project".into();
        for width in [16, 40, 120] {
            let details = composer_details(&state, width)
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("\n");
            assert!(!details.contains(label));
            let footer = footer(&state, width).to_string();
            assert!(!footer.contains(label));
            assert!(footer.contains("project"));
        }
        assert!(
            composer_details(&state, 120)[0]
                .to_string()
                .contains("model")
        );
        assert!(
            footer(&state, 120)
                .to_string()
                .contains("/project")
        );
    }
}

#[test]
fn connected_workspaces_keep_the_home_screen_until_a_conversation_starts() {
    for label in ["本地", "SSH 204", "WSL Ubuntu"] {
        let mut state = UiState::new("mock-model");
        state.connection_label = Some(label.into());
        state.workspace = "/project".into();
        state.tabs.push(SessionTab {
            id: "workspace:home".into(),
            title: label.into(),
            status: RunStatus::Idle,
            elapsed: std::time::Duration::ZERO,
            waiting_approval: false,
            active: true,
        });

        let screen = render(&mut state, 140, 40);
        assert!(state.is_home(), "{label}");
        assert!(screen.contains("████"), "{label}");
        assert!(screen.contains("mock-model"));
        assert!(state.input_area.x > 15);
        assert!(state.input_area.y < 30);
        assert_eq!(state.selection.viewport.height, 0);

        state.input.insert("/");
        assert!(render(&mut state, 140, 40).contains("/btw"));
        state.input.set(String::new());
        state.show_output("命令", "首页命令提示".into());
        assert!(state.info_dialog.is_none());
        assert!(state.transcript.is_empty());
        state.info_dialog = None;

        state.submit("hello Orbit".into());
        let screen = render(&mut state, 140, 40);
        assert!(!state.is_home());
        assert!(!screen.contains("████"));
        assert!(screen.contains("hello Orbit"));
        assert!(state.input_area.y >= 30);
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
    state.elapsed = std::time::Duration::from_millis(7_500);
    let completed = render(&mut state, 100, 24);
    assert!(!completed.contains("7.5s"));
}

#[test]
fn pending_images_render_as_chips_above_the_prompt() {
    let mut state = UiState::new("mock");
    state
        .attachments
        .push(orbit_client::protocol::model::Attachment::Image {
            lease: None,
            name: "capture.png".into(),
            mime_type: "image/png".into(),
            path: "/tmp/unused.png".into(),
        });
    state
        .input
        .insert("describe this image");
    let screen = render(&mut state, 80, 24);

    assert!(!screen.contains("capture.png"));
    assert!(screen.contains("describe this image"));
    assert!(state.input_area.y > 0);
    assert_eq!(state.attachment_areas.len(), 1);

    state.attachment_cursor = Some(0);
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| draw(frame, &mut state))
        .unwrap();

    assert!(!terminal.backend().cursor_visible());

    let chip = state.attachment_areas[0];

    assert!(!screen.contains("图片 1"));
    assert!((chip.y..chip.bottom()).any(|y| {
        (chip.x..chip.right())
            .any(|x| terminal.backend().buffer()[(x, y)].symbol() == icons::IMAGE_OUTLINE)
    }));
    assert_eq!(terminal.backend().buffer()[(chip.x, chip.y)].bg, ACCENT);
}

#[test]
fn details_hover_fills_spaces_and_wide_glyphs_without_underlining_the_caption() {
    let mut state = UiState::new("mock");
    state
        .transcript
        .push(TranscriptItem::Assistant(
            "<details><summary>点击展开 / 折叠</summary>正文</details>".into(),
        ));
    let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
    terminal
        .draw(|f| draw(f, &mut state))
        .unwrap();
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
                assert_eq!(buffer[(x, point.1)].bg, THEME.accent);
                assert_eq!(buffer[(x, point.1)].fg, THEME.on_accent);
                assert!(
                    !buffer[(x, point.1)]
                        .modifier
                        .contains(Modifier::UNDERLINED)
                );
            }

            assert_eq!(buffer[(first - 1, point.1)].bg, Color::Reset);
        })
        .unwrap();
    state.transcript_pointer = None;
    terminal
        .draw(|f| draw(f, &mut state))
        .unwrap();
    assert_eq!(
        terminal.backend().buffer()[(first, point.1)].bg,
        Color::Reset
    );
}

#[test]
fn details_code_block_retains_one_border_color_including_its_closing_corner() {
    let mut state = UiState::new("mock");
    state
        .transcript
        .push(TranscriptItem::Assistant(
            "<details open><summary>展开</summary>\n\n```json\
        \n{\"status\": \"success\"}\n```\n</details>"
                .into(),
        ));
    let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
    terminal
        .draw(|frame| draw(frame, &mut state))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let mut found = Vec::new();
    for y in 0..30 {
        for x in 0..80 {
            if matches!(buffer[(x, y)].symbol(), "╭" | "│" | "╰") {
                found.push((buffer[(x, y)].symbol().to_owned(), buffer[(x, y)].fg));
            }
        }
    }

    assert_eq!(found.len(), 3, "{found:?}");
    assert!(
        found
            .iter()
            .all(|(_, color)| *color == super::super::theme::code_label_color("json")),
        "{found:?}"
    );
    assert!(
        buffer
            .content
            .iter()
            .any(|cell| cell.symbol() == icons::CHEVRON_DOWN)
    );
}

#[test]
fn footnote_click_jumps_within_its_message_without_a_hover_tooltip() {
    let mut state = UiState::new("mock");
    let body = format!(
        "引用[^named]。\n\n{}\n[^named]: 正确脚注需要在窗口缩窄后仍然完整高亮，多行说明继续保持相同的脚注落点。\n\n    第二段补充说明\n",
        "正文\n".repeat(30)
    );
    state
        .transcript
        .push(TranscriptItem::Assistant(body));
    state
        .transcript
        .push(TranscriptItem::Assistant(
            "第二条[^named]。\n\n[^named]: 另一条脚注\n".into(),
        ));
    state.follow_bottom = false;
    state.scroll = usize::MAX;
    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
    terminal
        .draw(|frame| draw(frame, &mut state))
        .unwrap();
    let area = state.selection.viewport;
    let point = (area.y..area.bottom())
        .find_map(|y| {
            (area.x..area.right()).find_map(|x| {
                state
                    .transcript_layout
                    .link_at(area, 0, (x, y))
                    .filter(|url| super::super::markdown::footnotes::is_target(url))
                    .map(|_| (x, y))
            })
        })
        .unwrap();

    state.link_hovered = Some(point);
    terminal
        .draw(|frame| draw(frame, &mut state))
        .unwrap();
    assert!(state.link_tooltip_area.is_empty());
    let target = state
        .transcript_layout
        .footnote_at(area, 0, point)
        .unwrap();
    assert!(super::super::jump_footnote_at(&mut state, point));
    assert_eq!(
        state.max_scroll - state.scroll,
        target.min(state.max_scroll)
    );
    assert!(!state.follow_bottom);
    terminal
        .draw(|frame| draw(frame, &mut state))
        .unwrap();
    let visible = (area.y..area.bottom())
        .map(|y| {
            (area.x..area.right())
                .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<String>();

    assert!(
        visible
            .chars()
            .filter(|ch| !ch.is_whitespace())
            .collect::<String>()
            .contains("正确脚注"),
        "{visible}"
    );
    assert!(!visible.contains("无法打开链接"));
    for width in [40, 80] {
        terminal.backend_mut().resize(width, 20);
        terminal
            .resize(Rect::new(0, 0, width, 20))
            .unwrap();
        terminal
            .draw(|frame| draw(frame, &mut state))
            .unwrap();
        let highlighted = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .filter(|cell| cell.bg == THEME.md_mark)
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(highlighted.contains("正确脚注"), "{highlighted}");
        assert!(highlighted.contains("第二段补充说明"), "{highlighted}");
        assert!(!highlighted.contains("另一条脚注"), "{highlighted}");
    }
    state
        .footnote_focus
        .as_mut()
        .unwrap()
        .since = std::time::Instant::now() - std::time::Duration::from_secs(3);
    assert!(state.expire_footnote_focus());
    terminal
        .draw(|frame| draw(frame, &mut state))
        .unwrap();
    assert!(
        terminal
            .backend()
            .buffer()
            .content
            .iter()
            .all(|cell| cell.bg != THEME.md_mark)
    );
}

#[test]
fn file_approval_keeps_conversation_and_draft_visible() {
    let mut state = UiState::new("mock");
    state
        .transcript
        .push(TranscriptItem::Assistant(
            "Conversation remains visible".into(),
        ));
    state.input.insert("my draft");
    state.approval = Some(orbit_client::protocol::approval::ApprovalPreview::Write(
        orbit_client::protocol::approval::WritePreview {
            path: "price.c".into(),
            before: Some("return subtotal > 10000;\n".into()),
            after: "return subtotal >= 10000;\n".into(),
        },
    ));
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|frame| draw(frame, &mut state))
        .unwrap();
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
        terminal
            .backend_mut()
            .assert_cursor_position(caret);
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
        terminal
            .draw(|frame| draw(frame, &mut state))
            .unwrap();
        let position = state.ime_cursor.unwrap();

        assert_eq!(position.x, column);

        terminal
            .backend_mut()
            .assert_cursor_position(position);

        assert!(terminal.backend().cursor_visible());
        assert_eq!(
            terminal.backend().buffer()[(position.x, position.y)].bg,
            Color::Reset
        );

        // Streaming output must not leave the IME attached to the transcript.
        state.apply(orbit_client::protocol::events::AgentEvent::Model(
            orbit_client::protocol::model::StreamEvent::TextDelta("输出中".into()),
        ));
        terminal
            .draw(|frame| draw(frame, &mut state))
            .unwrap();
        terminal
            .backend_mut()
            .assert_cursor_position(position);
    }

    state.picker = Some(crate::tui::dialogs::SessionPicker::new(Vec::new()));
    crate::tui::dialogs::handle(
        &mut state,
        crossterm::event::Event::Paste("中文".repeat(80)),
    );
    terminal
        .draw(|frame| draw(frame, &mut state))
        .unwrap();
    let position = state.ime_cursor.unwrap();
    terminal
        .backend_mut()
        .assert_cursor_position(position);

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
        terminal
            .draw(|frame| draw(frame, &mut state))
            .unwrap();
        let caret = state.ime_cursor.unwrap();
        assert!(terminal.backend().cursor_visible());
        terminal
            .backend_mut()
            .assert_cursor_position(caret);
        assert_eq!(
            terminal.backend().buffer()[(caret.x, caret.y)].symbol(),
            " "
        );
        assert_eq!(
            terminal.backend().buffer()[(caret.x + 1, caret.y)].bg,
            Color::Reset
        );
    }

    state
        .input
        .set(format!("{}\nnext", "a".repeat(74)));
    state.input.set_cursor(74);
    terminal
        .draw(|frame| draw(frame, &mut state))
        .unwrap();
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

    assert!(
        state
            .cursor_motion
            .active(std::time::Instant::now())
    );
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
    state
        .input
        .extend_selection("你好👩‍💻".len());
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
            assert!(!text.contains("auto"));
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
fn usage_footer_shows_two_meters_on_home_and_chat_without_hover_details() {
    use crossterm::event::{Event, KeyModifiers, MouseEvent, MouseEventKind};
    use orbit_protocol::usage::{Account, Limit, Snapshot};
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    for chat in [false, true] {
        let mut state = UiState::new("model");
        state.workspace = "/long/project/path".into();
        if chat {
            state
                .transcript
                .push(TranscriptItem::User("existing message".into()));
        }
        state.input.set("unsent draft".into());
        state.usage.receive(Snapshot {
            model: "gemini-3.8-flash".into(),
            account: Account {
                provider: "Antigravity".into(),
                name: "main".into(),
                limits: vec![
                    Limit {
                        label: "Gemini Models · Weekly Limit Remaining · weekly".into(),
                        remaining_fraction: Some(0.59),
                        resets_at: Some((now + 6 * 86_400 + 17 * 3_600 + 59).to_string()),
                        ..Default::default()
                    },
                    Limit {
                        label: "Gemini Models · Five Hour Limit Remaining · 5h".into(),
                        remaining_fraction: Some(0.96),
                        resets_at: Some((now + 4 * 3_600 + 12 * 60 + 59).to_string()),
                        ..Default::default()
                    },
                    Limit {
                        label: "Claude and GPT models · weekly".into(),
                        remaining_fraction: Some(1.0),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            ..Default::default()
        });
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        terminal
            .draw(|frame| draw_frame(frame, &mut state))
            .unwrap();
        let anchor = state.usage_indicator_area;
        assert!(!anchor.is_empty());
        assert!(anchor.right() < state.context_indicator_area.x);
        let buffer = terminal.backend().buffer().clone();
        let meter = (anchor.x..anchor.right())
            .map(|x| buffer[(x, anchor.y)].symbol())
            .collect::<String>();
        assert!(meter.contains("59%") && meter.contains("96%"), "{meter}");
        assert_eq!(meter.matches('%').count(), 2);
        assert_eq!(meter.matches('(').count(), 2);
        assert!(meter.contains("周 ") && meter.contains("5h "));
        assert!(!meter.contains("后重置"));
        assert!(!meter.contains("Antigravity"));
        assert!(
            buffer
                .content
                .iter()
                .any(|cell| matches!(cell.fg, Color::Rgb(..)) && !cell.symbol().trim().is_empty())
        );
        assert_eq!(
            handle_context_hover(
                &mut state,
                &Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Moved,
                    column: anchor.x,
                    row: anchor.y,
                    modifiers: KeyModifiers::NONE,
                })
            ),
            Some(false)
        );
        terminal
            .draw(|frame| draw_frame(frame, &mut state))
            .unwrap();
        assert_eq!(*terminal.backend().buffer(), buffer);
        assert!(state.info_dialog.is_none());
        for x in anchor.x..state.context_indicator_area.x {
            assert!(
                !buffer[(x, anchor.y)]
                    .modifier
                    .contains(Modifier::UNDERLINED)
            );
        }
        for width in [12, 30, 60] {
            assert!(footer(&state, width).width() <= width);
            render(&mut state, width as u16, 12);
            if width >= 30 {
                assert!(!state.usage_indicator_area.is_empty());
            }
        }
        assert_eq!(state.input.text, "unsent draft");
        assert_eq!(state.transcript.len(), usize::from(chat));
    }
}

#[test]
fn context_indicator_reveals_its_budget_on_hover() {
    use crossterm::event::{Event, KeyModifiers, MouseEvent, MouseEventKind};

    let mut state = UiState::new("model");
    state.context_used_tokens = 51_200;
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|frame| draw(frame, &mut state))
        .unwrap();
    let indicator = state.context_indicator_area;

    assert_eq!(indicator.width, 7);

    let hover = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Moved,
        column: indicator.x,
        row: indicator.y,
        modifiers: KeyModifiers::NONE,
    });

    assert_eq!(handle_context_hover(&mut state, &hover), Some(true));

    terminal
        .draw(|frame| draw(frame, &mut state))
        .unwrap();
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

        assert!(
            footer(&state, 120)
                .to_string()
                .contains(expected)
        );
    }
}

#[test]
fn link_hover_shows_and_clears_its_ctrl_click_hint() {
    use crossterm::event::{Event, KeyModifiers, MouseEvent, MouseEventKind};

    let mut state = UiState::new("model");
    state
        .transcript
        .push(TranscriptItem::Assistant(
            "打开 https://example.com/docs 查看文档。".into(),
        ));
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| draw(frame, &mut state))
        .unwrap();
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

    terminal
        .draw(|frame| draw(frame, &mut state))
        .unwrap();
    let shown: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();

    assert!(
        shown
            .replace(' ', "")
            .contains("Ctrl+Click跳转"),
        "{shown}"
    );

    let leave = Event::Mouse(MouseEvent {
        kind: MouseEventKind::Moved,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    });

    assert_eq!(handle_link_hover(&mut state, &leave), Some(true));

    terminal
        .draw(|frame| draw(frame, &mut state))
        .unwrap();
    let hidden: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();

    assert!(
        !hidden
            .replace(' ', "")
            .contains("Ctrl+Click跳转"),
        "{hidden}"
    );
}

#[test]
fn table_link_hint_is_anchored_near_the_label_and_restores_covered_content() {
    use crossterm::event::{Event, KeyModifiers, MouseEvent, MouseEventKind};
    use ratatui::buffer::CellWidth;

    let url = "https://orbit.example/docs";
    let source = format!(
        "## 五、引用\n\n> 第一层引用。\n>\n> 引用块内的行内代码：`code` 与 **粗体**。\n\n## 六、表格\n\n| 左对齐 | 居中 | 右对齐 |\n| \
            :--- | :---: | ---: |\n| 左对齐文本 | 居中文字 | 123456 |\n| 单元格内**粗体** | 单元格内*斜体* | 0.42 |\n| \
            `行内代码` | [链接👩‍💻]({url}) | $e^{{i\\pi}}$ |\n\n## 七、代码块\n\n带语言标注的代码块：\n\n```python\n# \
            斐波那契数列\ndef fib(n: int) -> int:\n    a, b = 0, 1\n    for _ in range(n):\n        a, \
            b = b, a + b\n    return a\n```\n"
    );
    let mut state = UiState::new("mock");
    state
        .transcript
        .push(TranscriptItem::Assistant(source));

    for width in [44, 80, 140] {
        let mut terminal = Terminal::new(TestBackend::new(width, 60)).unwrap();
        terminal
            .draw(|frame| draw(frame, &mut state))
            .unwrap();
        let original = terminal.backend().buffer().clone();
        let viewport = state.selection.viewport;
        let points: Vec<_> = (viewport.y..viewport.bottom())
            .flat_map(|y| (viewport.x..viewport.right()).map(move |x| (x, y)))
            .filter(|&point| {
                state
                    .transcript_layout
                    .link_at(viewport, state.max_scroll, point)
                    == Some(url)
            })
            .collect();

        assert!(
            points.len() >= 6,
            "Chinese and emoji link did not render: {width}"
        );
        let mut tooltip = None;
        for (index, &(x, y)) in points.iter().enumerate() {
            let hover = Event::Mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                column: x,
                row: y,
                modifiers: KeyModifiers::NONE,
            });

            assert_eq!(handle_link_hover(&mut state, &hover), Some(index == 0));
            terminal
                .draw(|frame| draw(frame, &mut state))
                .unwrap();
            let hint = state.link_tooltip_area;
            assert!(!hint.is_empty());
            assert_eq!(hint.y, y + 1, "tooltip drifted away from table link");
            if let Some(previous) = tooltip {
                assert_eq!(hint, previous);
            }
            tooltip = Some(hint);
            let mut column = hint.x;
            while column < hint.right() {
                let cell = &terminal.backend().buffer()[(column, hint.y)];
                assert_eq!(cell.fg, THEME.overlay_foreground);
                assert_eq!(cell.bg, THEME.overlay_background);
                column += cell.cell_width().max(1);
            }
        }

        assert!(tooltip.unwrap().y < viewport.bottom() - 3);
        handle_link_hover(
            &mut state,
            &Event::Mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                column: 0,
                row: 0,
                modifiers: KeyModifiers::NONE,
            }),
        );
        terminal
            .draw(|frame| draw(frame, &mut state))
            .unwrap();
        assert!(state.link_tooltip_area.is_empty());
        for (restored, original) in terminal
            .backend()
            .buffer()
            .content
            .iter()
            .zip(&original.content)
        {
            assert_eq!(
                (restored.symbol(), restored.style()),
                (original.symbol(), original.style())
            );
        }
    }
}

#[test]
fn wrapped_text_link_hints_stay_fixed_for_each_occurrence() {
    use crossterm::event::{Event, KeyModifiers, MouseEvent, MouseEventKind};

    let url = "https://orbit.example/docs";
    let title = "中文👩‍💻abcdef".repeat(16);
    for width in [30, 64, 140] {
        let mut state = UiState::new("mock");
        state
            .transcript
            .push(TranscriptItem::Assistant(format!(
                "开始 [{title}]({url}) 结束\n\n分隔正文\n\n第二个 [{title}]({url})。"
            )));
        let mut terminal = Terminal::new(TestBackend::new(width, 120)).unwrap();
        terminal
            .draw(|frame| draw(frame, &mut state))
            .unwrap();
        let viewport = state.selection.viewport;
        let points: Vec<_> = (viewport.y..viewport.bottom())
            .flat_map(|y| (viewport.x..viewport.right()).map(move |x| (x, y)))
            .filter_map(|point| {
                let hit = state
                    .transcript_layout
                    .link_hit(viewport, state.max_scroll, point)?;
                (hit.url == url).then_some((point, hit.area))
            })
            .collect();
        let mut subjects = Vec::new();
        for &(_, area) in &points {
            if !subjects.contains(&area) {
                subjects.push(area);
            }
        }

        assert_eq!(subjects.len(), 2, "width {width}");
        assert!(
            subjects
                .iter()
                .all(|area| area.height > 1),
            "width {width}: {subjects:?}"
        );

        for subject in subjects {
            let points: Vec<_> = points
                .iter()
                .filter(|(_, area)| *area == subject)
                .map(|(point, _)| *point)
                .collect();
            let mut tooltip = None;
            for (index, &(x, y)) in [points[0], points[points.len() / 2], *points.last().unwrap()]
                .iter()
                .enumerate()
            {
                let hover = Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Moved,
                    column: x,
                    row: y,
                    modifiers: KeyModifiers::NONE,
                });

                assert_eq!(handle_link_hover(&mut state, &hover), Some(index == 0));
                terminal
                    .draw(|frame| draw(frame, &mut state))
                    .unwrap();
                let hint = state.link_tooltip_area;
                assert!(!hint.is_empty());
                assert_eq!(hint.y, subject.bottom());
                assert!(hint.intersection(subject).is_empty());
                if let Some(previous) = tooltip {
                    assert_eq!(hint, previous);
                }
                tooltip = Some(hint);
            }
        }
    }
}

#[test]
fn repository_url_in_a_table_has_hover_feedback_after_wrapping_and_resize() {
    use crossterm::event::{Event, KeyModifiers, MouseEvent, MouseEventKind};
    use ratatui::buffer::CellWidth;
    let url = "https://github.com/example-org/sample-app";
    let mut state = UiState::new("model");
    state.transcript.push(TranscriptItem::Assistant(format!(
        "Orbit 示例项目\n\n| 项 | 值 |\n| --- | --- |\n| 项目 | Orbit |\n| 仓库 | {url} |\n| 提交署名 | Orbit \
            测试身份（虚构） |\n| Windows 用户名 | orbit（示例路径 C:\\Users\\orbit\\） |\n| 会话 ID | \
            orbit-test-session |\n| 用途 | TUI 链接测试 |\n"
    )));
    for width in [100, 64, 30] {
        let mut terminal = Terminal::new(TestBackend::new(width, 80)).unwrap();
        terminal
            .draw(|frame| draw(frame, &mut state))
            .unwrap();
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
            terminal
                .draw(|frame| draw(frame, &mut state))
                .unwrap();
            let hint = state.link_tooltip_area;
            assert!(hint.width > 0, "width {width}, point ({x}, {y})");
            let mut text = String::new();
            let mut column = hint.x;
            while column < hint.right() {
                let cell = &terminal.backend().buffer()[(column, hint.y)];
                text.push_str(cell.symbol());
                column += cell.cell_width().max(1);
            }

            assert!(
                text.replace(' ', "")
                    .contains("Ctrl+Click跳转"),
                "{text}"
            );
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
    assert!(line.spans.iter().any(|span| {
        span.content == "重点"
            && span
                .style
                .add_modifier
                .contains(Modifier::BOLD)
    }));
    assert!(
        line.spans
            .iter()
            .any(|span| span.content == "code"
                && span.style.fg == Some(desaturate_for_thinking(Some(THEME.md_inline_code))))
    );
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
    use orbit_client::protocol::events::AgentEvent;

    use orbit_client::protocol::model::StreamEvent;

    let mut state = UiState::new("mock");
    state.apply(AgentEvent::Model(StreamEvent::TextDelta(
        (0..200)
            .map(|i| format!("line {i:03}\n"))
            .collect(),
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
    state
        .transcript
        .push(TranscriptItem::Assistant(format!(
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
    state
        .transcript
        .push(TranscriptItem::Assistant(
            (0..80)
                .map(|index| format!("段落 {index:02} {}\n", "中文内容 words ".repeat(8)))
                .collect(),
        ));
    render(&mut state, 90, 18);
    state.scroll_up(70);
    render(&mut state, 90, 18);
    let old_offset = state.max_scroll - state.scroll;
    let anchor = state
        .transcript_layout
        .anchor_at(old_offset)
        .unwrap();

    render(&mut state, 48, 18);
    let expected = state
        .transcript_layout
        .offset_for_anchor(anchor)
        .unwrap()
        .min(state.max_scroll);

    assert_eq!(state.max_scroll - state.scroll, expected);
    assert!(!state.follow_bottom);
}

#[test]
fn activity_stays_readable_and_in_place_while_colors_animate() {
    let mut state = UiState::new("mock");
    state.submit("work".into());
    state.started_at = None;
    let first = Line::from(activity_spans(&state, 100));
    assert!(
        first
            .to_string()
            .contains("等待模型响应")
    );
    state.elapsed = std::time::Duration::from_millis(800);
    let second = Line::from(activity_spans(&state, 100));
    assert_eq!(first.to_string(), second.to_string());
    assert_ne!(first.spans, second.spans);
    state.apply(orbit_client::protocol::events::AgentEvent::CompactionStarted);
    assert!(
        render(&mut state, 100, 24)
            .replace(' ', "")
            .contains("正在压缩上下文")
    );
    for width in [12, 24, 40, 80] {
        assert!(Line::from(activity_spans(&state, width)).width() <= width);
    }
    state.status = RunStatus::Cancelling;
    assert!(
        Line::from(activity_spans(&state, 100))
            .to_string()
            .contains("正在取消")
    );
    state.apply(orbit_client::protocol::events::AgentEvent::Cancelled);
    assert!(!render(&mut state, 100, 24).contains("正在取消"));
}

#[test]
fn activity_animates_only_in_footer_and_stops_when_completed() {
    use orbit_client::protocol::events::AgentEvent;
    use orbit_client::protocol::model::{FunctionCall, StreamEvent, ToolCall};

    let mut state = UiState::new("mock");
    state.submit("work".into());
    state.started_at = None;
    state.apply(AgentEvent::Model(StreamEvent::ReasoningDelta(
        "stable reasoning".into(),
    )));
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|frame| draw(frame, &mut state))
        .unwrap();
    let first = terminal.backend().buffer().clone();
    state.elapsed = std::time::Duration::from_millis(800);
    terminal
        .draw(|frame| draw(frame, &mut state))
        .unwrap();
    let second = terminal.backend().buffer();
    let footer_start = usize::from(state.progress_area.y) * 100;
    assert_eq!(
        first.content[..footer_start],
        second.content[..footer_start]
    );
    assert_ne!(
        first.content[footer_start..],
        second.content[footer_start..]
    );
    assert!(
        Line::from(activity_spans(&state, 100))
            .to_string()
            .contains("正在思考")
    );

    let call = ToolCall {
        id: "command".into(),
        kind: "function".into(),
        metadata: None,
        purpose: None,
        function: FunctionCall {
            name: "Exec".into(),
            arguments: r#"{"command":"echo stable"}"#.into(),
        },
    };
    state.apply(AgentEvent::ToolStarted { call: call.clone() });
    state.started_at = None;
    state.elapsed = std::time::Duration::ZERO;
    terminal
        .draw(|frame| draw(frame, &mut state))
        .unwrap();
    let first = terminal.backend().buffer().clone();
    state.elapsed = std::time::Duration::from_millis(800);
    terminal
        .draw(|frame| draw(frame, &mut state))
        .unwrap();
    let second = terminal.backend().buffer();
    let footer_start = usize::from(state.progress_area.y) * 100;
    assert_eq!(
        first.content[..footer_start],
        second.content[..footer_start]
    );
    assert_ne!(
        first.content[footer_start..],
        second.content[footer_start..]
    );
    assert!(
        Line::from(activity_spans(&state, 100))
            .to_string()
            .contains("正在调用工具")
    );
    state.apply(AgentEvent::ToolFinished {
        outcome: None,
        call,
        content: "stable output".into(),
        changes: vec![],
        files: None,
    });
    state.apply(AgentEvent::Completed {
        turns: 1,
        tool_calls: 1,
    });
    terminal
        .draw(|frame| draw(frame, &mut state))
        .unwrap();
    let first = terminal.backend().buffer().clone();
    state.elapsed += std::time::Duration::from_millis(800);
    terminal
        .draw(|frame| draw(frame, &mut state))
        .unwrap();
    assert_eq!(&first, terminal.backend().buffer());
}

#[test]
fn execution_footer_shows_stable_status_labels_for_commands_and_tasks() {
    use orbit_client::protocol::events::AgentEvent;

    for connection in [None, Some("SSH 204".into())] {
        let mut state = UiState::new("mock");
        state.connection_label = connection;
        state.submit("work".into());
        state.started_at = None;

        for (event, expected) in [
            (
                AgentEvent::WaitingForCommands { job_ids: vec![] },
                "等待命令完成",
            ),
            (
                AgentEvent::WaitingForTasks { job_ids: vec![] },
                "等待子任务完成",
            ),
        ] {
            state.apply(event);
            let mut frames = std::collections::HashSet::new();
            let mut hint_offset = None;
            for millis in (0..2_000).step_by(130) {
                state.elapsed = std::time::Duration::from_millis(millis);
                let line = Line::from(activity_spans(&state, 100)).to_string();
                assert!(line.contains(expected));
                assert!(!line.contains('█'));
                let offset = line[..line.find("Ctrl+C 取消").unwrap()].width();
                assert_eq!(*hint_offset.get_or_insert(offset), offset);
                frames.insert(line);
            }
            assert_eq!(frames.len(), 1);
            for width in [12, 24, 40, 80] {
                assert!(Line::from(activity_spans(&state, width)).width() <= width);
            }
        }

        state.status = RunStatus::Cancelling;
        assert!(
            Line::from(activity_spans(&state, 100))
                .to_string()
                .contains("正在取消")
        );
        assert!(
            !Line::from(activity_spans(&state, 100))
                .to_string()
                .contains('█')
        );
        state.apply(AgentEvent::Cancelled);
        assert!(
            !Line::from(activity_spans(&state, 100))
                .to_string()
                .contains('█')
        );
    }
}
