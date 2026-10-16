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
                "ReadFile",
                serde_json::json!({
                    "path": "orbit.rs"
                }),
            )],
        },
        ChatMessage::tool("orbit-call", "Orbit 文件内容"),
        ChatMessage::assistant_tool_calls(vec![ToolCall::function(
            "pending",
            "ReadFile",
            serde_json::json!({
                "path": "orbit.md"
            }),
        )]),
        ChatMessage::assistant("正在检查中文"),
    ];
    let sent = request(
        history.clone(),
        &[],
        &[],
        "什么原因？",
        Some(ReasoningEffort::High),
        4_096,
    );

    assert!(sent.tools.is_empty());
    assert_eq!(sent.reasoning_effort, Some(ReasoningEffort::High));
    assert_eq!(sent.max_tokens, Some(4_096));
    assert!(
        sent.messages
            .iter()
            .all(|message| match message {
                ChatMessage::Tool { .. } => false,
                ChatMessage::Assistant { tool_calls, .. } => tool_calls.is_empty(),
                _ => true,
            })
    );
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
