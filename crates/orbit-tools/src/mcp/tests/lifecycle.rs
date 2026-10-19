use super::*;

#[tokio::test]
async fn discovery_reads_all_pages_and_rejects_ambiguous_catalogs() {
    let (_directory, client) = peer("pages").await;
    assert_eq!(
        client
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["work", "second"]
    );
    for mode in ["cursor_loop", "duplicates"] {
        let directory = tempfile::tempdir().unwrap();
        assert!(
            StdioClient::start("synthetic", &config(mode), directory.path())
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn structured_results_survive_and_catalog_changes_block_stale_calls() {
    let (_directory, mut client) = peer("structured").await;
    let output = client
        .call_tool("work", json!({}), CancellationToken::new())
        .await
        .unwrap();
    let content: Value = serde_json::from_str(&output.content).unwrap();
    assert_eq!(content["structured_content"]["count"], 3);
    let (directory, mut client) = peer("list_changed").await;
    client
        .call_tool("work", json!({}), CancellationToken::new())
        .await
        .unwrap();
    let error = client
        .call_tool("work", json!({}), CancellationToken::new())
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("catalog changed")
    );
    assert_eq!(
        messages(directory.path())
            .iter()
            .filter(|entry| entry["method"] == "tools/call")
            .count(),
        1
    );
}

async fn peer(mode: &str) -> (tempfile::TempDir, StdioClient) {
    let directory = tempfile::tempdir().unwrap();
    let config = config(mode);
    let (client, _) = StdioClient::start("synthetic", &config, directory.path())
        .await
        .unwrap();
    (directory, client)
}

pub(super) fn config(mode: &str) -> McpServerConfig {
    static PEER: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    let peer = PEER.get_or_init(|| {
        orbit_build::build_test_peer(Path::new(env!("CARGO_MANIFEST_DIR")), "orbit-mcp-test-peer")
    });
    serde_json::from_value(json!({
        "command": peer,
        "args": [mode],
        "timeout_seconds": 1,
        "enabled": true
    }))
    .unwrap()
}

fn messages(directory: &Path) -> Vec<Value> {
    std::fs::read_to_string(directory.join("requests.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

async fn wait_for_call(directory: &Path) {
    time::timeout(Duration::from_secs(2), async {
        loop {
            if messages(directory)
                .iter()
                .any(|message| message["method"] == "tools/call")
                && directory.join("child.pid").exists()
            {
                break;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[cfg(target_os = "linux")]
async fn assert_descendant_stopped(directory: &Path) {
    let pid = std::fs::read_to_string(directory.join("child.pid")).unwrap();
    time::timeout(Duration::from_secs(2), async {
        loop {
            let status = std::fs::read_to_string(format!("/proc/{pid}/stat"));
            if status.is_err()
                || status
                    .unwrap()
                    .split_whitespace()
                    .nth(2)
                    == Some("Z")
            {
                break;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("owned descendant must no longer run");
}

#[tokio::test]
async fn timeout_cancels_reaps_and_reconnects_without_replaying() {
    let (directory, mut client) = peer("timeout").await;
    let error = client
        .call_tool("work", json!({}), CancellationToken::new())
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("timeout_seconds"));
    assert!(
        client
            ._child
            .try_wait()
            .unwrap()
            .is_some()
    );
    let log = messages(directory.path());
    assert!(log.iter().any(|message| {
        message["method"] == "notifications/cancelled" && message["params"]["requestId"] == 3
    }));
    #[cfg(target_os = "linux")]
    assert_descendant_stopped(directory.path()).await;

    let result = client
        .call_tool("work", json!({}), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&result.content).unwrap()["connection_restarted"],
        true
    );
    assert_eq!(
        std::fs::read_to_string(directory.path().join("starts")).unwrap(),
        "2"
    );
    assert_eq!(
        messages(directory.path())
            .iter()
            .filter(|message| message["method"] == "tools/call")
            .count(),
        2
    );
}

#[tokio::test]
async fn registered_tool_preserves_business_failure_without_restarting_the_peer() {
    use orbit_agent::tool::ToolOutcome;

    let directory = tempfile::tempdir().unwrap();
    let (approvals, mut receiver) = crate::approval::channel();
    let approval = tokio::spawn(async move {
        for _ in 0..2 {
            receiver
                .recv()
                .await
                .unwrap()
                .decision
                .send(true)
                .unwrap();
        }
    });
    let mut registry = ToolRegistry::new();
    let statuses = register(
        &mut registry,
        &McpConfig {
            servers: BTreeMap::from([("synthetic".into(), config("tool_error"))]),
        },
        directory.path(),
        Some(approvals),
        true,
    )
    .await;
    assert!(statuses[0].connected);
    for (index, expected) in [ToolOutcome::Failed, ToolOutcome::Completed]
        .into_iter()
        .enumerate()
    {
        let output = registry
            .execute(
                ToolCall::function(format!("call-{index}"), "MCP_Synthetic_Work", json!({})),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(output.outcome, expected);
        let content: Value = serde_json::from_str(&output.content).unwrap();
        assert_eq!(content["is_error"], index == 0);
        assert_eq!(content["connection_restarted"], false);
    }
    approval.await.unwrap();
    assert_eq!(
        std::fs::read_to_string(directory.path().join("starts")).unwrap(),
        "1"
    );
}

#[tokio::test]
async fn server_errors_do_not_destroy_a_healthy_connection() {
    for mode in ["rpc_error", "tool_error"] {
        let (directory, mut client) = peer(mode).await;
        let result = client
            .call_tool("work", json!({}), CancellationToken::new())
            .await;
        if mode == "rpc_error" {
            assert!(
                result
                    .unwrap_err()
                    .to_string()
                    .contains("busy")
            );
        } else {
            assert_eq!(result.unwrap().outcome, ToolOutcome::Failed);
        }
        assert!(!client.poisoned);
        let result = client
            .call_tool("work", json!({}), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&result.content).unwrap()["connection_restarted"],
            false
        );
        assert_eq!(
            std::fs::read_to_string(directory.path().join("starts")).unwrap(),
            "1"
        );
    }
}

#[tokio::test]
async fn broken_frames_and_eof_recover_on_a_fresh_transport() {
    for mode in ["partial", "malformed", "exit"] {
        let (_directory, mut client) = peer(mode).await;
        assert!(
            client
                .call_tool("work", json!({}), CancellationToken::new())
                .await
                .is_err()
        );
        #[cfg(target_os = "linux")]
        assert_descendant_stopped(_directory.path()).await;
        let result = client
            .call_tool("work", json!({}), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&result.content).unwrap()["connection_restarted"],
            true
        );
        assert!(client.pending_line.is_empty());
    }
}

#[tokio::test]
async fn cancellation_notifies_server_and_does_not_start_another_call() {
    let (directory, mut client) = peer("timeout").await;
    let token = CancellationToken::new();
    let call = client.call_tool("work", json!({}), token.clone());
    let cancel = async {
        wait_for_call(directory.path()).await;
        token.cancel();
    };
    let (result, ()) = tokio::join!(call, cancel);
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("cancelled")
    );
    let result = client
        .call_tool("work", json!({}), token)
        .await;
    assert!(result.is_err());
    assert_eq!(
        std::fs::read_to_string(directory.path().join("starts")).unwrap(),
        "1"
    );
    assert!(
        messages(directory.path())
            .iter()
            .any(|message| message["method"] == "notifications/cancelled")
    );
    #[cfg(target_os = "linux")]
    assert_descendant_stopped(directory.path()).await;
}

#[tokio::test]
async fn initialization_timeout_never_sends_a_cancellation_notification() {
    let directory = tempfile::tempdir().unwrap();
    let result = StdioClient::start("synthetic", &config("init_timeout"), directory.path()).await;
    assert!(result.is_err());
    assert!(
        !messages(directory.path())
            .iter()
            .any(|message| message["method"] == "notifications/cancelled")
    );
}

#[tokio::test]
async fn timeout_covers_blocked_stdin_writes() {
    let (_directory, mut client) = peer("blocked_write").await;
    let call = client.call_tool(
        "work",
        json!({"data": "x".repeat(MAX_MESSAGE_BYTES / 2)}),
        CancellationToken::new(),
    );
    let error = time::timeout(Duration::from_secs(6), call)
        .await
        .unwrap()
        .unwrap_err();
    assert!(format!("{error:#}").contains("timed out"));
    assert!(
        client
            ._child
            .try_wait()
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn reconnect_rejects_a_changed_tool_contract() {
    let (directory, mut client) = peer("changed").await;
    assert!(
        client
            .call_tool("work", json!({}), CancellationToken::new())
            .await
            .is_err()
    );
    let error = client
        .call_tool("work", json!({}), CancellationToken::new())
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("tool changed")
    );
    assert_eq!(
        messages(directory.path())
            .iter()
            .filter(|message| message["method"] == "tools/call")
            .count(),
        1
    );
}

#[tokio::test]
async fn server_request_ids_are_independent_from_client_response_ids() {
    let (_directory, mut client) = peer("collision").await;
    client
        .call_tool("work", json!({}), CancellationToken::new())
        .await
        .unwrap();
    assert!(!client.poisoned);
}

#[tokio::test]
async fn dropping_a_client_kills_its_owned_descendants() {
    let (directory, mut client) = peer("timeout").await;
    let call = client.call_tool("work", json!({}), CancellationToken::new());
    tokio::select! {
        result = call => panic!("unexpected completion: {result:?}"),
        () = wait_for_call(directory.path()) => (),
    }
    drop(client);
    #[cfg(target_os = "linux")]
    assert_descendant_stopped(directory.path()).await;
}

#[tokio::test]
async fn oversized_local_arguments_leave_the_connection_usable() {
    let (directory, mut client) = peer("normal").await;
    let error = client
        .call_tool(
            "work",
            json!({"data": "x".repeat(MAX_MESSAGE_BYTES)}),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("4 MiB"));
    assert!(!client.poisoned);
    client
        .call_tool("work", json!({}), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(directory.path().join("starts")).unwrap(),
        "1"
    );
}

#[tokio::test]
async fn cancellation_while_waiting_for_the_connection_returns_without_a_request() {
    let (directory, client) = peer("normal").await;
    let definition = client.tools[0].clone();
    let client = Arc::new(Mutex::new(client));
    let _lock = client.lock().await;
    let (approvals, mut receiver) = crate::approval::channel();
    let tool = McpTool {
        exposed_name: "MCP_Synthetic_Work".into(),
        server: "synthetic".into(),
        definition,
        client: client.clone(),
        approvals,
    };
    let token = CancellationToken::new();
    let cancel = async {
        receiver
            .recv()
            .await
            .unwrap()
            .decision
            .send(true)
            .unwrap();
        tokio::task::yield_now().await;
        token.cancel();
    };
    let call = tool.execute(
        ToolCall::function("queued", "MCP_Synthetic_Work", json!({})),
        token.clone(),
    );
    let (result, ()) = time::timeout(Duration::from_secs(1), async { tokio::join!(call, cancel) })
        .await
        .unwrap();
    assert!(matches!(result, Err(ToolError::Cancelled)));
    assert!(
        !messages(directory.path())
            .iter()
            .any(|message| message["method"] == "tools/call")
    );
}

#[tokio::test]
async fn timed_out_mcp_calls_emit_failure_receipts_and_later_calls_still_need_approval() {
    use orbit_agent::{
        agent::{Agent, AgentConfig},
        events::AgentEvent,
        model::MockModel,
    };
    use orbit_model::types::{ChatCompletion, ChatMessage, ChatRequest};

    let directory = tempfile::tempdir().unwrap();
    let mut registry = ToolRegistry::new();
    let (approvals, mut receiver) = crate::approval::channel();
    let statuses = register(
        &mut registry,
        &McpConfig {
            servers: BTreeMap::from([("synthetic".into(), config("timeout"))]),
        },
        directory.path(),
        Some(approvals),
        true,
    )
    .await;
    assert!(statuses[0].connected);
    let approval = tokio::spawn(async move {
        let mut count = 0;
        while let Some(request) = receiver.recv().await {
            request.decision.send(true).unwrap();
            count += 1;
        }
        count
    });
    let completions = ["first", "second"]
        .into_iter()
        .map(|id| ChatCompletion {
            id: None,
            model: None,
            message: ChatMessage::assistant_tool_calls(vec![ToolCall::function(
                id,
                "MCP_Synthetic_Work",
                json!({}),
            )]),
            finish_reason: Some("tool_calls".into()),
            usage: None,
        })
        .chain(std::iter::once(ChatCompletion {
            id: None,
            model: None,
            message: ChatMessage::assistant("complete"),
            finish_reason: Some("stop".into()),
            usage: None,
        }));
    let agent = Agent::new(
        MockModel::new(completions),
        registry,
        AgentConfig::default(),
    );
    let (sender, mut events) = tokio::sync::mpsc::channel(64);
    agent
        .run_with_agent_events(
            ChatRequest::new(vec![ChatMessage::user("perform the synthetic work")]),
            CancellationToken::new(),
            sender,
        )
        .await
        .unwrap();
    let mut receipts = Vec::new();
    while let Some(event) = events.recv().await {
        match event {
            AgentEvent::ToolStarted { call } => receipts.push(format!("{}:started", call.id)),
            AgentEvent::ToolFailed { call, error } => {
                assert!(error.contains("timed out"));
                receipts.push(format!("{}:failed", call.id));
            }
            AgentEvent::ToolFinished { call, content, .. } => {
                assert_eq!(
                    serde_json::from_str::<Value>(&content).unwrap()["connection_restarted"],
                    true
                );
                receipts.push(format!("{}:finished", call.id));
            }
            _ => {}
        }
    }
    assert_eq!(
        receipts,
        [
            "first:started",
            "first:failed",
            "second:started",
            "second:finished"
        ]
    );
    drop(agent);
    assert_eq!(approval.await.unwrap(), 2);
}
