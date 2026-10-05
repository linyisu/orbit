use orbit_agent::{checkpoint::ExecutionCheckpoint, events::AgentEvent};
use orbit_model::types::{ChatMessage, StreamEvent, ToolCall};
use orbit_store::{side_questions::SideQuestion, store::Store};

#[tokio::test]
async fn side_records_survive_restart_are_session_scoped_and_do_not_change_main_history() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("orbit.db");
    let store = Store::open(&path).await.unwrap();
    let writer = store
        .create(dir.path().display().to_string(), "orbit-fast".into(), false)
        .await
        .unwrap();
    let other = store
        .create(dir.path().display().to_string(), "orbit-fast".into(), false)
        .await
        .unwrap();
    let id = writer.id().to_owned();
    let before = store.load(&id).await.unwrap();
    let usage = store
        .session_storage_usage()
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.id == id)
        .unwrap()
        .payload_bytes;

    for status in ["completed", "failed", "cancelled"] {
        store
            .save_side_question(
                &id,
                SideQuestion {
                    thread_id: String::new(),
                    model: "orbit-fast".into(),
                    question: format!("Orbit {status}?"),
                    answer: "独立回答".into(),
                    status: status.into(),
                    error: None,
                },
            )
            .await
            .unwrap();
    }

    assert!(store.side_questions(other.id()).await.unwrap().is_empty());
    let after = store.load(&id).await.unwrap();
    assert_eq!(before.history, after.history);
    assert_eq!(before.session.updated_at, after.session.updated_at);
    assert!(after.runs.is_empty());
    assert!(
        store
            .session_storage_usage()
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.id == id)
            .unwrap()
            .payload_bytes
            > usage
    );
    drop(writer);
    let reopened = Store::open(&path).await.unwrap();
    assert_eq!(reopened.side_questions(&id).await.unwrap().len(), 3);
    assert!(reopened.delete_session(&id).await.unwrap());
    assert!(reopened.side_questions(&id).await.unwrap().is_empty());
}

#[tokio::test]
async fn current_context_includes_streaming_text_and_unresolved_tools_without_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("orbit.db")).await.unwrap();
    let mut writer = store
        .create(dir.path().display().to_string(), "orbit-fast".into(), false)
        .await
        .unwrap();
    writer
        .begin("检查 Orbit".into(), "orbit-fast".into())
        .await
        .unwrap();
    let journal = writer.execution_journal().unwrap();
    let call = ToolCall::function(
        "orbit-call",
        "read_file",
        serde_json::json!({"path":"orbit.rs"}),
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
                in_flight: None,
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
    let context = store.side_question_context(writer.id()).await.unwrap();
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
                in_flight: None,
                attachments: vec![],
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        store.side_question_context(writer.id()).await.unwrap(),
        saved
    );
    let pending = vec![
        ChatMessage::user("检查 Orbit"),
        ChatMessage::assistant_tool_calls(vec![ToolCall::function(
            "pending",
            "read_file",
            serde_json::json!({"path":"orbit.rs"}),
        )]),
    ];

    journal
        .save(
            ExecutionCheckpoint {
                history: pending.clone(),
                turns: 3,
                tool_calls: 2,
                in_flight: Some("pending".into()),
                attachments: vec![],
            },
            Some(AgentEvent::TurnStarted { turn: 3 }),
        )
        .await
        .unwrap();
    assert_eq!(
        store.side_question_context(writer.id()).await.unwrap(),
        pending
    );
    assert_eq!(
        store.load(writer.id()).await.unwrap().runs[0].status,
        "running"
    );
}

#[tokio::test]
async fn context_handles_the_gap_before_checkpoint_and_failed_partial_answers() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("orbit.db")).await.unwrap();
    let mut writer = store
        .create(dir.path().display().to_string(), "orbit-fast".into(), false)
        .await
        .unwrap();
    writer
        .begin("Orbit 问题".into(), "orbit-fast".into())
        .await
        .unwrap();
    assert_eq!(
        store.side_question_context(writer.id()).await.unwrap(),
        vec![ChatMessage::user("Orbit 问题")]
    );
    writer
        .finish(
            "failed",
            vec![
                AgentEvent::Model(StreamEvent::TextDelta("部分回答".into())),
                AgentEvent::Failed {
                    error: "Orbit 请求失败".into(),
                },
            ],
            None,
        )
        .await
        .unwrap();
    let context = store.side_question_context(writer.id()).await.unwrap();
    assert!(context.contains(&ChatMessage::assistant("部分回答")));
    assert!(context.iter().any(
        |m| matches!(m, ChatMessage::User { content, .. } if content.contains("Orbit 请求失败"))
    ));
}

#[tokio::test]
async fn legacy_side_questions_keep_stable_thread_ids_after_restart_and_follow_up() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("orbit.db");
    let store = Store::open(&path).await.unwrap();
    let writer = store
        .create(
            dir.path().display().to_string(),
            "orbit-model".into(),
            false,
        )
        .await
        .unwrap();
    let id = writer.id().to_owned();
    let old = serde_json::json!({"model":"orbit-model","question":"old question","answer":"old answer","status":"completed","error":null});
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection
        .execute(
            "INSERT INTO side_questions(session_id,payload) VALUES(?1,?2)",
            rusqlite::params![id, old.to_string()],
        )
        .unwrap();
    drop(connection);
    let original = store.side_questions(&id).await.unwrap().remove(0);
    assert!(original.thread_id.starts_with("legacy-"));
    let mut follow_up = original.clone();
    follow_up.question = "continued question".into();
    store.save_side_question(&id, follow_up).await.unwrap();
    let reopened = Store::open(&path).await.unwrap();
    let records = reopened.side_questions(&id).await.unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].thread_id, original.thread_id);
    assert_eq!(records[1].thread_id, original.thread_id);
}
