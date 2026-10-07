use super::*;
use orbit_service::session::SessionState;

#[tokio::test]
async fn replay_is_incremental_exact_and_preserves_scroll_and_saved_elapsed_time() {
    use orbit_model::types::{StreamEvent, ToolCall};

    let tmp = tempfile::tempdir().unwrap();
    let session = SessionState::open(tmp.path().join("sessions.db"), None, "model".into())
        .await
        .unwrap();
    let mut snapshot = session.snapshot.clone();
    snapshot.runs = (0..3)
        .map(|index| {
            let call = ToolCall::function(
                format!("tool-{index}"),
                "read_file",
                serde_json::json!({ "path": "test" }),
            );
            let mut events = vec![AgentEvent::TurnStarted { turn: 1 }];
            events
                .extend((0..40).map(|_| AgentEvent::Model(StreamEvent::TextDelta("中文".into()))));
            events.push(AgentEvent::ToolStarted { call: call.clone() });

            if index != 1 {
                events.push(AgentEvent::ToolFinished {
                    call,
                    content: "result".into(),
                    changes: vec![],
                });
            }

            orbit_store::types::Run {
                images: Vec::new(),
                prompt: format!("prompt-{index}"),
                status: "completed".into(),
                elapsed_ms: Some(12345),
                events,
            }
        })
        .collect();
    let mut ui = UiState::new("model");
    ui.input.insert("draft");
    ui.scroll = 5;
    ui.follow_bottom = false;
    let mut replay = HistoryReplay::default();

    assert!(!replay.advance(&snapshot, &mut ui, 2, Duration::MAX));
    assert_eq!(ui.input.history.len(), 1);
    assert!(matches!(ui.transcript.last(), Some(TranscriptItem::Assistant(s)) if s == "中文"));

    let mut batches = 1;

    while !replay.advance(&snapshot, &mut ui, 7, Duration::MAX) {
        batches += 1;
    }

    assert!(batches > 10);
    assert_eq!(ui.input.history, vec!["prompt-0", "prompt-1", "prompt-2"]);
    assert_eq!(ui.input.text, "draft");
    assert_eq!(ui.scroll, 5);
    assert!(!ui.follow_bottom);
    assert_eq!(ui.elapsed, Duration::from_millis(12345));
    assert!(ui.started_at.is_none());

    let texts: Vec<_> = ui
        .transcript
        .iter()
        .filter_map(|item| {
            if let TranscriptItem::Assistant(s) = item {
                Some(s)
            } else {
                None
            }
        })
        .collect();

    assert_eq!(texts.len(), 3);
    assert!(texts.iter().all(|s| **s == "中文".repeat(40)));

    let statuses: Vec<_> = ui
        .transcript
        .iter()
        .filter_map(|item| {
            if let TranscriptItem::Tool(t) = item {
                Some(t.status.clone())
            } else {
                None
            }
        })
        .collect();

    assert_eq!(
        statuses,
        vec![
            ToolStatus::Finished,
            ToolStatus::Failed,
            ToolStatus::Finished
        ]
    );

    let count = ui.transcript.len();

    assert!(replay.advance(&snapshot, &mut ui, 7, Duration::MAX));
    assert_eq!(ui.transcript.len(), count);
    assert!(
        session
            .store
            .load(session.id())
            .await
            .unwrap()
            .runs
            .is_empty()
    );
}
