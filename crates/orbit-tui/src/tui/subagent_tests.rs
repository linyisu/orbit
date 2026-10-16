use super::app::TranscriptItem;
use super::*;
use crossterm::event::KeyEvent;

#[test]
fn nested_html_clicks_do_not_collapse_the_enclosing_tool_or_thinking_item() {
    let source = "<details><summary>内容</summary>\n[正文](https://orbit.example)\n</details>";
    let mut thought = app::ThinkingRow::from(source.to_owned());
    thought.finished = true;

    for item in [
        TranscriptItem::Assistant(source.into()),
        TranscriptItem::Thinking(thought),
    ] {
        let mut state = UiState::new("orbit::model");
        state.transcript.push(item);
        state.expanded.insert(0);
        state.selection.viewport = ratatui::layout::Rect::new(0, 0, 80, 80);
        state.transcript_layout.update_expanded(
            &state.transcript,
            &state.expanded,
            &state.expanded_tool_groups,
            80,
        );
        let point = (0..80)
            .find_map(|y| {
                (0..80).find_map(|x| {
                    state
                        .transcript_layout
                        .html_disclosure_at(state.selection.viewport, 0, (x, y))
                        .map(|_| (x, y))
                })
            })
            .unwrap();

        assert!(toggle_transcript_at(&mut state, point));
        assert!(state.expanded.contains(&0));
        let body_link = (0..80).any(|y| {
            (0..80).any(|x| {
                state
                    .transcript_layout
                    .link_at(state.selection.viewport, 0, (x, y))
                    == Some("https://orbit.example")
            })
        });

        assert!(body_link);
        assert!(toggle_transcript_at(&mut state, point));
        assert!(state.expanded.contains(&0));
        let body_link = (0..80).any(|y| {
            (0..80).any(|x| {
                state
                    .transcript_layout
                    .link_at(state.selection.viewport, 0, (x, y))
                    == Some("https://orbit.example")
            })
        });

        assert!(!body_link);
    }
}

#[test]
fn child_session_links_open_a_dialog_without_switching_tabs() {
    assert_eq!(
        subagent_link_id("orbit://session/00000000000000000000000000000001"),
        Some("00000000000000000000000000000001")
    );
    for target in [
        "https://orbit.example/",
        "orbit://session/../orbit.db",
        "orbit://session/00000000000000000000000000000001/extra",
    ] {
        assert!(subagent_link_id(target).is_none());
    }

    let mut state = UiState::new("orbit::model");
    let id = "00000000000000000000000000000001";
    state
        .transcript
        .push(TranscriptItem::User("inspect Orbit".into()));
    state
        .transcript
        .push(TranscriptItem::Tool(app::ToolRow {
            changes: Vec::new(),
            purpose: None,
            id: "child".into(),
            name: "SpawnAgent".into(),
            arguments: "{\"task\":\"inspect\"}".into(),
            output: serde_json::json!({
                "agent_id": id,
                "role": "explore",
                "model": "orbit::model",
                "status": "running"
            })
            .to_string(),
            status: app::ToolStatus::Running,
        }));
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &mut state))
        .unwrap();
    toggle_transcript_item(&mut state, 1);
    dialogs::handle(
        &mut state,
        Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
    );
    assert!(state.resume_request.is_none());
    assert!(state.subagent_dialog.is_some());
    assert!(state.expanded.is_empty());
}
