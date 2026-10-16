use orbit_agent::{checkpoint::ExecutionCheckpoint, events::AgentEvent};
use orbit_model::types::{ChatMessage, StreamEvent, ToolCall};
use orbit_store::store::Store;

#[tokio::test]
async fn current_context_includes_streaming_text_and_unresolved_tools_without_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("orbit.db"))
        .await
        .unwrap();
    let mut writer = store
        .create(dir.path().display().to_string(), "orbit-fast".into())
        .await
        .unwrap();
    writer
        .begin("检查 Orbit".into(), "orbit-fast".into())
        .await
        .unwrap();
    let journal = writer.execution_journal().unwrap();
    let call = ToolCall::function(
        "orbit-call",
        "ReadFile",
        serde_json::json!({
            "path": "orbit.rs"
        }),
    );
    let history = vec![
        ChatMessage::user("检查 Orbit"),
        ChatMessage::assistant_tool_calls(vec![call]),
        ChatMessage::tool("orbit-call", "已读取 Orbit 文件"),
    ];

    journal
        .save(
            ExecutionCheckpoint {
                history: history.clone(),
                turns: 2,
                tool_calls: 1,
                in_flight: Vec::new(),
                attachments: vec![],
            },
            Some(AgentEvent::TurnStarted { turn: 2 }),
        )
        .await
        .unwrap();
    journal.record(AgentEvent::Model(StreamEvent::TextDelta(
        "你好，正在检查".into(),
    )));
    journal.flush().await.unwrap();
    let context = store
        .live_context(writer.id())
        .await
        .unwrap();
    assert_eq!(&context[..history.len()], &history);
    assert_eq!(
        context.last(),
        Some(&ChatMessage::assistant("你好，正在检查"))
    );
    let mut saved = history.clone();
    saved.push(ChatMessage::assistant("你好，正在检查"));
    journal.record(AgentEvent::Model(StreamEvent::Completed {
        finish_reason: Some("stop".into()),
        usage: None,
    }));
    journal
        .save(
            ExecutionCheckpoint {
                history: saved.clone(),
                turns: 2,
                tool_calls: 1,
                in_flight: Vec::new(),
                attachments: vec![],
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .live_context(writer.id())
            .await
            .unwrap(),
        saved
    );
    let pending = vec![
        ChatMessage::user("检查 Orbit"),
        ChatMessage::assistant_tool_calls(vec![ToolCall::function(
            "pending",
            "ReadFile",
            serde_json::json!({
                "path": "orbit.rs"
            }),
        )]),
    ];

    journal
        .save(
            ExecutionCheckpoint {
                history: pending.clone(),
                turns: 3,
                tool_calls: 2,
                in_flight: vec!["pending".into()],
                attachments: vec![],
            },
            Some(AgentEvent::TurnStarted { turn: 3 }),
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .live_context(writer.id())
            .await
            .unwrap(),
        pending
    );
    assert_eq!(
        store
            .load(writer.id())
            .await
            .unwrap()
            .runs[0]
            .status,
        "running"
    );
}

#[tokio::test]
async fn context_handles_the_gap_before_checkpoint_and_failed_partial_answers() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("orbit.db"))
        .await
        .unwrap();
    let mut writer = store
        .create(dir.path().display().to_string(), "orbit-fast".into())
        .await
        .unwrap();
    writer
        .begin("Orbit 问题".into(), "orbit-fast".into())
        .await
        .unwrap();
    assert_eq!(
        store
            .live_context(writer.id())
            .await
            .unwrap(),
        vec![ChatMessage::user("Orbit 问题")]
    );
    writer
        .finish(
            &orbit_store::types::RunCompletion::Failed,
            vec![
                AgentEvent::Model(StreamEvent::TextDelta("部分回答".into())),
                AgentEvent::Failed {
                    error: "Orbit 请求失败".into(),
                },
            ],
        )
        .await
        .unwrap();
    let context = store
        .live_context(writer.id())
        .await
        .unwrap();
    assert!(context.contains(&ChatMessage::assistant("部分回答")));
    assert!(context.iter().any(|m| matches!(
        m,
        ChatMessage::User { content, .. } if content.contains("Orbit 请求失败")
    )));
}
