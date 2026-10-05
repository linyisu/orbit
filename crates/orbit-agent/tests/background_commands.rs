use std::{collections::HashMap, sync::Mutex, time::Duration};

use orbit_agent::{
    Agent, AgentConfig,
    background::BackgroundJobs,
    events::AgentEvent,
    model::MockModel,
    tool::{ClosureTool, Tool, ToolFuture, ToolOutput, ToolRegistry},
};
use orbit_model::types::{ChatCompletion, ChatMessage, ChatRequest, ToolCall, ToolDefinition};
use serde_json::json;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

struct GatedCommand {
    jobs: BackgroundJobs,
    gates: Mutex<HashMap<String, oneshot::Receiver<String>>>,
}

impl Tool for GatedCommand {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "run_command",
            "Orbit test command",
            json!({"type":"object"}),
        )
    }

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            let name = call.arguments_json().unwrap()["command"]
                .as_str()
                .unwrap()
                .to_owned();
            let gate = self.gates.lock().unwrap().remove(&name).unwrap();
            let job = self.jobs.start(call, name, None, cancellation, |token| async move {
                tokio::select! {
                    biased;
                    _ = token.cancelled() => Err(orbit_agent::tool::ToolError::Cancelled),
                    result = gate => Ok(ToolOutput::text(json!({"exit_code": 0, "stdout": result.unwrap()}).to_string())),
                }
            })?;
            let mut output =
                ToolOutput::text(json!({"job_id":job.job_id,"background":true}).to_string());
            output.background_job = Some(job.job_id);
            Ok(output)
        })
    }
}

fn response(message: ChatMessage) -> ChatCompletion {
    ChatCompletion {
        id: None,
        model: Some("orbit-test".into()),
        message,
        finish_reason: Some("stop".into()),
        usage: None,
    }
}

fn launch(id: &str, command: &str) -> ToolCall {
    ToolCall::function(
        id,
        "run_command",
        json!({"command":command,"background":true}),
    )
}

fn fixture(names: &[&str]) -> (ToolRegistry, Vec<oneshot::Sender<String>>) {
    let mut tools = ToolRegistry::new();
    let mut gates = HashMap::new();
    let mut releases = Vec::new();
    for name in names {
        let (release, gate) = oneshot::channel();
        gates.insert((*name).into(), gate);
        releases.push(release);
    }

    tools.register(GatedCommand {
        jobs: tools.background_jobs().clone(),
        gates: Mutex::new(gates),
    });
    (tools, releases)
}

async fn waiting(receiver: &mut mpsc::Receiver<AgentEvent>) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(event) = receiver.recv().await {
            if matches!(event, AgentEvent::WaitingForCommands { .. }) {
                return;
            }
        }

        panic!("Agent ended before waiting for commands");
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn idle_agent_waits_without_model_polling_and_receives_completion_once() {
    let (tools, mut release) = fixture(&["orbit-build"]);
    let model = MockModel::new([
        response(ChatMessage::assistant_tool_calls(vec![launch(
            "build",
            "orbit-build",
        )])),
        response(ChatMessage::assistant("Waiting for the build.")),
        response(ChatMessage::assistant("Build passed.")),
    ]);
    let agent = Agent::new(model.clone(), tools, AgentConfig::default());
    let (sender, mut receiver) = mpsc::channel(128);
    let run = tokio::spawn(async move {
        agent
            .run_with_agent_events(
                ChatRequest::new(vec![ChatMessage::user("Build Orbit")]),
                CancellationToken::new(),
                sender,
            )
            .await
    });

    waiting(&mut receiver).await;
    assert_eq!(model.requests().len(), 2);
    assert!(!run.is_finished());
    release
        .pop()
        .unwrap()
        .send("ORBIT BUILD PASSED".into())
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    assert_eq!(model.requests().len(), 3);
    let notifications = result.history.iter().filter(|message| matches!(message, ChatMessage::User {content, ..} if content.contains("background_command_completion"))).count();
    assert_eq!(notifications, 1);
    assert_eq!(result.history.iter().filter(|message| matches!(message, ChatMessage::Tool {tool_call_id, ..} if tool_call_id == "build")).count(), 1);
    assert!(model.requests()[2].messages.iter().any(|message| matches!(message, ChatMessage::User {content, ..} if content.contains("ORBIT BUILD PASSED"))));
    let mut finished = 0;
    while let Some(event) = receiver.recv().await {
        if matches!(event, AgentEvent::ToolFinished { call, .. } if call.id == "build") {
            finished += 1;
        }
    }

    assert_eq!(finished, 1);
}

#[tokio::test]
async fn commands_overlap_other_work_and_wait_all_does_not_duplicate_results() {
    let (mut tools, mut releases) = fixture(&["orbit-build", "orbit-test"]);
    let jobs = tools.background_jobs().clone();
    tools.register(ClosureTool::new(
        "check_docs",
        "Inspect Orbit docs",
        json!({"type":"object"}),
        move |_, _| {
            let jobs = jobs.clone();
            async move {
                let snapshots = jobs.snapshots(&[], false)?;
                assert_eq!(snapshots.len(), 2);
                assert!(
                    snapshots
                        .iter()
                        .all(|job| job.status == orbit_agent::background::JobStatus::Running)
                );
                Ok("ORBIT DOCS CHECKED".into())
            }
        },
    ));
    let jobs = tools.background_jobs().clone();
    let (waiting_sender, waiting_receiver) = oneshot::channel();
    let waiting_sender = std::sync::Arc::new(Mutex::new(Some(waiting_sender)));
    tools.register(ClosureTool::new(
        "wait_commands",
        "Wait for commands",
        json!({"type":"object"}),
        move |_, token| {
            let jobs = jobs.clone();
            waiting_sender
                .lock()
                .unwrap()
                .take()
                .unwrap()
                .send(())
                .unwrap();
            async move { Ok(serde_json::to_string(&jobs.wait(&[], true, token).await?).unwrap()) }
        },
    ));
    let model = MockModel::new([
        response(ChatMessage::assistant_tool_calls(vec![
            launch("build", "orbit-build"),
            launch("test", "orbit-test"),
        ])),
        response(ChatMessage::assistant_tool_calls(vec![ToolCall::function(
            "docs",
            "check_docs",
            json!({}),
        )])),
        response(ChatMessage::assistant_tool_calls(vec![ToolCall::function(
            "wait",
            "wait_commands",
            json!({}),
        )])),
        response(ChatMessage::assistant("Build, tests and docs passed.")),
    ]);
    let agent = Agent::new(model.clone(), tools, AgentConfig::default());
    let run = tokio::spawn(async move {
        agent
            .run(
                ChatRequest::new(vec![ChatMessage::user("Check Orbit")]),
                CancellationToken::new(),
            )
            .await
    });

    tokio::time::timeout(Duration::from_secs(5), waiting_receiver)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(model.requests().len(), 3);
    releases
        .pop()
        .unwrap()
        .send("ORBIT TEST PASSED".into())
        .unwrap();
    tokio::task::yield_now().await;
    assert!(!run.is_finished());
    assert_eq!(model.requests().len(), 3);
    releases
        .pop()
        .unwrap()
        .send("ORBIT BUILD PASSED".into())
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    assert_eq!(model.requests().len(), 4);
    assert!(!result.history.iter().any(|message| matches!(message, ChatMessage::User {content, ..} if content.contains("background_command_completion"))));
    assert!(model.requests()[3].messages.iter().any(|message| matches!(message, ChatMessage::Tool {tool_call_id, content} if tool_call_id == "wait" && content.contains("ORBIT TEST PASSED") && content.contains("ORBIT BUILD PASSED"))));
}

#[tokio::test]
async fn cancellation_while_idle_reaps_jobs_without_another_model_request() {
    let (tools, _release) = fixture(&["orbit-build"]);
    let jobs = tools.background_jobs().clone();
    let model = MockModel::new([
        response(ChatMessage::assistant_tool_calls(vec![launch(
            "build",
            "orbit-build",
        )])),
        response(ChatMessage::assistant("Waiting.")),
    ]);
    let token = CancellationToken::new();
    let run_token = token.clone();
    let agent = Agent::new(model.clone(), tools, AgentConfig::default());
    let (sender, mut receiver) = mpsc::channel(128);
    let run = tokio::spawn(async move {
        agent
            .run_with_agent_events(
                ChatRequest::new(vec![ChatMessage::user("Build Orbit")]),
                run_token,
                sender,
            )
            .await
    });

    waiting(&mut receiver).await;
    let id = jobs.snapshots(&[], false).unwrap()[0].job_id.clone();
    token.cancel();
    let result = tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        result,
        Err(orbit_agent::error::AgentError::Cancelled)
    ));
    assert_eq!(model.requests().len(), 2);
    assert_eq!(
        jobs.snapshots(&[id], false).unwrap()[0].status,
        orbit_agent::background::JobStatus::Cancelled
    );
}
