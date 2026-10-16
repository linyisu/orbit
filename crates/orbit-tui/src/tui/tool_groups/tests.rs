use super::super::{app::UiState, pointer, transcript::TranscriptLayout};
use orbit_client::protocol::{
    events::AgentEvent,
    model::{StreamEvent, ToolCall},
};
use ratatui::layout::Rect;
use serde_json::json;

fn call(id: &str) -> ToolCall {
    let mut call = ToolCall::function(id, "read_file", json!({"path": "generated.rs"}));
    call.purpose = Some(format!("Inspect generated section {id}"));
    call
}

#[test]
fn running_batches_reopen_and_background_completion_collapses_without_overriding_manual_choice() {
    let mut state = UiState::new("fixture");
    state.apply(AgentEvent::ToolStarted {
        call: call("first"),
    });
    assert!(state.expanded_tool_groups.contains(&0));
    state.apply(AgentEvent::ToolBackgrounded {
        call: call("first"),
        job_id: "generated-job".into(),
        content: "queued".into(),
    });
    assert!(!state.expanded_tool_groups.contains(&0));
    state.apply(AgentEvent::ToolStarted {
        call: call("second"),
    });
    assert!(state.expanded_tool_groups.contains(&0));
    state.manual_disclosures.insert(0);
    state.apply(AgentEvent::ToolFinished {
        call: call("second"),
        content: "done".into(),
        changes: vec![],
        files: None,
    });
    assert!(state.expanded_tool_groups.contains(&0));
}

#[test]
fn interleaved_sources_keep_click_identity_without_reordering_after_resize() {
    let mut state = UiState::new("fixture");
    state.apply(AgentEvent::ToolStarted {
        call: call("first"),
    });
    state.apply(AgentEvent::Model(StreamEvent::TextDelta(
        "generated progress".into(),
    )));
    state.apply(AgentEvent::Model(StreamEvent::ReasoningDelta(
        "generated reasoning".into(),
    )));
    state.apply(AgentEvent::ToolStarted {
        call: call("second"),
    });
    for width in [26, 80, 40] {
        let mut layout = TranscriptLayout::default();
        layout.update_expanded(&state.transcript, &state.expanded, &[0, 3].into(), width);
        let area = Rect::new(0, 0, width, layout.height() as u16);
        let hits = (0..area.height)
            .filter_map(|y| layout.disclosure_at(&state.transcript, area, 0, (3, y)))
            .collect::<std::collections::BTreeSet<_>>();
        assert!(hits.contains(&0) && hits.contains(&3), "{hits:?}");
        assert!(!hits.contains(&1));
    }
    pointer::toggle_transcript_item(&mut state, 3);
    assert!(
        state
            .info_dialog
            .as_ref()
            .unwrap()
            .content
            .contains("section second")
    );
    assert!(state.expanded.is_empty());
}

#[test]
fn reasoning_finishes_independently_and_manual_expansion_survives() {
    let mut state = UiState::new("fixture");
    state.apply(AgentEvent::Model(StreamEvent::ReasoningDelta(
        "generated thought".into(),
    )));
    assert!(state.expanded.contains(&0));
    state.apply(AgentEvent::ReasoningFinished { elapsed_ms: 3 });
    assert!(!state.expanded.contains(&0));
    state.expanded.insert(0);
    state.manual_disclosures.insert(0);
    state.apply(AgentEvent::ToolStarted {
        call: call("first"),
    });
    assert!(state.expanded.contains(&0));
}

#[test]
fn interleaved_batches_have_independent_running_and_manual_states() {
    let mut state = UiState::new("fixture");
    state.apply(AgentEvent::ToolStarted {
        call: call("alpha"),
    });
    state.apply(AgentEvent::Model(StreamEvent::ReasoningDelta(
        "generated stage beta".into(),
    )));
    state.apply(AgentEvent::ToolStarted { call: call("beta") });
    assert_eq!(state.expanded_tool_groups, [0, 2].into());
    state.apply(AgentEvent::ToolFinished {
        call: call("alpha"),
        content: "first completed".into(),
        changes: vec![],
        files: None,
    });
    assert_eq!(state.expanded_tool_groups, [2].into());
    state.expanded_tool_groups.insert(0);
    state.manual_disclosures.insert(0);
    state.apply(AgentEvent::ToolFinished {
        call: call("beta"),
        content: "second completed".into(),
        changes: vec![],
        files: None,
    });
    assert_eq!(state.expanded_tool_groups, [0].into());
}

#[test]
fn manual_disclosures_keep_the_clicked_header_at_the_same_screen_row() {
    use super::super::app::TranscriptItem;
    for group in [false, true] {
        let mut state = UiState::new("fixture");
        state
            .transcript
            .push(TranscriptItem::Assistant(
                "Generated prefix.\n\n".repeat(20),
            ));
        if group {
            for index in 0..20 {
                let tool = call(&format!("generated-{index}"));
                state.apply(AgentEvent::ToolStarted { call: tool.clone() });
                state.apply(AgentEvent::ToolFinished {
                    call: tool,
                    content: "complete".into(),
                    changes: vec![],
                    files: None,
                });
            }
        } else {
            state.apply(AgentEvent::Model(StreamEvent::ReasoningDelta(
                "Generated analysis.\n\n".repeat(40),
            )));
            state.apply(AgentEvent::ReasoningFinished { elapsed_ms: 13 });
        }
        state.apply(AgentEvent::Model(StreamEvent::TextDelta(
            "Generated suffix.\n\n".repeat(30),
        )));
        state.selection.viewport = Rect::new(0, 0, 80, 12);
        state.transcript_layout.update_expanded(
            &state.transcript,
            &state.expanded,
            &state.expanded_tool_groups,
            80,
        );
        state.max_scroll = state
            .transcript_layout
            .height()
            .saturating_sub(12);
        let header = state
            .transcript_layout
            .item_offset(1)
            .unwrap();
        let top = header.saturating_sub(4);
        state.scroll = state.max_scroll - top;
        state.follow_bottom = false;
        for _ in 0..2 {
            pointer::toggle_transcript(&mut state, 1, group);
            assert_eq!(
                state
                    .transcript_layout
                    .item_offset(1)
                    .unwrap()
                    - (state.max_scroll - state.scroll),
                header - top
            );
            assert!(!state.follow_bottom);
        }
    }
}
