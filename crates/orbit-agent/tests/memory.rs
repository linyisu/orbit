use orbit_agent::{
    autocompact, context,
    events::AgentEvent,
    memory::{FileActivity, TaskSummary},
    model::MockModel,
};
use orbit_model::types::{ChatCompletion, ChatMessage, ToolCall};
use tokio_util::sync::CancellationToken;

fn call(id: &str, name: &str, path: &str) -> ToolCall {
    ToolCall::function(id, name, serde_json::json!({ "path": path }))
}

fn summary() -> TaskSummary {
    TaskSummary {
        goal: "修复任务，不修改测试".into(),
        constraints: vec!["保留 API".into()],
        decisions: vec![],
        completed: vec![],
        remaining: vec!["运行测试".into()],
        verification: vec![],
        uncertainty: vec!["测试尚未运行".into()],
    }
}

fn backend() -> MockModel {
    MockModel::new((0..20).map(|_| ChatCompletion {
        id: None,
        model: None,
        message: ChatMessage::assistant(serde_json::to_string(&summary()).unwrap()),
        finish_reason: Some("stop".into()),
        usage: None,
    }))
}

#[test]
fn only_confirmed_file_receipts_are_recorded_not_failed_tools_or_model_claims() {
    let read = call("r", "read_file", "source.rs");
    let edit = call("e", "edit_file", "source.rs");
    let rejected = call("denied", "write_file", "secret.txt");
    let shell = call("sh", "run_command", "shell.txt");
    let events = vec![
        AgentEvent::ToolStarted {
            call: rejected.clone(),
        },
        AgentEvent::ToolFailed {
            call: rejected,
            error: "denied".into(),
        },
        AgentEvent::ToolFinished {
            call: read.clone(),
            content: r#"{"path":"source.rs","total_lines":12,"content":"1: source"}"#.into(),
            changes: Vec::new(),
        },
        AgentEvent::ToolFinished {
            call: edit.clone(),
            content: r#"{"path":"source.rs","written_bytes":0}"#.into(),
            changes: Vec::new(),
        },
        AgentEvent::ToolFinished {
            call: shell,
            content: r#"{"path":"shell.txt","written_bytes":99}"#.into(),
            changes: Vec::new(),
        },
        AgentEvent::ToolFinished {
            call: call("fake", "write_file", "fake.txt"),
            content: "I wrote fake.txt".into(),
            changes: Vec::new(),
        },
    ];
    let activity = FileActivity::from_events(&events);

    assert_eq!(
        activity.read.iter().cloned().collect::<Vec<_>>(),
        ["source.rs"]
    );
    assert_eq!(
        activity.written.iter().cloned().collect::<Vec<_>>(),
        ["source.rs"]
    );

    let history = vec![
        ChatMessage::assistant_tool_calls(vec![read, edit]),
        ChatMessage::tool(
            "r",
            r#"{"path":"source.rs","total_lines":12,"content":"1: source"}"#,
        ),
        ChatMessage::tool("e", r#"{"path":"source.rs","written_bytes":0}"#),
        ChatMessage::assistant("I also wrote secret.txt"),
    ];

    assert_eq!(FileActivity::from_history(&history), activity);
}

#[test]
fn structured_summary_rejects_missing_fields_and_accepts_fenced_json() {
    assert!(TaskSummary::parse("a vague summary").is_err());
    assert!(TaskSummary::parse(r#"{"goal":"missing sections"}"#).is_err());

    let valid = summary();

    assert_eq!(
        TaskSummary::parse(&format!(
            "```json\n{}\n```",
            serde_json::to_string(&valid).unwrap()
        ))
        .unwrap(),
        valid
    );

    let mut empty = valid;
    empty.goal = " ".into();

    assert!(TaskSummary::parse(&serde_json::to_string(&empty).unwrap()).is_err());
    assert_eq!(
        TaskSummary::parse_model_response(&format!(
            "Here is the summary:\n```json\n{}\n```\nDone.",
            serde_json::to_string(&summary()).unwrap()
        ))
        .unwrap(),
        summary()
    );
    assert!(TaskSummary::parse_model_response(r#"{"goal":"missing sections"}"#).is_err());
}

#[tokio::test]
async fn repeated_compaction_and_event_reload_keep_confirmed_files_and_cut_metadata() {
    let mut activity = FileActivity::default();
    activity.observe(
        &call("old", "edit_file", "original.rs"),
        r#"{"path":"original.rs","written_bytes":80}"#,
    );
    let mut history=vec![ChatMessage::system("system"),ChatMessage::user("exact current goal"),
        ChatMessage::assistant_tool_calls(vec![call("r","read_file","large.rs")]),
        ChatMessage::tool("r",serde_json::json!({ "path": "large.rs", "total_lines": 100, "content": "detail ".repeat(2600) }).to_string()),
        ChatMessage::assistant_tool_calls(vec![call("last","read_file","recent.rs")]),
        ChatMessage::tool("last",r#"{"path":"recent.rs","total_lines":1,"content":"recent"}"#)];
    let plan = autocompact::plan(&history, &[], 1000, 8000)
        .unwrap()
        .with_files(&activity);
    let first = autocompact::execute(&backend(), plan, &[], 1000, 8000, CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(first.record.source_messages, 6);
    assert_eq!(first.record.retained_from, 4);
    assert_eq!(first.record.pinned_user, Some(1));
    assert_eq!(first.record.summary, summary());

    let serialized = serde_json::to_string(&AgentEvent::CompactionRecorded {
        record: first.record.clone(),
    })
    .unwrap();
    let event: AgentEvent = serde_json::from_str(&serialized).unwrap();
    let reloaded = FileActivity::from_events([&event]);

    assert!(reloaded.written.contains("original.rs"));
    assert!(reloaded.read.contains("large.rs"));

    history = first.history;
    history.extend([
        ChatMessage::assistant("more data ".repeat(2000)),
        ChatMessage::user("next goal"),
    ]);
    let second = autocompact::execute(
        &backend(),
        autocompact::plan(&history, &[], 1000, 8000)
            .unwrap()
            .with_files(&reloaded),
        &[],
        1000,
        8000,
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(second.record.files, reloaded);
    assert!(
        serde_json::to_string(&second.history)
            .unwrap()
            .contains("original.rs")
    );
}

#[tokio::test]
async fn manual_compaction_uses_same_structure_and_records_its_boundary() {
    let history = vec![
        ChatMessage::system("system"),
        ChatMessage::user("old"),
        ChatMessage::assistant("long ".repeat(2000)),
        ChatMessage::user("recent"),
        ChatMessage::assistant("done"),
        ChatMessage::user("latest"),
        ChatMessage::assistant("done"),
    ];
    let result = context::compact(&backend(), history, &[], 32000, CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(
        result.record.mode,
        orbit_agent::memory::CompactionMode::Manual
    );
    assert_eq!(result.record.retained_from, 3);
    assert_eq!(result.record.source_messages, 7);
    assert_eq!(result.record.pinned_user, None);
    assert_eq!(result.record.summary, summary());
}
