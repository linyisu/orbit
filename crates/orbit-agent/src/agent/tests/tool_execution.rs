use super::*;
use crate::tool::ToolCapabilities;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn batch(calls: Vec<ToolCall>) -> ChatCompletion {
    ChatCompletion {
        message: ChatMessage::assistant_tool_calls(calls),
        ..assistant_text("")
    }
}

fn call(id: usize, name: &str) -> ToolCall {
    ToolCall::function(id.to_string(), name, json!({"index":id}))
}

#[tokio::test]
async fn reads_overlap_with_a_bound_and_preserve_write_barriers_and_result_order() {
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let phase = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(tokio::sync::Barrier::new(4));
    let mut tools = ToolRegistry::new();
    let (a, p, stage) = (active.clone(), peak.clone(), phase.clone());
    tools.register(
        ClosureTool::new("observe", "", json!({}), move |args, _| {
            let (a, p, stage, barrier) = (a.clone(), p.clone(), stage.clone(), barrier.clone());
            async move {
                let index = args["index"].as_u64().unwrap();
                assert_eq!(stage.load(Ordering::SeqCst), usize::from(index > 4));
                let count = a.fetch_add(1, Ordering::SeqCst) + 1;
                p.fetch_max(count, Ordering::SeqCst);
                barrier.wait().await;
                if index.is_multiple_of(4) {
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                }
                a.fetch_sub(1, Ordering::SeqCst);
                Ok(index.to_string())
            }
        })
        .with_capabilities(ToolCapabilities::READ),
    );
    let (a, stage) = (active.clone(), phase.clone());
    // An unclassified extension is an exclusive barrier too.
    tools.register(ClosureTool::new("mutate", "", json!({}), move |_, _| {
        assert_eq!(a.load(Ordering::SeqCst), 0);
        stage.store(1, Ordering::SeqCst);
        async { Ok("written".into()) }
    }));
    let mut calls: Vec<_> = (0..4).map(|i| call(i, "observe")).collect();
    calls.push(call(4, "mutate"));
    // Eight contiguous reads exercise the concurrency limit, not just overlap.
    calls.extend((5..13).map(|i| call(i, "observe")));
    let agent = Agent::new(
        MockModel::new(vec![batch(calls), assistant_text("done")]),
        tools,
        AgentConfig::default(),
    );
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        agent.run(request(), CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(peak.load(Ordering::SeqCst), 4);
    let ids: Vec<_> = result
        .history
        .iter()
        .filter_map(|m| match m {
            ChatMessage::Tool { tool_call_id, .. } => Some(tool_call_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(ids, (0..13).map(|i| i.to_string()).collect::<Vec<_>>());
}

#[tokio::test]
async fn failed_parallel_receipt_preserves_all_uncertain_calls_and_stops_before_write() {
    let writes = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::new();
    tools.register(
        ClosureTool::new("observe", "", json!({}), |_, _| async { Ok("read".into()) })
            .with_capabilities(ToolCapabilities::READ),
    );
    let w = writes.clone();
    tools.register(
        ClosureTool::new("mutate", "", json!({}), move |_, _| {
            w.fetch_add(1, Ordering::SeqCst);
            async { Ok("written".into()) }
        })
        .with_capabilities(ToolCapabilities::WRITE),
    );
    let journal = Arc::new(FailingJournal {
        checkpoints: Default::default(),
        fail_start: false,
        fail_receipt: true,
    });
    let agent = Agent::new(
        MockModel::new(vec![batch(vec![
            call(0, "observe"),
            call(1, "observe"),
            call(2, "mutate"),
        ])]),
        tools,
        AgentConfig::default(),
    )
    .with_execution_journal(journal.clone());
    assert!(matches!(
        agent.run(request(), CancellationToken::new()).await,
        Err(AgentError::Checkpoint(_))
    ));
    assert_eq!(writes.load(Ordering::SeqCst), 0);
    let checkpoints = journal.checkpoints.lock().unwrap();
    let checkpoint = checkpoints.last().unwrap();
    assert_eq!(checkpoint.in_flight, ["0", "1"]);
    let (history, unresolved) = checkpoint.recovered_history();
    assert_eq!(unresolved, 3);
    for m in &history {
        if let ChatMessage::Tool {
            tool_call_id,
            content,
        } = m
        {
            assert!(content.contains(if tool_call_id == "2" {
                "NOT executed"
            } else {
                "UNKNOWN"
            }));
        }
    }
    // A saved serial checkpoint must remain recoverable after an upgrade.
    let mut saved = serde_json::to_value(checkpoint).unwrap();
    saved["in_flight"] = json!("0");
    let restored: crate::checkpoint::ExecutionCheckpoint = serde_json::from_value(saved).unwrap();
    assert_eq!(restored.in_flight, ["0"]);
}

#[tokio::test]
async fn cancelling_pending_reads_drops_the_batch_without_starting_the_next_barrier() {
    let started = Arc::new(tokio::sync::Notify::new());
    let count = Arc::new(AtomicUsize::new(0));
    let writes = Arc::new(AtomicUsize::new(0));
    let mut tools = ToolRegistry::new();
    let (signal, c) = (started.clone(), count.clone());
    tools.register(
        ClosureTool::new("observe", "", json!({}), move |_, _| {
            let (signal, c) = (signal.clone(), c.clone());
            async move {
                if c.fetch_add(1, Ordering::SeqCst) == 1 {
                    signal.notify_one();
                }
                std::future::pending::<Result<String, ToolError>>().await
            }
        })
        .with_capabilities(ToolCapabilities::READ),
    );
    let w = writes.clone();
    tools.register(ClosureTool::new("mutate", "", json!({}), move |_, _| {
        w.fetch_add(1, Ordering::SeqCst);
        async { Ok("written".into()) }
    }));
    let agent = Agent::new(
        MockModel::new(vec![batch(vec![
            call(0, "observe"),
            call(1, "observe"),
            call(2, "mutate"),
        ])]),
        tools,
        AgentConfig::default(),
    );
    let token = CancellationToken::new();
    let cancel = token.clone();
    let (_, result) = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        tokio::join!(
            async {
                started.notified().await;
                cancel.cancel();
            },
            agent.run(request(), token)
        )
    })
    .await
    .unwrap();
    assert!(matches!(result, Err(AgentError::Cancelled)));
    assert_eq!(count.load(Ordering::SeqCst), 2);
    assert_eq!(writes.load(Ordering::SeqCst), 0);
}
