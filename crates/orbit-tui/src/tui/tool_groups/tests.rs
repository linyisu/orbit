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
fn interleaved_sources_keep_click_identity_after_reordering_and_resize() {
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
        layout.update_expanded(&state.transcript, &state.expanded, &[0].into(), width);
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
