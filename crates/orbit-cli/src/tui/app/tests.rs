#[test]
fn background_tools_finish_the_original_row_and_cancel_pending_rows() {
    let mut state = UiState::new("orbit-test");
    let first = call("orbit-first");
    let second = call("orbit-second");
    state.apply(AgentEvent::ToolStarted {
        call: first.clone(),
    });
    state.apply(AgentEvent::ToolBackgrounded {
        call: first.clone(),
        job_id: "job-orbit-first".into(),
        content: "{\"running\":true}".into(),
    });
    state.apply(AgentEvent::ToolStarted { call: second });
    state.apply(AgentEvent::Model(StreamEvent::TextDelta(
        "Other Orbit work".into(),
    )));
    state.apply(AgentEvent::ToolFinished {
        call: first,
        content: "Orbit finished".into(),
        changes: vec![],
    });
    assert_eq!(state.tool_calls, 2);
    let TranscriptItem::Tool(first) = &state.transcript[0] else {
        panic!("missing first command");
    };
    assert_eq!(first.status, ToolStatus::Finished);
    assert_eq!(first.output, "Orbit finished");
    let TranscriptItem::Tool(second) = &state.transcript[1] else {
        panic!("missing second command");
    };
    assert_eq!(second.status, ToolStatus::Running);
    state.apply(AgentEvent::Cancelled);
    let TranscriptItem::Tool(second) = &state.transcript[1] else {
        panic!("missing second command");
    };
    assert_eq!(second.status, ToolStatus::Failed);
    assert!(second.output.contains("取消"));
}

#[test]
fn resuming_after_command_wait_starts_a_separate_assistant_message() {
    let mut state = UiState::new("orbit-test");
    state.apply(AgentEvent::TurnStarted { turn: 1 });
    state.apply(AgentEvent::Model(StreamEvent::TextDelta(
        "Orbit is building.".into(),
    )));
    state.apply(AgentEvent::WaitingForCommands {
        job_ids: vec!["job-orbit".into()],
    });
    state.apply(AgentEvent::TurnStarted { turn: 2 });
    state.apply(AgentEvent::Model(StreamEvent::TextDelta(
        "Orbit build passed.".into(),
    )));
    assert!(
        matches!(&state.transcript[0], TranscriptItem::Assistant(text) if text == "Orbit is building.")
    );
    assert!(
        matches!(&state.transcript[1], TranscriptItem::Assistant(text) if text == "Orbit build passed.")
    );
}

#[test]
fn reasoning_completion_only_offers_current_model_capabilities() {
    use orbit_model::types::ReasoningEffort;

    let mut state = super::UiState::new("provider::model");
    state.reasoning_efforts = vec![ReasoningEffort::Low, ReasoningEffort::High];
    state.input.set("/reasoning ".into());

    let names = state
        .menu()
        .into_iter()
        .map(|item| item.name)
        .collect::<Vec<_>>();

    assert_eq!(names, ["/reasoning low", "/reasoning high"]);
    state.input.set("/reasoning hi".into());
    assert_eq!(state.menu()[0].name, "/reasoning high");
}
use orbit_model::types::FunctionCall;

use super::*;

#[test]
fn reasoning_finishes_before_text_tools_and_terminal_events() {
    let endings = [
        AgentEvent::Model(StreamEvent::TextDelta("answer".into())),
        AgentEvent::ToolStarted { call: call("tool") },
        AgentEvent::Completed {
            turns: 1,
            tool_calls: 0,
        },
        AgentEvent::Failed {
            error: "network".into(),
        },
        AgentEvent::Cancelled,
    ];

    for ending in endings {
        let mut state = UiState::new("mock");
        state.apply(AgentEvent::Model(StreamEvent::ReasoningDelta(
            "first".into(),
        )));
        state.apply(AgentEvent::Model(StreamEvent::ReasoningDelta(
            " second".into(),
        )));
        let TranscriptItem::Thinking(row) = &mut state.transcript[0] else {
            panic!("missing reasoning");
        };
        assert!(!row.finished);
        row.started_at = Some(Instant::now() - std::time::Duration::from_millis(7500));

        state.apply(ending);

        let TranscriptItem::Thinking(row) = &state.transcript[0] else {
            panic!("missing reasoning");
        };
        assert!(row.finished);
        assert_eq!(row.content, "first second");
        assert!(row.elapsed.unwrap() >= std::time::Duration::from_millis(7500));
        let elapsed = row.elapsed;
        state.finish_clock();
        let TranscriptItem::Thinking(row) = &state.transcript[0] else {
            unreachable!()
        };
        assert_eq!(
            row.elapsed, elapsed,
            "later completion must not extend reasoning time"
        );
    }
}

#[test]
fn thinking_duration_measures_reasoning_stream_time() {
    let mut state = UiState::new("mock");
    state.apply(AgentEvent::ContextUsage { estimated_input: 1 });
    state.model_started_at = Some(Instant::now() - std::time::Duration::from_secs(3));

    state.apply(AgentEvent::Model(StreamEvent::ReasoningDelta(
        "buffered reasoning".into(),
    )));
    state.apply(AgentEvent::Model(StreamEvent::TextDelta("answer".into())));

    let TranscriptItem::Thinking(row) = &state.transcript[0] else {
        panic!("missing reasoning");
    };
    assert!(row.elapsed.unwrap() < std::time::Duration::from_secs(2));
    assert!(state.model_started_at.is_none());
}

#[test]
fn replay_preserves_recorded_reasoning_time() {
    let events = vec![
        AgentEvent::Model(StreamEvent::ReasoningDelta("reasoning".into())),
        AgentEvent::ReasoningFinished { elapsed_ms: 7500 },
        AgentEvent::Model(StreamEvent::Completed {
            finish_reason: None,
            usage: None,
        }),
    ];
    let stored = serde_json::to_string(&events).unwrap();
    let restored: Vec<AgentEvent> = serde_json::from_str(&stored).unwrap();
    let mut state = UiState::new("mock");

    for event in restored {
        state.apply_replayed(event);
    }

    let TranscriptItem::Thinking(row) = &state.transcript[0] else {
        panic!("missing reasoning");
    };
    assert_eq!(row.elapsed, Some(std::time::Duration::from_millis(7500)));
}

#[test]
fn replay_does_not_invent_reasoning_duration() {
    let mut state = UiState::new("mock");
    state.apply_replayed(AgentEvent::Model(StreamEvent::ReasoningDelta(
        "old thought".into(),
    )));
    state.apply_replayed(AgentEvent::Model(StreamEvent::TextDelta(
        "old answer".into(),
    )));

    let TranscriptItem::Thinking(row) = &state.transcript[0] else {
        panic!("missing reasoning");
    };
    assert!(row.finished);
    assert_eq!(row.elapsed, None);
}

#[test]
fn approval_wait_is_excluded_from_response_clock() {
    let mut state = UiState::new("mock");
    state.begin_run();
    state.started_at = Some(Instant::now() - std::time::Duration::from_secs(12));
    state.pause_clock();
    state.paused_at = Some(Instant::now() - std::time::Duration::from_secs(8));
    let active = state.active_elapsed();

    assert!(active >= std::time::Duration::from_secs(3));
    assert!(active < std::time::Duration::from_secs(5));

    state.resume_clock();
    state.finish_clock();

    assert!(state.elapsed >= std::time::Duration::from_secs(3));
    assert!(state.elapsed < std::time::Duration::from_secs(5));
}

fn call(id: &str) -> ToolCall {
    ToolCall {
        metadata: None,
        id: id.to_owned(),
        kind: "function".to_owned(),
        function: FunctionCall {
            name: "echo".to_owned(),
            arguments: "{}".to_owned(),
        },
    }
}

#[test]
fn reducer_tracks_stream_and_tool_lifecycle() {
    let mut state = UiState::new("mock");
    state.submit("hello".to_owned());
    state.apply(AgentEvent::TurnStarted { turn: 1 });
    state.apply(AgentEvent::Model(StreamEvent::TextDelta("hi".to_owned())));
    state.apply(AgentEvent::ToolStarted { call: call("one") });
    state.apply(AgentEvent::ToolFinished {
        call: call("one"),
        content: "done".to_owned(),
        changes: Vec::new(),
    });
    state.apply(AgentEvent::Completed {
        turns: 1,
        tool_calls: 1,
    });

    assert_eq!(state.status, RunStatus::Completed);
    assert!(matches!(
        &state.transcript[1],
        TranscriptItem::Assistant(content) if content == "hi"
    ));
    assert!(matches!(
        &state.transcript[2],
        TranscriptItem::Tool(row) if row.status == ToolStatus::Finished && row.output == "done"
    ));
}

#[test]
fn running_menu_keeps_only_safe_local_commands() {
    let mut state = UiState::new("mock");
    state.status = RunStatus::Running;
    state.input.insert("/");
    let commands = state.menu();

    assert!(commands.iter().any(|command| command.name == "/model"));
    assert!(commands.iter().any(|command| command.name == "/status"));
    assert!(!commands.iter().any(|command| command.name == "/new"));
    assert!(!commands.iter().any(|command| command.name == "/compact"));
}

#[test]
fn compaction_uses_one_quiet_maintenance_row() {
    let mut state = UiState::new("mock");
    state.apply(AgentEvent::CompactionStarted);

    assert!(state.transcript.is_empty());
    assert_eq!(state.activity.label(), Some("正在压缩上下文"));

    state.apply(AgentEvent::ContextCompacted {
        strategy: Default::default(),
        before_tokens: 195_754,
        after_tokens: 96_000,
        fallback: true,
        fallback_reason: Some("摘要请求超时".into()),
    });

    assert_eq!(state.context_used_tokens, 96_000);
    assert!(matches!(
        state.transcript.as_slice(),
        [TranscriptItem::Maintenance(text)]
            if text.contains("195754 → 96000") && text.contains("保留最近工作内容")
    ));
}

#[test]
fn archive_projection_explains_that_older_constraints_need_retrieval() {
    use orbit_agent::memory::{CompactionMode, CompactionRecord, FileActivity, TaskSummary};

    let mut state = UiState::new("mock");
    state.apply(AgentEvent::CompactionRecorded {
        record: CompactionRecord {
            version: 2,
            mode: CompactionMode::Manual,
            source_messages: 8,
            retained_from: 4,
            pinned_user: None,
            before_tokens: 200_000,
            after_tokens: 40_000,
            strategy: orbit_agent::memory::CompactionStrategy::ArchiveProjection,
            summary_requests: 1,
            summary_input_tokens: 2_048,
            archive: Some("read_history archive_id=12; search with query".into()),
            fallback: true,
            fallback_reason: None,
            summary: TaskSummary {
                goal: String::new(),
                constraints: vec![],
                decisions: vec![],
                completed: vec![],
                remaining: vec![],
                verification: vec![],
                uncertainty: vec![],
            },
            files: FileActivity::default(),
        },
    });
    state.apply(AgentEvent::ContextCompacted {
        strategy: orbit_agent::memory::CompactionStrategy::ArchiveProjection,
        before_tokens: 200_000,
        after_tokens: 40_000,
        fallback: true,
        fallback_reason: None,
    });

    assert!(matches!(
        state.transcript.as_slice(),
        [TranscriptItem::Maintenance(text)]
            if text.contains("摘要未完成") && text.contains("归档 #12") && text.contains("旧约束需按需回读")
    ));
}

#[test]
fn context_indicator_tracks_each_model_request_during_a_run() {
    let mut state = UiState::new("mock");
    state.begin_run();
    state.apply(AgentEvent::ContextUsage {
        estimated_input: 4_000,
    });

    assert_eq!(state.context_used_tokens, 4_000);

    state.apply(AgentEvent::ContextUsage {
        estimated_input: 12_000,
    });

    assert_eq!(state.context_used_tokens, 12_000);

    state.apply(AgentEvent::RequestMetrics {
        profile: "mock".into(),
        estimated_input: 12_000,
        retries: None,
        usage: Some(orbit_model::types::Usage {
            prompt_tokens: Some(13_000),
            completion_tokens: None,
            total_tokens: None,
        }),
        elapsed_ms: 100,
    });

    assert_eq!(state.context_used_tokens, 13_000);
}

#[test]
fn activity_tracks_compaction_and_concurrent_tools_without_background_completion_override() {
    let mut state = UiState::new("mock");
    state.submit("work".into());
    assert_eq!(state.activity.label(), Some("等待模型响应"));
    state.apply(AgentEvent::CompactionStarted);
    assert_eq!(state.activity.label(), Some("正在压缩上下文"));
    state.apply(AgentEvent::CompactionFailed {
        kind: None,
        error: "timeout".into(),
    });
    assert_eq!(state.activity.label(), Some("等待模型响应"));
    state.apply(AgentEvent::Model(StreamEvent::ReasoningDelta(
        "reason".into(),
    )));
    assert_eq!(state.activity.label(), None);
    let mut read = call("read");
    read.function.name = "read_file".into();
    let mut edit = call("edit");
    edit.function.name = "edit_file".into();
    state.apply(AgentEvent::ToolStarted { call: read.clone() });
    state.apply(AgentEvent::ToolStarted { call: edit.clone() });
    assert_eq!(state.activity.label(), None);
    state.apply(AgentEvent::ToolFinished {
        call: edit,
        content: String::new(),
        changes: vec![],
    });
    assert_eq!(state.activity.label(), None);
    state.apply(AgentEvent::ToolBackgrounded {
        call: read.clone(),
        job_id: "job".into(),
        content: String::new(),
    });
    state.apply(AgentEvent::WaitingForTasks {
        job_ids: vec!["job".into()],
    });
    assert_eq!(state.activity.label(), None);
    state.apply(AgentEvent::TurnStarted { turn: 2 });
    state.apply(AgentEvent::Model(StreamEvent::TextDelta("answer".into())));
    state.apply(AgentEvent::ToolFinished {
        call: read,
        content: String::new(),
        changes: vec![],
    });
    assert_eq!(state.activity.label(), None);
    state.apply(AgentEvent::Cancelled);
    state.begin_run();
    assert_eq!(state.activity.label(), Some("等待模型响应"));
}
