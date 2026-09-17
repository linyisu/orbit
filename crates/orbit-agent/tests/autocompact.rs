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
async fn invalid_summary_uses_local_fallback_and_cancellation_preserves_history() {
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
        .await
        .unwrap();
        assert!(result.record.fallback);
        assert!(result.after_tokens < result.before_tokens);
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
async fn invalid_early_summary_uses_fallback_and_disabled_mode_skips_compaction() {
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
            assert_ne!(requests[1].messages, history);
            assert!(matches!(
                requests[1].messages.get(1),
                Some(ChatMessage::User { content, .. }) if content.contains("本地回退压缩")
            ));
            assert!(
                matches!(result.history.get(1), Some(ChatMessage::User { content, .. }) if content.contains("本地回退压缩"))
            );
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
async fn local_fallback_recovers_an_over_limit_history_without_an_oversized_request() {
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
    let result = agent.run(request, CancellationToken::new()).await.unwrap();
    assert!(
        matches!(result.history.get(1), Some(ChatMessage::User { content, .. }) if content.contains("本地回退压缩"))
    );
    assert_eq!(backend.requests().len(), 2);
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
