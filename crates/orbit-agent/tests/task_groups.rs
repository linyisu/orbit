use std::sync::Arc;

use orbit_agent::{
    agent::{Agent, AgentConfig},
    background::BackgroundJobs,
    events::AgentEvent,
    inbox::{AgentInbox, InboxMessage},
    model::MockModel,
    tool::{ToolOutput, ToolRegistry},
};
use orbit_model::types::{ChatCompletion, ChatMessage, ChatRequest, ToolCall};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

fn response(text: &str) -> ChatCompletion {
    ChatCompletion {
        id: None,
        model: None,
        message: ChatMessage::assistant(text),
        finish_reason: Some("stop".into()),
        usage: None,
    }
}

#[tokio::test]
async fn auxiliary_completion_wakes_parent_without_polling_or_duplicate_tool_responses() {
    let jobs = BackgroundJobs::with_capacity(16).unwrap();
    jobs.restore_result(
        ToolCall::function("child", "spawn_agent", serde_json::json!({"task":"review"})),
        "review".into(),
        ToolOutput::text("{\"agent_id\":\"orbit-child\",\"status\":\"completed\"}"),
    )
    .unwrap();
    let mut tools = ToolRegistry::new();
    tools.add_task_group(jobs);
    let model = MockModel::new([response("conclusion")]);
    let result = Agent::new(model.clone(), tools, AgentConfig::default())
        .run(
            ChatRequest::new(vec![ChatMessage::user("task")]),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(model.requests().len(), 1);
    assert!(result.history.iter().any(
        |m| matches!(m,ChatMessage::User{content,..} if content.contains("subagent_completion"))
    ));
    assert!(
        !result
            .history
            .iter()
            .any(|m| matches!(m,ChatMessage::Tool{tool_call_id,..} if tool_call_id=="child"))
    );
}

#[tokio::test]
async fn inbox_wakes_waiting_agent_and_cancellation_reaps_auxiliary_jobs() {
    let jobs = BackgroundJobs::default();
    let mut tools = ToolRegistry::new();
    tools.add_task_group(jobs.clone());
    let inbox = AgentInbox::default();
    let job_done = Arc::new(Notify::new());
    let done = job_done.clone();
    let start_jobs = jobs.clone();
    tools.register(orbit_agent::tool::ClosureTool::new(
        "start_child",
        "",
        serde_json::json!({}),
        move |_, token| {
            let jobs = start_jobs.clone();
            let done = done.clone();
            async move {
                jobs.start(
                    ToolCall::function("child", "spawn_agent", serde_json::json!({})),
                    "child".into(),
                    None,
                    token,
                    move |cancel| async move {
                        cancel.cancelled().await;
                        done.notify_one();
                        Err(orbit_agent::tool::ToolError::Cancelled)
                    },
                )?;
                Ok::<_, orbit_agent::tool::ToolError>("started".into())
            }
        },
    ));
    let mut initial = response("");
    initial.message = ChatMessage::assistant_tool_calls(vec![ToolCall::function(
        "start",
        "start_child",
        serde_json::json!({}),
    )]);
    let model = MockModel::new([initial, response("waiting"), response("updated")]);
    let token = CancellationToken::new();
    let mut first = true;
    let agent = Agent::new(model.clone(), tools, AgentConfig::default()).with_inbox(inbox.clone());
    let (sender, mut receiver) = tokio::sync::mpsc::channel(32);
    let run_token = token.clone();
    let run = tokio::spawn(async move {
        agent
            .run_with_agent_events(
                ChatRequest::new(vec![ChatMessage::user("initial")]),
                run_token,
                sender,
            )
            .await
    });
    while let Some(event) = receiver.recv().await {
        if matches!(event, AgentEvent::WaitingForTasks { .. }) {
            if first {
                first = false;
                inbox
                    .push(InboxMessage {
                        id: 7,
                        content: "extra context".into(),
                    })
                    .await
                    .unwrap();
            } else {
                token.cancel();
            }
        }
    }
    let result = run.await.unwrap();
    job_done.notified().await;
    assert!(matches!(
        result,
        Err(orbit_agent::error::AgentError::Cancelled)
    ));
    assert_eq!(model.requests().len(), 3);
    assert!(
        model.requests()[2]
            .messages
            .iter()
            .any(|m| matches!(m,ChatMessage::User{content,..} if content=="extra context"))
    );
}
