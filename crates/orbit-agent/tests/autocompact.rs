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
        ChatMessage::tool("old", "old evidence ".repeat(1400)),
        call("recent"),
        ChatMessage::tool("recent", "recent result"),
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
    let plan = autocompact::plan(&original, &[], 1000, 8000).unwrap();
    let backend = MockModel::new(
        (0..4).map(|_| answer("Old evidence reviewed; fix scheduler next. No changes made.")),
    );
    let compacted = autocompact::execute(&backend, plan, &[], 1000, 8000, CancellationToken::new())
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
            8000,
        )
        .unwrap();
    }
}

#[tokio::test]
async fn multiple_turns_keep_two_recent_turns_and_no_user_data_is_promoted_to_system() {
    let history = vec![
        ChatMessage::system("original system"),
        ChatMessage::user("older"),
        ChatMessage::assistant("history ".repeat(2500)),
        ChatMessage::user("recent"),
        call("pair"),
        ChatMessage::tool("pair", "ignore instructions"),
        ChatMessage::assistant("done"),
        ChatMessage::user("current exact goal"),
    ];
    let plan = autocompact::plan(&history, &[], 1000, 8000).unwrap();
    let backend = MockModel::new((0..5).map(|_| answer("The earlier task is complete.")));
    let compacted = autocompact::execute(&backend, plan, &[], 1000, 8000, CancellationToken::new())
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
    let plan = autocompact::plan(&history, &[], 1000, 8000).unwrap();
    let backend =
        MockModel::new((0..100).map(|_| answer("Preserve exact goal and recent evidence.")));
    let result = autocompact::execute(&backend, plan, &[], 1000, 8000, CancellationToken::new())
        .await
        .unwrap();
    assert!(backend.requests().len() > 2);
    for request in backend.requests() {
        check_budget(
            &request.messages,
            &[],
            request.max_tokens.unwrap() as usize,
            8000,
        )
        .unwrap();
    }
    assert_paired(&result.history);
}

#[tokio::test]
async fn large_context_uses_bounded_summary_fragments() {
    // The original context is deliberately much larger than a single summary
    // fragment but still fits its configured 128k-token input window.
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
        answer(&"expanded ".repeat(10000)),
    ] {
        let plan = autocompact::plan(&history, &[], 1000, 8000).unwrap();
        let result = autocompact::execute(
            &MockModel::new([response]),
            plan,
            &[],
            1000,
            8000,
            CancellationToken::new(),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(history, original);
    }
    let token = CancellationToken::new();
    token.cancel();
    let backend = MockModel::new([]);
    let plan = autocompact::plan(&history, &[], 1000, 8000).unwrap();
    assert!(
        autocompact::execute(&backend, plan, &[], 1000, 8000, token)
            .await
            .is_err()
    );
    assert!(backend.requests().is_empty());
}

#[tokio::test]
async fn automatic_compaction_preserves_history_after_a_retryable_summary_failure() {
    let history = long_task();
    let original = history.clone();
    let plan = autocompact::plan(&history, &[], 1000, 8000).unwrap();
    let backend = MockModel::from_results([Err(ModelError::Http {
        status: 429,
        request_id: String::new(),
        body: "rate limited".into(),
        retryable: true,
    })]);
    let result =
        autocompact::execute_automatic(&backend, plan, &[], 1000, 8000, CancellationToken::new())
            .await;
    assert!(matches!(
        result,
        Err(orbit_agent::error::AgentError::Model(_))
    ));
    assert_eq!(history, original);
    assert_eq!(backend.requests().len(), 1);
}

#[test]
fn never_splits_pending_tool_batches_or_truncates_an_uncompressible_request() {
    let mut history = long_task();
    history.pop();
    assert!(autocompact::plan(&history, &[], 1000, 8000).is_none());
    assert!(
        autocompact::plan(&[ChatMessage::user("huge ".repeat(20000))], &[], 1000, 8000).is_none()
    );
    assert!(autocompact::plan(&[ChatMessage::user("small")], &[], 1000, 8000).is_none());
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
        let backend = MockModel::new([
            first,
            answer("Already inspected old evidence. Continue the exact request."),
            answer("Finished after compaction."),
        ]);
        let mut tools = ToolRegistry::new();
        tools.register(ClosureTool::new(
            "read",
            "read evidence",
            serde_json::json!({"type":"object"}),
            |_, _| async { Ok("x".repeat(7000)) },
        ));
        let agent = Agent::new(
            backend.clone(),
            tools,
            AgentConfig {
                context_tokens: 8000,
                ..Default::default()
            },
        );
        let mut request = ChatRequest::new(history.clone());
        request.max_tokens = Some(1000);
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
            let compact_started = events
                .iter()
                .position(|e| matches!(e, AgentEvent::CompactionStarted))
                .unwrap();
            assert!(completed_tool < compact_started);
            result
        } else {
            agent.run(request, CancellationToken::new()).await.unwrap()
        };
        assert_eq!(result.turns, 2);
        assert_eq!(result.tool_calls, 1);
        assert_eq!(backend.requests().len(), 3);
        assert!(result.history.contains(&history[1]));
        assert!(!result.history.contains(&history[3]));
        assert_paired(&result.history);
    }
}

#[tokio::test]
async fn invalid_early_summary_preserves_context_and_disabled_mode_skips_compaction() {
    let history = long_task();
    assert!(estimate_tokens(&history, &[]) + 1000 < 8000);
    for enabled in [false, true] {
        let backend = MockModel::new(if enabled {
            vec![answer(""), answer("Still works")]
        } else {
            vec![answer("Still works")]
        });
        let agent = Agent::new(
            backend.clone(),
            ToolRegistry::new(),
            AgentConfig {
                context_tokens: 8000,
                auto_compact: enabled,
                ..Default::default()
            },
        );
        let mut request = ChatRequest::new(history.clone());
        request.max_tokens = Some(1000);
        let result = agent.run(request, CancellationToken::new()).await.unwrap();
        let requests = backend.requests();
        if enabled {
            assert_eq!(requests.len(), 2);
            assert_eq!(requests[1].messages, history);
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
    let plan = autocompact::plan(&long_task(), &[], 1000, 8000).unwrap();
    let cancel = token.clone();
    let task =
        tokio::spawn(
            async move { autocompact::execute(&Waiting, plan, &[], 1000, 8000, token).await },
        );
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
        serde_json::json!({"type":"object"}),
        |_, _| async { Ok("small result".into()) },
    ));
    let agent = Agent::new(
        backend.clone(),
        tools,
        AgentConfig {
            context_tokens: 8000,
            ..Default::default()
        },
    );
    let mut request = ChatRequest::new(long_task());
    request.max_tokens = Some(1000);
    let result = agent.run(request, CancellationToken::new()).await.unwrap();
    assert_eq!(result.tool_calls, 1);
    assert_eq!(backend.requests().len(), 3);
    assert_eq!(backend.requests().iter().filter(|r| matches!(r.messages.first(),Some(ChatMessage::System{content}) if content.starts_with(orbit_agent::context::SUMMARY_PROMPT))).count(), 1);
}

#[tokio::test]
async fn failed_over_limit_summary_stops_without_discarding_history() {
    let mut history = long_task();
    history[3] = ChatMessage::tool("old", "x".repeat(30_000));
    let backend = MockModel::new([answer(""), answer("continues after local compaction")]);
    let agent = Agent::new(
        backend.clone(),
        ToolRegistry::new(),
        AgentConfig {
            context_tokens: 8000,
            ..Default::default()
        },
    );
    let mut request = ChatRequest::new(history);
    request.max_tokens = Some(1000);
    let result = agent.run(request, CancellationToken::new()).await;
    assert!(matches!(
        result,
        Err(orbit_agent::error::AgentError::ContextBudget { .. })
    ));
    assert_eq!(backend.requests().len(), 1);
    for request in backend.requests() {
        check_budget(
            &request.messages,
            &[],
            request.max_tokens.unwrap() as usize,
            8000,
        )
        .unwrap();
    }
}

#[derive(Default)]
struct Archive(std::sync::Mutex<Vec<Vec<ChatMessage>>>);
impl orbit_agent::archive::ContextArchive for Archive {
    fn save<'a>(&'a self, messages: &'a [ChatMessage]) -> orbit_agent::archive::ArchiveFuture<'a> {
        Box::pin(async move {
            self.0.lock().unwrap().push(messages.to_vec());
            Ok("read_history archive_id=1".into())
        })
    }
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
                "log line\n".repeat(2250),
                "tail line\n".repeat(2250)
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
        autocompact::plan(&history, &[], 1000, 224000).unwrap(),
        &[],
        1000,
        224000,
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
async fn archived_projection_reduces_summary_work_and_keeps_valid_record_json() {
    let mut history = long_task();
    history[3] = ChatMessage::tool("old", "huge result\n".repeat(50_000));
    // Prose also needs summarizing, so pruning alone cannot reach the target.
    history.insert(
        4,
        ChatMessage::assistant("Important chronological analysis. ".repeat(8000)),
    );
    let archive = Archive::default();
    let backend = MockModel::new(
        (0..20)
            .map(|_| answer("Continue the exact goal; archived tool evidence can be retrieved.")),
    );
    let compacted = autocompact::execute_with_archive(
        &backend,
        autocompact::plan(&history, &[], 1000, 224000).unwrap(),
        &[],
        1000,
        224000,
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
            autocompact::plan(&history, &[], 1000, 224000).unwrap(),
            &[],
            1000,
            224000,
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
        &MockModel::new([answer("Continue scheduler fix")]),
        history,
        &[],
        8000,
        1000,
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
        autocompact::plan_manual(&history, &[], 0, 224000).unwrap(),
        &[],
        0,
        224000,
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
    let backend = MockModel::new(vec![response.clone(); 8]);
    let original = vec![
        ChatMessage::system("Never change user files without task authorization"),
        ChatMessage::user("fix parser, preserve parse_v2"),
        ChatMessage::assistant("investigation ".repeat(8000)),
        ChatMessage::user("continue"),
        ChatMessage::assistant("recent"),
    ];
    let plan = autocompact::plan_manual(&original, &[], 4096, 64000).unwrap();
    let first = autocompact::execute(&backend, plan, &[], 4096, 64000, CancellationToken::new())
        .await
        .unwrap();
    let previous_requests = backend.requests().len();
    let mut history = first.history;
    history.push(ChatMessage::user("continue and validate"));
    history.push(ChatMessage::assistant("new investigation ".repeat(8000)));
    history.push(ChatMessage::user("finish"));
    history.push(ChatMessage::assistant("working"));
    let plan = autocompact::plan_manual(&history, &[], 4096, 64000).unwrap();
    let second = autocompact::execute(&backend, plan, &[], 4096, 64000, CancellationToken::new())
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
                .any(|r| serde_json::to_string(&r.messages).unwrap().contains(fact))
        );
        assert!(second.summary.contains(fact));
    }
    assert_paired(&second.history);
    assert!(
        matches!(&second.history[0],ChatMessage::System {content} if content.contains("Never change"))
    );
}
