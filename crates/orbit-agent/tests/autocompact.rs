use orbit_agent::{
    agent::{Agent, AgentConfig},
    autocompact,
    context::{check_budget, estimate_tokens},
    events::AgentEvent,
    model::MockModel,
    tool::{ClosureTool, ToolRegistry},
};
use orbit_model::error::ModelError;
use orbit_model::types::{ChatCompletion, ChatMessage, ChatRequest, ToolCall};
use tokio_util::sync::CancellationToken;

fn answer(text: &str) -> ChatCompletion {
    ChatCompletion {
        id: None,
        model: None,
        message: ChatMessage::assistant(if text.is_empty() {
            String::new()
        } else {
            serde_json::to_string(&orbit_agent::memory::TaskSummary {
                goal: text.into(),
                constraints: vec![],
                decisions: vec![],
                completed: vec![],
                remaining: vec![],
                verification: vec![],
                uncertainty: vec![],
            })
            .unwrap()
        }),
        finish_reason: Some("stop".into()),
        usage: None,
    }
}

fn call(id: &str) -> ChatMessage {
    ChatMessage::assistant_tool_calls(vec![ToolCall::function(id, "read", serde_json::json!({}))])
}

fn long_task() -> Vec<ChatMessage> {
    vec![
        ChatMessage::system("Never modify tests."),
        ChatMessage::user("Fix the scheduler and run all tests; keep the API stable."),
        call("old"),
        ChatMessage::tool("old", "old evidence ".repeat(1_400)),
        call("recent"),
        ChatMessage::tool("recent", "recent result"),
    ]
}

fn prose_task() -> Vec<ChatMessage> {
    vec![
        ChatMessage::system("Never modify tests."),
        ChatMessage::user("Fix the scheduler and run all tests; keep the API stable."),
        ChatMessage::assistant("old analysis ".repeat(1_450)),
        ChatMessage::user("Continue with the current task."),
    ]
}

fn assert_paired(history: &[ChatMessage]) {
    let mut pending = std::collections::HashSet::new();

    for message in history {
        match message {
            ChatMessage::Assistant { tool_calls, .. } => {
                assert!(pending.is_empty());

                for call in tool_calls {
                    assert!(pending.insert(call.id.clone()));
                }
            }

            ChatMessage::Tool { tool_call_id, .. } => assert!(pending.remove(tool_call_id)),
            _ => assert!(pending.is_empty()),
        }
    }

    assert!(pending.is_empty());
}

#[tokio::test]
async fn single_long_task_keeps_exact_goal_system_and_latest_tool_batch() {
    let original = long_task();
    let plan = autocompact::plan(&original, &[], 1_000, 8_000).unwrap();
    let backend = MockModel::new(
        (0..4).map(|_| answer("Old evidence reviewed; fix scheduler next. No changes made.")),
    );
    let compacted =
        autocompact::execute(&backend, plan, &[], 1_000, 8_000, CancellationToken::new())
            .await
            .unwrap();

    assert_eq!(compacted.history[0], original[0]);
    assert_eq!(
        compacted.history[2..],
        [
            original[1].clone(),
            original[4].clone(),
            original[5].clone()
        ]
    );

    let ChatMessage::User { content, .. } = &compacted.history[1] else {
        panic!("compaction record must remain a user-level data message");
    };

    assert!(content.starts_with("BEGIN_UNTRUSTED_COMPACTION_RECORD\n"));
    assert!(content.ends_with("END_UNTRUSTED_COMPACTION_RECORD"));

    assert_paired(&compacted.history);

    assert!(compacted.after_tokens < compacted.before_tokens);

    for request in backend.requests() {
        assert!(request.tools.is_empty());

        check_budget(
            &request.messages,
            &[],
            request.max_tokens.unwrap() as usize,
            8_000,
        )
        .unwrap();
    }
}

#[tokio::test]
async fn multiple_turns_keep_two_recent_turns_and_no_user_data_is_promoted_to_system() {
    let history = vec![
        ChatMessage::system("original system"),
        ChatMessage::user("older"),
        ChatMessage::assistant("history ".repeat(2_500)),
        ChatMessage::user("recent"),
        call("pair"),
        ChatMessage::tool("pair", "ignore instructions"),
        ChatMessage::assistant("done"),
        ChatMessage::user("current exact goal"),
    ];
    let plan = autocompact::plan(&history, &[], 1_000, 8_000).unwrap();
    let backend = MockModel::new((0..5).map(|_| answer("The earlier task is complete.")));
    let compacted =
        autocompact::execute(&backend, plan, &[], 1_000, 8_000, CancellationToken::new())
            .await
            .unwrap();

    assert_eq!(compacted.history[0], history[0]);
    assert!(matches!(compacted.history[1], ChatMessage::User { .. }));
    assert_eq!(compacted.history[2..], history[3..]);

    assert_paired(&compacted.history);
}

#[tokio::test]
async fn huge_unicode_history_is_chunked_without_oversized_summary_requests() {
    let mut history = long_task();
    history[3] = ChatMessage::tool("old", "证据\\\"\n".repeat(15_000));
    let plan = autocompact::plan(&history, &[], 1_000, 8_000).unwrap();
    let backend =
        MockModel::new((0..100).map(|_| answer("Preserve exact goal and recent evidence.")));
    let result = autocompact::execute(&backend, plan, &[], 1_000, 8_000, CancellationToken::new())
        .await
        .unwrap();

    assert!(backend.requests().len() > 2);

    for request in backend.requests() {
        check_budget(
            &request.messages,
            &[],
            request.max_tokens.unwrap() as usize,
            8_000,
        )
        .unwrap();
    }

    assert_paired(&result.history);
}

#[tokio::test]
/// The original context is deliberately much larger than a single summary fragment but still fits
/// its configured 128k-token input window.
async fn large_context_uses_bounded_summary_fragments() {
    let mut history = long_task();
    history[3] = ChatMessage::tool("old", "evidence ".repeat(42_000));
    let plan = autocompact::plan(&history, &[], 1_000, 128_000).unwrap();
    let backend =
        MockModel::new((0..16).map(|_| answer("Preserve the current goal and recent evidence.")));
    let compacted = autocompact::execute(
        &backend,
        plan,
        &[],
        1_000,
        128_000,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let requests = backend.requests();

    assert!(
        requests.len() > 1,
        "large context should use several fragments"
    );

    let first_tokens = estimate_tokens(&requests[0].messages, &[]);

    assert!(first_tokens <= 4_096, "first summary batch should be small");
    assert!(
        requests[1..]
            .iter()
            .any(|request| estimate_tokens(&request.messages, &[]) > first_tokens),
        "quick successful requests should permit larger later batches"
    );

    for request in requests {
        let ChatMessage::User { content, .. } = &request.messages[1] else {
            panic!("summary request must carry its fragment as user data");
        };

        assert!(serde_json::from_str::<serde_json::Value>(content).is_ok());
        assert!(
            estimate_tokens(&request.messages, &[]) <= 32_768,
            "summary request exceeded its token budget"
        );

        check_budget(
            &request.messages,
            &[],
            request.max_tokens.unwrap() as usize,
            128_000,
        )
        .unwrap();
    }

    assert!(compacted.after_tokens < compacted.before_tokens);
}

#[tokio::test]
async fn invalid_summary_and_cancellation_preserve_history() {
    let history = long_task();
    let original = history.clone();
    let mut truncated = answer("partial");
    truncated.finish_reason = Some("length".into());
    let invalid_tool = ChatCompletion {
        message: call("unexpected"),
        ..answer("")
    };

    for response in [
        answer(""),
        truncated,
        invalid_tool,
        answer(&"expanded ".repeat(10_000)),
    ] {
        let plan = autocompact::plan(&history, &[], 1_000, 8_000).unwrap();
        let result = autocompact::execute(
            &MockModel::new([response]),
            plan,
            &[],
            1_000,
            8_000,
            CancellationToken::new(),
        )
        .await;

        assert!(result.is_err());
        assert_eq!(history, original);
    }

    let token = CancellationToken::new();
    token.cancel();
    let backend = MockModel::new([]);
    let plan = autocompact::plan(&history, &[], 1_000, 8_000).unwrap();

    assert!(
        autocompact::execute(&backend, plan, &[], 1_000, 8_000, token)
            .await
            .is_err()
    );
    assert!(backend.requests().is_empty());
}

#[tokio::test]
async fn invalid_local_summary_gets_one_schema_repair_request() {
    let history = long_task();
    let plan = autocompact::plan(&history, &[], 1_000, 8_000).unwrap();
    let invalid = ChatCompletion {
        message: ChatMessage::assistant("I have reviewed the history."),
        ..answer("")
    };
    let backend = MockModel::new(
        [invalid]
            .into_iter()
            .chain((0..20).map(|_| answer("Continue fixing the scheduler; preserve the API."))),
    );
    let result = autocompact::execute(&backend, plan, &[], 1_000, 8_000, CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(
        result.record.strategy,
        orbit_agent::memory::CompactionStrategy::Summary
    );
    assert!(!result.record.fallback);

    let requests = backend.requests();

    assert!(requests.len() >= 2);
    assert!(matches!(
        &requests[1].messages[0],
        ChatMessage::System { content }
            if content.contains("last response did not satisfy")
    ));
}

#[tokio::test]
async fn truncated_summary_retries_with_a_smaller_source_batch() {
    let mut history = long_task();
    history[3] = ChatMessage::tool("old", "evidence ".repeat(42_000));
    let plan = autocompact::plan(&history, &[], 1_000, 128_000).unwrap();
    let mut truncated = answer("incomplete");
    truncated.finish_reason = Some("length".into());
    let backend = MockModel::new(
        [answer("Keep the scheduler API stable."), truncated]
            .into_iter()
            .chain((0..100).map(|_| answer("Keep the current task and API stable."))),
    );
    let result = autocompact::execute(
        &backend,
        plan,
        &[],
        1_000,
        128_000,
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(
        result.record.strategy,
        orbit_agent::memory::CompactionStrategy::Summary
    );

    let requests = backend.requests();

    assert!(requests.len() > 2);
    assert!(
        estimate_tokens(&requests[2].messages, &[]) < estimate_tokens(&requests[1].messages, &[]),
        "truncated={} retry={}",
        estimate_tokens(&requests[1].messages, &[]),
        estimate_tokens(&requests[2].messages, &[]),
    );
}

#[tokio::test]
async fn automatic_compaction_preserves_history_after_a_retryable_summary_failure() {
    let history = long_task();
    let original = history.clone();
    let plan = autocompact::plan(&history, &[], 1_000, 8_000).unwrap();
    let backend = MockModel::from_results([Err(ModelError::Http {
        status: 429,
        request_id: String::new(),
        body: "rate limited".into(),
        retryable: true,
        context_limit: false,
    })]);
    let result =
        autocompact::execute_automatic(&backend, plan, &[], 1_000, 8_000, CancellationToken::new())
            .await;

    assert!(matches!(
        result,
        Err(orbit_agent::error::AgentError::CompactionFailed {
            kind: orbit_agent::compaction::CompactionFailureKind::Upstream,
            ..
        })
    ));
    assert_eq!(history, original);
    assert_eq!(backend.requests().len(), 1);
}

#[tokio::test]
async fn transient_summary_failure_below_the_emergency_threshold_uses_the_saved_archive() {
    use orbit_agent::{
        compaction::CompactionSettings,
        memory::{CompactionMode, CompactionStrategy},
    };
    let history = prose_task();
    let settings = CompactionSettings::default();
    let before = estimate_tokens(&history, &[]);
    let limit = 1_000 + before * 100 / 88;
    let plan = autocompact::plan_configured(
        &history,
        &[],
        1_000,
        limit,
        CompactionMode::Automatic,
        &settings,
    )
    .unwrap();

    assert!(before < (limit - 1_000) * 92 / 100);
    let backend = MockModel::from_results([Err(ModelError::Http {
        status: 429,
        request_id: String::new(),
        body: "Orbit rate limit".into(),
        retryable: true,
        context_limit: false,
    })]);
    let archive = Archive::default();
    let result = autocompact::execute_configured(
        &backend,
        plan,
        &[],
        1_000,
        limit,
        CancellationToken::new(),
        Some(&archive),
        &settings,
    )
    .await
    .unwrap();

    assert!(result.record.fallback);
    assert_eq!(
        result.record.strategy,
        CompactionStrategy::ArchiveProjection
    );
    assert_eq!(
        archive.0.lock().unwrap().as_slice(),
        std::slice::from_ref(&history)
    );
    check_budget(&result.history, &[], 1_000, limit).unwrap();
}

#[test]
fn never_splits_pending_tool_batches_or_truncates_an_uncompressible_request() {
    let mut history = long_task();
    history.pop();

    assert!(autocompact::plan(&history, &[], 1_000, 8_000).is_none());
    assert!(
        autocompact::plan(
            &[ChatMessage::user("huge ".repeat(20_000))],
            &[],
            1_000,
            8_000
        )
        .is_none()
    );
    assert!(autocompact::plan(&[ChatMessage::user("small")], &[], 1_000, 8_000).is_none());
}

#[tokio::test]
async fn automatic_compaction_runs_between_tool_batches_in_both_execution_paths() {
    for streaming in [false, true] {
        let mut history = long_task();
        history[3] = ChatMessage::tool("old", "x".repeat(12_000));
        history.truncate(4);
        let first = ChatCompletion {
            message: call("new"),
            ..answer("")
        };
        let backend = MockModel::new([first].into_iter().chain(
            (0..12).map(|_| answer("Already inspected old evidence. Continue the exact request.")),
        ));
        let mut tools = ToolRegistry::new();
        tools.register(ClosureTool::new(
            "read",
            "read evidence",
            serde_json::json!({
                "type": "object"
            }),
            |_, _| async { Ok("x".repeat(7_000)) },
        ));
        let agent = Agent::new(
            backend.clone(),
            tools,
            AgentConfig {
                context_tokens: 8_000,
                ..Default::default()
            },
        )
        .with_context_archive(std::sync::Arc::new(Archive::default()));
        let mut request = ChatRequest::new(history.clone());
        request.max_tokens = Some(1_000);
        let result = if streaming {
            let (tx, mut rx) = tokio::sync::mpsc::channel(64);
            let result = agent
                .run_with_agent_events(request, CancellationToken::new(), tx)
                .await
                .unwrap();
            let mut events = Vec::new();

            while let Some(event) = rx.recv().await {
                events.push(event);
            }

            assert_eq!(
                events
                    .iter()
                    .filter(|e| matches!(e, AgentEvent::ContextCompacted { .. }))
                    .count(),
                1
            );

            let completed_tool = events
                .iter()
                .position(|e| matches!(e, AgentEvent::ToolFinished { .. }))
                .unwrap();
            let compacted = events
                .iter()
                .position(|e| matches!(e, AgentEvent::ContextCompacted { .. }))
                .unwrap();

            assert!(completed_tool < compacted);

            result
        } else {
            agent
                .run(request, CancellationToken::new())
                .await
                .unwrap()
        };

        assert_eq!(result.turns, 2);
        assert_eq!(result.tool_calls, 1);
        assert!(backend.requests().len() >= 2);
        assert!(result.history.contains(&history[1]));
        assert!(!result.history.contains(&history[3]));

        assert_paired(&result.history);
    }
}

#[tokio::test]
async fn invalid_early_summary_preserves_context_and_disabled_mode_skips_compaction() {
    let history = prose_task();

    assert!(estimate_tokens(&history, &[]) + 1_000 < 8_000);

    for enabled in [false, true] {
        let backend = MockModel::new(if enabled {
            vec![answer(""), answer(""), answer("Still works")]
        } else {
            vec![answer("Still works")]
        });
        let agent = Agent::new(
            backend.clone(),
            ToolRegistry::new(),
            AgentConfig {
                context_tokens: 8_000,
                auto_compact: enabled,
                ..Default::default()
            },
        )
        .with_context_archive(std::sync::Arc::new(Archive::default()));
        let mut request = ChatRequest::new(history.clone());
        request.max_tokens = Some(1_000);
        let result = agent
            .run(request, CancellationToken::new())
            .await
            .unwrap();
        let requests = backend.requests();

        if enabled {
            assert_eq!(requests.len(), 3);
            assert_eq!(requests[2].messages, history);
            assert_eq!(&result.history[..history.len()], history.as_slice());
        } else {
            assert_eq!(requests.len(), 1);
            assert_eq!(requests[0].messages, history);
        }
    }
}

#[tokio::test]
async fn cancellation_interrupts_a_summary_backend_that_does_not_poll_the_token() {
    use orbit_agent::model::{CompletionFuture, ModelBackend, StreamFuture};

    struct Waiting;

    impl ModelBackend for Waiting {
        fn complete<'a>(&'a self, _: ChatRequest, _: CancellationToken) -> CompletionFuture<'a> {
            Box::pin(std::future::pending())
        }

        fn stream<'a>(&'a self, _: ChatRequest, _: CancellationToken) -> StreamFuture<'a> {
            Box::pin(std::future::pending())
        }
    }

    let token = CancellationToken::new();
    let plan = autocompact::plan(&long_task(), &[], 1_000, 8_000).unwrap();
    let cancel = token.clone();
    let task = tokio::spawn(async move {
        autocompact::execute(&Waiting, plan, &[], 1_000, 8_000, token).await
    });
    tokio::task::yield_now().await;
    cancel.cancel();
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();

    assert!(matches!(
        result,
        Err(orbit_agent::error::AgentError::Cancelled)
    ));
}

#[tokio::test]
async fn failed_summary_is_not_retried_on_every_tool_call() {
    let backend = MockModel::new([
        answer(""),
        answer(""),
        ChatCompletion {
            message: call("next"),
            ..answer("")
        },
        answer("finished"),
    ]);
    let mut tools = ToolRegistry::new();
    tools.register(ClosureTool::new(
        "read",
        "read",
        serde_json::json!({
            "type": "object"
        }),
        |_, _| async { Ok("small result".into()) },
    ));
    let agent = Agent::new(
        backend.clone(),
        tools,
        AgentConfig {
            context_tokens: 8_000,
            ..Default::default()
        },
    )
    .with_context_archive(std::sync::Arc::new(Archive::default()));
    let mut request = ChatRequest::new(prose_task());
    request.max_tokens = Some(1_000);
    let result = agent
        .run(request, CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(result.tool_calls, 1);
    assert_eq!(backend.requests().len(), 4);
    assert_eq!(
        backend
            .requests()
            .iter()
            .filter(|r| matches!(
                r.messages.first(),
                Some(ChatMessage::System { content })
                    if content.starts_with(orbit_agent::context::SUMMARY_PROMPT)
            ))
            .count(),
        2
    );
}

#[tokio::test]
async fn failed_over_limit_summary_uses_retrievable_archive_projection() {
    let mut history = prose_task();
    history[2] = ChatMessage::assistant("x".repeat(30_000));
    let backend = MockModel::new((0..64).map(|_| answer("")));
    let archive = std::sync::Arc::new(Archive::default());
    let agent = Agent::new(
        backend.clone(),
        ToolRegistry::new(),
        AgentConfig {
            context_tokens: 8_000,
            ..Default::default()
        },
    )
    .with_context_archive(archive.clone());
    let mut request = ChatRequest::new(history.clone());
    request.max_tokens = Some(1_000);
    let result = agent
        .run(request, CancellationToken::new())
        .await
        .unwrap();

    assert!(
        archive
            .0
            .lock()
            .unwrap()
            .iter()
            .any(|saved| saved == &history)
    );
    assert!(
        serde_json::to_string(&result.history)
            .unwrap()
            .contains("read_history archive_id=1")
    );
    assert!(backend.requests().len() >= 2);

    for request in backend.requests() {
        check_budget(
            &request.messages,
            &[],
            request.max_tokens.unwrap() as usize,
            8_000,
        )
        .unwrap();
    }
}

#[derive(Default)]
struct Archive(std::sync::Mutex<Vec<Vec<ChatMessage>>>);

impl orbit_agent::archive::ContextArchive for Archive {
    fn save<'a>(&'a self, messages: &'a [ChatMessage]) -> orbit_agent::archive::ArchiveFuture<'a> {
        Box::pin(async move {
            self.0
                .lock()
                .unwrap()
                .push(messages.to_vec());
            Ok("read_history archive_id=1".into())
        })
    }
}

#[derive(Default)]
struct ProgressArchive(std::sync::Mutex<Option<orbit_agent::memory::SummaryProgress>>);

impl orbit_agent::archive::ContextArchive for ProgressArchive {
    fn save<'a>(&'a self, _: &'a [ChatMessage]) -> orbit_agent::archive::ArchiveFuture<'a> {
        Box::pin(async { Ok("read_history archive_id=1".into()) })
    }

    fn load_progress<'a>(&'a self, _: &'a str) -> orbit_agent::archive::ProgressLoadFuture<'a> {
        Box::pin(async move { Ok(self.0.lock().unwrap().clone()) })
    }

    fn save_progress<'a>(
        &'a self,
        progress: &'a orbit_agent::memory::SummaryProgress,
    ) -> orbit_agent::archive::ProgressSaveFuture<'a> {
        Box::pin(async move {
            *self.0.lock().unwrap() = Some(progress.clone());
            Ok(())
        })
    }
}

#[tokio::test]
async fn manual_compaction_archives_when_format_repair_still_fails() {
    use orbit_agent::{
        compaction::CompactionSettings,
        memory::{CompactionMode, CompactionStrategy},
    };

    let history = long_task();
    let settings = CompactionSettings::default();
    let plan = autocompact::plan_configured(
        &history,
        &[],
        1_000,
        8_000,
        CompactionMode::Manual,
        &settings,
    )
    .unwrap();
    let bad = ChatCompletion {
        message: ChatMessage::assistant("A prose summary without structured fields"),
        ..answer("")
    };
    let backend = MockModel::new([bad.clone(), bad]);
    let archive = Archive::default();
    let result = autocompact::execute_configured(
        &backend,
        plan,
        &[],
        1_000,
        8_000,
        CancellationToken::new(),
        Some(&archive),
        &settings,
    )
    .await
    .unwrap();

    assert_eq!(backend.requests().len(), 2);
    assert_eq!(
        result.record.strategy,
        CompactionStrategy::ArchiveProjection
    );
    assert!(result.record.fallback);
    assert!(result.record.archive.is_some());
    assert!(result.after_tokens < result.before_tokens);
    assert_eq!(
        archive.0.lock().unwrap().as_slice(),
        std::slice::from_ref(&history)
    );
}

#[tokio::test]
async fn manual_compaction_keeps_summarizing_after_two_timeouts() {
    use orbit_agent::{
        compaction::CompactionSettings,
        memory::{CompactionMode, CompactionStrategy},
    };

    let history = vec![
        ChatMessage::system("Keep the API stable"),
        ChatMessage::user("Fix the parser ".repeat(1_500)),
        ChatMessage::assistant("Old analysis. ".repeat(3_000)),
        ChatMessage::user("Continue the current task"),
        ChatMessage::assistant("Ready"),
    ];
    let settings = CompactionSettings {
        max_input_tokens: 8_192,
        max_output_tokens: 512,
        request_timeout_seconds: 1,
        total_timeout_seconds: 4,
        manual_request_timeout_seconds: 1,
        manual_total_timeout_seconds: 30,
        ..Default::default()
    };
    let archive = Archive::default();
    let timeout = || ModelError::Timeout {
        duration: std::time::Duration::from_secs(1),
        stage: "summary".into(),
    };
    let backend = MockModel::from_results(
        [Err(timeout()), Err(timeout())]
            .into_iter()
            .chain((0..50).map(|_| Ok(answer("Parser investigation summarized")))),
    );
    let plan = autocompact::plan_configured(
        &history,
        &[],
        1_000,
        16_000,
        CompactionMode::Manual,
        &settings,
    )
    .unwrap();
    let result = autocompact::execute_configured(
        &backend,
        plan,
        &[],
        1_000,
        16_000,
        CancellationToken::new(),
        Some(&archive),
        &settings,
    )
    .await
    .unwrap();

    assert_eq!(result.record.strategy, CompactionStrategy::Summary);
    assert!(!result.record.fallback);
    assert!(result.after_tokens < result.before_tokens);
    assert_eq!(
        archive.0.lock().unwrap().as_slice(),
        std::slice::from_ref(&history)
    );
    assert!(
        result
            .history
            .iter()
            .any(|message| matches!(
                message,
                ChatMessage::User { content, .. } if content == "Continue the current task"
            ))
    );
    assert!(
        result
            .history
            .iter()
            .any(|message| matches!(
                message,
                ChatMessage::User { content, .. }
                    if content.contains("Parser investigation summarized")
            ))
    );

    assert_paired(&result.history);
    let requests = backend.requests();

    assert!(requests.len() >= 3);
    assert!(
        estimate_tokens(&requests[1].messages, &[]) < estimate_tokens(&requests[0].messages, &[])
    );
    assert!(
        estimate_tokens(&requests[2].messages, &[]) < estimate_tokens(&requests[1].messages, &[])
    );
}

#[tokio::test]
async fn manual_compaction_does_not_report_success_without_a_summary() {
    use orbit_agent::{compaction::CompactionSettings, memory::CompactionMode};

    let history = vec![
        ChatMessage::system("Keep the API stable"),
        ChatMessage::user("Fix the parser ".repeat(1_500)),
        ChatMessage::assistant("Old analysis. ".repeat(3_000)),
        ChatMessage::user("Continue the current task"),
        ChatMessage::assistant("Ready"),
    ];
    let settings = CompactionSettings {
        max_input_tokens: 8_192,
        max_output_tokens: 512,
        manual_request_timeout_seconds: 1,
        manual_total_timeout_seconds: 30,
        ..Default::default()
    };
    let archive = Archive::default();
    let backend = MockModel::from_results((0..20).map(|_| {
        Err(ModelError::Timeout {
            duration: std::time::Duration::from_secs(1),
            stage: "summary".into(),
        })
    }));
    let plan = autocompact::plan_configured(
        &history,
        &[],
        1_000,
        16_000,
        CompactionMode::Manual,
        &settings,
    )
    .unwrap();
    let result = autocompact::execute_configured(
        &backend,
        plan,
        &[],
        1_000,
        16_000,
        CancellationToken::new(),
        Some(&archive),
        &settings,
    )
    .await;

    assert!(result.is_err());
    assert_eq!(
        archive.0.lock().unwrap().as_slice(),
        std::slice::from_ref(&history)
    );
    assert!(backend.requests().len() > 2);
}

#[tokio::test]
async fn successful_summary_batches_resume_after_later_provider_failure() {
    let history = vec![
        ChatMessage::system("Keep the API stable"),
        ChatMessage::user("Investigate the parser"),
        ChatMessage::assistant("Important earlier analysis. ".repeat(3_000)),
        ChatMessage::user("Continue the current task"),
        ChatMessage::assistant("Ready"),
    ];
    let archive = ProgressArchive::default();
    let failure = ModelError::Http {
        status: 429,
        request_id: String::new(),
        body: "rate limited".into(),
        retryable: true,
        context_limit: false,
    };
    let first = MockModel::from_results([Ok(answer("First batch accepted")), Err(failure)]);
    let first_result = autocompact::execute_with_archive(
        &first,
        autocompact::plan(&history, &[], 1_000, 8_000).unwrap(),
        &[],
        1_000,
        8_000,
        CancellationToken::new(),
        Some(&archive),
    )
    .await;
    let first_projection = first_result.unwrap();

    assert_eq!(
        first_projection.record.strategy,
        orbit_agent::memory::CompactionStrategy::ArchiveProjection
    );
    assert!(
        first_projection
            .history
            .iter()
            .any(|message| matches!(
                message,
                ChatMessage::User { content, .. }
                    if content.contains("First batch accepted")
                        && content.contains("read_history archive_id=1")
            ))
    );

    let saved = archive
        .0
        .lock()
        .unwrap()
        .clone()
        .unwrap();

    assert!(saved.next_record > 0 || saved.next_byte_offset > 0);
    assert_eq!(saved.summary.goal, "First batch accepted");

    let resumed = MockModel::new((0..50).map(|_| answer("Complete historical state")));
    let result = autocompact::execute_with_archive(
        &resumed,
        autocompact::plan(&history, &[], 1_000, 8_000).unwrap(),
        &[],
        1_000,
        8_000,
        CancellationToken::new(),
        Some(&archive),
    )
    .await
    .unwrap();
    let ChatMessage::User { content, .. } = &resumed.requests()[0].messages[1] else {
        panic!("summary input should be JSON user data");
    };

    let input: serde_json::Value = serde_json::from_str(content).unwrap();

    assert!(
        input["previous_summary"]
            .as_str()
            .unwrap()
            .contains("First batch accepted")
    );
    assert_eq!(
        result.record.strategy,
        orbit_agent::memory::CompactionStrategy::Summary
    );
}

fn tool_heavy_task() -> Vec<ChatMessage> {
    let mut messages = vec![
        ChatMessage::system("Never change the tests"),
        ChatMessage::user("Fix the parser; preserve the public API"),
    ];

    for i in 0..16 {
        messages.push(ChatMessage::assistant(format!("Inspect batch {i}")));
        messages.push(call(&format!("call-{i}")));
        messages.push(ChatMessage::tool(
            format!("call-{i}"),
            format!(
                "{}\nerror: failure-{i}\n{}",
                "log line\n".repeat(2_250),
                "tail line\n".repeat(2_250)
            ),
        ));
    }

    messages
}

#[tokio::test]
async fn tool_heavy_history_reclaims_over_seventy_percent_without_a_model_call() {
    let history = tool_heavy_task();
    let archive = Archive::default();
    let backend = MockModel::new([]);
    let started = std::time::Instant::now();
    let compacted = autocompact::execute_with_archive(
        &backend,
        autocompact::plan(&history, &[], 1_000, 224_000).unwrap(),
        &[],
        1_000,
        224_000,
        CancellationToken::new(),
        Some(&archive),
    )
    .await
    .unwrap();

    println!(
        "tool-heavy replay: {} -> {} estimated tokens; {} model requests; {:?}",
        compacted.before_tokens,
        compacted.after_tokens,
        backend.requests().len(),
        started.elapsed()
    );

    assert!(backend.requests().is_empty());
    assert_eq!(
        compacted.record.strategy,
        orbit_agent::memory::CompactionStrategy::ToolPruning
    );
    assert!(compacted.after_tokens < compacted.before_tokens * 3 / 10);
    assert_eq!(archive.0.lock().unwrap()[0], history);

    assert_paired(&compacted.history);

    assert_eq!(compacted.history.last(), history.last());

    for (before, after) in history.iter().zip(&compacted.history) {
        if matches!(
            before,
            ChatMessage::User { .. } | ChatMessage::System { .. }
        ) {
            assert_eq!(before, after);
        }
    }

    let shortened = serde_json::to_string(&compacted.history).unwrap();

    assert!(shortened.contains("failure-0"));
    assert!(shortened.contains("read_history archive_id=1"));
}

#[tokio::test]
async fn agent_prunes_archived_tools_before_the_summary_threshold() {
    let history = tool_heavy_task();
    let before = estimate_tokens(&history, &[]);
    let context_tokens = before.saturating_mul(100) / 70 + 4_096;

    assert!(before >= (context_tokens - 4_096) * 60 / 100);
    assert!(before < (context_tokens - 4_096) * 85 / 100);

    let backend = MockModel::new([ChatCompletion {
        id: None,
        model: None,
        message: ChatMessage::assistant("Continue"),
        finish_reason: Some("stop".into()),
        usage: None,
    }]);
    let archive = std::sync::Arc::new(Archive::default());
    let agent = Agent::new(
        backend.clone(),
        ToolRegistry::new(),
        AgentConfig {
            context_tokens,
            ..AgentConfig::default()
        },
    )
    .with_context_archive(archive.clone());
    let result = agent
        .run(ChatRequest::new(history.clone()), CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(
        result.history.last(),
        Some(&ChatMessage::assistant("Continue"))
    );
    assert_eq!(
        backend.requests().len(),
        1,
        "no summary model call should occur"
    );
    assert!(estimate_tokens(&backend.requests()[0].messages, &[]) < before.saturating_mul(3) / 4);
    assert_eq!(archive.0.lock().unwrap()[0], history);
}

#[tokio::test]
async fn archived_projection_reduces_summary_work_and_keeps_valid_record_json() {
    let mut history = long_task();
    history[3] = ChatMessage::tool("old", "huge result\n".repeat(50_000));
    // Prose also needs summarizing, so pruning alone cannot reach the target.
    history.insert(
        4,
        ChatMessage::assistant("Important chronological analysis. ".repeat(8_000)),
    );
    let archive = Archive::default();
    let backend = MockModel::new(
        (0..20)
            .map(|_| answer("Continue the exact goal; archived tool evidence can be retrieved.")),
    );
    let compacted = autocompact::execute_with_archive(
        &backend,
        autocompact::plan(&history, &[], 1_000, 224_000).unwrap(),
        &[],
        1_000,
        224_000,
        CancellationToken::new(),
        Some(&archive),
    )
    .await
    .unwrap();

    assert_eq!(
        compacted.record.strategy,
        orbit_agent::memory::CompactionStrategy::Summary
    );
    assert!(compacted.record.summary_input_tokens < 120_000);

    for request in backend.requests() {
        let ChatMessage::User { content, .. } = &request.messages[1] else {
            panic!()
        };

        let data: serde_json::Value = serde_json::from_str(content).unwrap();

        assert!(data["records"].is_array());

        for record in data["records"].as_array().unwrap() {
            assert!(record["source_message"].is_number());
        }
    }

    assert_eq!(archive.0.lock().unwrap()[0], history);

    assert_paired(&compacted.history);
}

#[tokio::test]
async fn archive_failure_prevents_any_lossy_projection_or_model_request() {
    struct Broken;

    impl orbit_agent::archive::ContextArchive for Broken {
        fn save<'a>(&'a self, _: &'a [ChatMessage]) -> orbit_agent::archive::ArchiveFuture<'a> {
            Box::pin(async {
                Err(orbit_agent::error::AgentError::Compaction(
                    "disk full".into(),
                ))
            })
        }
    }

    let history = tool_heavy_task();
    let backend = MockModel::new([]);

    assert!(
        autocompact::execute_with_archive(
            &backend,
            autocompact::plan(&history, &[], 1_000, 224_000).unwrap(),
            &[],
            1_000,
            224_000,
            CancellationToken::new(),
            Some(&Broken)
        )
        .await
        .is_err()
    );
    assert!(backend.requests().is_empty());
}

#[tokio::test]
async fn manual_compaction_handles_a_single_long_task_with_the_same_budget() {
    let history = long_task();
    let compacted = orbit_agent::context::compact_with_archive(
        &MockModel::new((0..12).map(|_| answer("Continue scheduler fix"))),
        history,
        &[],
        8_000,
        1_000,
        CancellationToken::new(),
        &Default::default(),
        None,
    )
    .await
    .unwrap();

    assert_eq!(
        compacted.record.mode,
        orbit_agent::memory::CompactionMode::Manual
    );

    assert_paired(&compacted.history);
}

/// Opt-in local replay. Never calls a provider or modifies the user's database.
#[tokio::test]
#[ignore = "set ORBIT_REPLAY_HISTORY to a locally exported ChatMessage array"]
async fn replay_archived_tool_pruning() {
    let path = std::env::var("ORBIT_REPLAY_HISTORY").unwrap();
    let history: Vec<ChatMessage> = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let archive = Archive::default();
    let backend = MockModel::new([]);
    let started = std::time::Instant::now();
    let compacted = autocompact::execute_with_archive(
        &backend,
        autocompact::plan(&history, &[], 0, 224_000).unwrap(),
        &[],
        0,
        224_000,
        CancellationToken::new(),
        Some(&archive),
    )
    .await
    .unwrap();

    assert_eq!(
        compacted.record.strategy,
        orbit_agent::memory::CompactionStrategy::ToolPruning
    );
    assert_eq!(archive.0.lock().unwrap()[0], history);

    assert_paired(&compacted.history);
    println!(
        "local replay: {} -> {} estimated tokens; {} model requests; {:?}",
        compacted.before_tokens,
        compacted.after_tokens,
        backend.requests().len(),
        started.elapsed()
    );
}

#[tokio::test]
#[ignore = "set ORBIT_REPLAY_HISTORY to a locally exported ChatMessage array"]
async fn replay_manual_compaction_when_summary_is_unavailable() {
    use orbit_agent::{compaction::CompactionSettings, memory::CompactionMode};

    let path = std::env::var("ORBIT_REPLAY_HISTORY").unwrap();
    let history: Vec<ChatMessage> = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let settings = CompactionSettings::default();
    let archive = Archive::default();
    let timeout = || ModelError::Timeout {
        duration: std::time::Duration::from_secs(1),
        stage: "summary".into(),
    };
    let backend = MockModel::from_results((0..20).map(|_| Err(timeout())));
    let plan = autocompact::plan_configured(
        &history,
        &[],
        64_000,
        288_000,
        CompactionMode::Manual,
        &settings,
    )
    .unwrap();
    let result = autocompact::execute_configured(
        &backend,
        plan,
        &[],
        64_000,
        288_000,
        CancellationToken::new(),
        Some(&archive),
        &settings,
    )
    .await;

    assert!(result.is_err());

    println!(
        "local replay: manual summary did not complete after {} model requests",
        backend.requests().len()
    );
}

#[tokio::test]
async fn repeated_compaction_carries_prior_constraints_errors_and_verification_as_source() {
    use orbit_agent::memory::TaskSummary;

    let summary = TaskSummary {
        goal: "fix parser".into(),
        constraints: vec!["keep API parse_v2 unchanged".into()],
        decisions: vec!["use borrowed tokens".into()],
        completed: vec![],
        remaining: vec!["empty input panic unresolved".into()],
        verification: vec!["cargo test parser failed exit 101".into()],
        uncertainty: vec!["full suite not run".into()],
    };
    let response = ChatCompletion {
        id: None,
        model: None,
        message: ChatMessage::assistant(serde_json::to_string(&summary).unwrap()),
        finish_reason: Some("stop".into()),
        usage: None,
    };
    let backend = MockModel::new(vec![response.clone(); 40]);
    let original = vec![
        ChatMessage::system("Never change user files without task authorization"),
        ChatMessage::user("fix parser, preserve parse_v2"),
        ChatMessage::assistant("investigation ".repeat(8_000)),
        ChatMessage::user("continue"),
        ChatMessage::assistant("recent"),
    ];
    let plan = autocompact::plan_manual(&original, &[], 4_096, 64_000).unwrap();
    let first = autocompact::execute(&backend, plan, &[], 4_096, 64_000, CancellationToken::new())
        .await
        .unwrap();
    let previous_requests = backend.requests().len();
    let mut history = first.history;
    history.push(ChatMessage::user("continue and validate"));
    history.push(ChatMessage::assistant("new investigation ".repeat(8_000)));
    history.push(ChatMessage::user("finish"));
    history.push(ChatMessage::assistant("working"));
    let plan = autocompact::plan_manual(&history, &[], 4_096, 64_000).unwrap();
    let second = autocompact::execute(&backend, plan, &[], 4_096, 64_000, CancellationToken::new())
        .await
        .unwrap();

    for fact in [
        &summary.constraints[0],
        &summary.remaining[0],
        &summary.verification[0],
    ] {
        assert!(
            backend.requests()[previous_requests..]
                .iter()
                .any(|r| serde_json::to_string(&r.messages)
                    .unwrap()
                    .contains(fact))
        );
        assert!(second.summary.contains(fact));
    }

    assert_paired(&second.history);

    assert!(matches!(
        &second.history[0],
        ChatMessage::System { content } if content.contains("Never change")
    ));
}
