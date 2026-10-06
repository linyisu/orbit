use super::*;
use orbit_model::types::ToolCall;

#[test]
fn tool_less_requests_preserve_context_without_unresolved_protocol_calls() {
    let history = vec![
        ChatMessage::system("Orbit 项目指令"),
        ChatMessage::user("检查 Orbit"),
        ChatMessage::Assistant {
            content: Some("读取文件".into()),
            tool_calls: vec![ToolCall::function(
                "orbit-call",
                "read_file",
                serde_json::json!({"path":"orbit.rs"}),
            )],
        },
        ChatMessage::tool("orbit-call", "Orbit 文件内容"),
        ChatMessage::assistant_tool_calls(vec![ToolCall::function(
            "pending",
            "read_file",
            serde_json::json!({"path":"orbit.md"}),
        )]),
        ChatMessage::assistant("正在检查中文"),
    ];
    let sent = request(
        history.clone(),
        &[],
        &[],
        "什么原因？",
        Some(ReasoningEffort::High),
        4096,
    );

    assert!(sent.tools.is_empty());
    assert_eq!(sent.reasoning_effort, Some(ReasoningEffort::High));
    assert_eq!(sent.max_tokens, Some(4096));
    assert!(sent.messages.iter().all(|message| match message {
        ChatMessage::Tool { .. } => false,
        ChatMessage::Assistant { tool_calls, .. } => tool_calls.is_empty(),
        _ => true,
    }));
    let text = serde_json::to_string(&sent.messages).unwrap();
    for expected in [
        "Orbit 项目指令",
        "Orbit 文件内容",
        "正在检查中文",
        "pending",
    ] {
        assert!(text.contains(expected));
    }

    assert_eq!(sent.messages.last(), Some(&ChatMessage::user("什么原因？")));
    assert_eq!(history.len(), 6);
}

#[tokio::test]
async fn recovery_retains_partial_answers_without_replaying_a_question() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("session.db"))
        .await
        .unwrap();
    let writer = store
        .create(
            directory.path().to_string_lossy().into_owned(),
            "mock".into(),
            false,
        )
        .await
        .unwrap();
    let session = writer.id();
    let mut record = SideQuestion {
        thread_id: "thread".into(),
        model: "mock".into(),
        question: "question".into(),
        answer: String::new(),
        status: "running".into(),
        error: None,
    };
    let checkpoint = store.begin_side_question(session, &record).await.unwrap();
    record.answer = "已生成的部分回答".into();
    store
        .checkpoint_side_question(session, checkpoint, &record)
        .await
        .unwrap();
    let mut recovered = SideQuestions::default();
    recovered.load(&store, session).await.unwrap();
    assert!(!recovered.running());
    assert_eq!(recovered.records().len(), 1);
    assert_eq!(recovered.records()[0].status, "interrupted");
    assert_eq!(recovered.records()[0].answer, record.answer);
    assert_eq!(
        store.side_questions(session).await.unwrap()[0].status,
        "interrupted"
    );
}
