use std::time::Duration;

use orbit_agent::{
    background::{BackgroundJobs, JobStatus},
    tool::{Tool, ToolError},
};
use orbit_model::types::ToolCall;
use orbit_runtime::filesystem::Workspace;
use orbit_tools::{
    approval,
    command_jobs::{CommandJobAction, CommandJobs},
    run_command::RunCommand,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn commands_run_in_parallel_and_keep_their_own_logs_and_verification() {
    let root = tempfile::tempdir().unwrap();
    let logs = orbit_runtime::execution::output::CommandOutputs::default();
    let task = logs.begin_run().unwrap();
    let jobs = BackgroundJobs::default();
    let (sender, mut approvals) = approval::channel();
    let approver = tokio::spawn(async move {
        while let Some(request) = approvals.recv().await {
            request.decision.send(true).unwrap();
        }
    });
    let command = RunCommand::new(Workspace::new(root.path()).unwrap(), sender)
        .with_background_jobs(jobs.clone())
        .with_output_store(logs.clone());
    let mut ids = Vec::new();
    let mut outputs = Vec::new();
    for name in ["orbit-first", "orbit-second"] {
        let script = format!(
            "printf '{name}\\n'; touch {name}.started; while [ ! -f release ]; do sleep 0.01; \
                done; printf '{name} finished\\n'"
        );
        let output = tokio::time::timeout(
            Duration::from_secs(10),
            command.execute(
                ToolCall::function(
                    name,
                    "Exec",
                    json!({
                        "command": script,
                        "background": true,
                        "workspace_access": "read_write"
                    }),
                ),
                CancellationToken::new(),
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let value: Value = serde_json::from_str(&output.content).unwrap();
        assert_eq!(value["running"], true);
        ids.push(
            value["job_id"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
        outputs.push(
            value["output_id"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }

    tokio::time::timeout(Duration::from_secs(10), async {
        while !root
            .path()
            .join("orbit-first.started")
            .exists()
            || !root
                .path()
                .join("orbit-second.started")
                .exists()
        {
            let snapshots = jobs.snapshots(&ids, true).unwrap();
            assert!(
                !snapshots
                    .iter()
                    .any(|job| job.status == JobStatus::Failed),
                "command launch failed: {snapshots:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        jobs.snapshots(&ids, false)
            .unwrap()
            .iter()
            .all(|job| job.status == JobStatus::Running)
    );
    assert_ne!(outputs[0], outputs[1]);
    std::fs::write(root.path().join("release"), "Orbit release").unwrap();
    let finished = tokio::time::timeout(
        Duration::from_secs(10),
        jobs.wait(&ids, true, CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap();

    for (index, job) in finished.iter().enumerate() {
        assert_eq!(job.status, JobStatus::Completed);
        assert_eq!(job.result.as_ref().unwrap()["exit_code"], 0);
        assert!(
            job.result.as_ref().unwrap()["verification_error"]
                .as_str()
                .unwrap()
                .contains("workspace changed")
        );
        let log = std::fs::read_to_string(
            logs.lease()
                .unwrap()
                .path()
                .join(format!("{}.stdout", outputs[index])),
        )
        .unwrap();
        let own = if index == 0 {
            "orbit-first"
        } else {
            "orbit-second"
        };
        let other = if index == 0 {
            "orbit-second"
        } else {
            "orbit-first"
        };

        assert!(log.contains(own));
        assert!(!log.contains(other));
    }

    drop(command);
    let log_path = logs.lease().unwrap().path().to_owned();
    drop(task);
    assert!(!log_path.exists());
    approver.await.unwrap();
}

#[tokio::test]
async fn background_launch_still_requires_approval_and_denial_creates_no_job() {
    let root = tempfile::tempdir().unwrap();
    let jobs = BackgroundJobs::default();
    let (sender, mut approvals) = approval::channel();
    let command = RunCommand::new(Workspace::new(root.path()).unwrap(), sender)
        .with_background_jobs(jobs.clone());
    let launch = tokio::spawn(async move {
        command
            .execute(
                ToolCall::function(
                    "denied",
                    "Exec",
                    json!({
                        "command": "touch orbit-denied",
                        "background": true,
                        "workspace_access": "read_write"
                    }),
                ),
                CancellationToken::new(),
            )
            .await
    });
    let request = approvals.recv().await.unwrap();
    assert!(
        jobs.snapshots(&[], false)
            .unwrap()
            .is_empty()
    );
    assert!(!launch.is_finished());
    request.decision.send(false).unwrap();
    assert!(matches!(
        launch.await.unwrap(),
        Err(ToolError::ExecutionFailed(_))
    ));
    assert!(
        jobs.snapshots(&[], false)
            .unwrap()
            .is_empty()
    );
    assert!(
        !root
            .path()
            .join("orbit-denied")
            .exists()
    );
}

#[tokio::test]
async fn job_tools_validate_schema_and_cancellation_is_session_scoped() {
    let first = BackgroundJobs::default();
    let second = BackgroundJobs::default();
    let job = first
        .start(
            ToolCall::function("orbit-job", "Exec", json!({})),
            "orbit-job".into(),
            None,
            CancellationToken::new(),
            |token| async move {
                token.cancelled().await;
                Err(ToolError::Cancelled)
            },
        )
        .unwrap();
    let foreign_cancel = CommandJobs::new(second, CommandJobAction::Cancel);
    assert!(
        foreign_cancel
            .execute(
                ToolCall::function(
                    "foreign",
                    "StopCommands",
                    json!({
                        "job_ids": [job.job_id]
                    })
                ),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert_eq!(
        first
            .snapshots(std::slice::from_ref(&job.job_id), false)
            .unwrap()[0]
            .status,
        JobStatus::Running
    );
    let cancel = CommandJobs::new(first.clone(), CommandJobAction::Cancel);
    let wait = CommandJobs::new(first.clone(), CommandJobAction::Wait);
    let timed_out = wait
        .execute(
            ToolCall::function(
                "bounded-wait",
                "WaitCommands",
                json!({
                    "job_ids": [job.job_id], "timeout_seconds": 0
                }),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let timed_out: Value = serde_json::from_str(&timed_out.content).unwrap();
    assert_eq!(timed_out["wait_timed_out"], true);
    assert_eq!(timed_out["jobs"][0]["status"], "running");
    assert!(
        cancel
            .execute(
                ToolCall::function("empty", "StopCommands", json!({})),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    let status = CommandJobs::new(first.clone(), CommandJobAction::Status);
    assert!(
        status
            .execute(
                ToolCall::function(
                    "bad",
                    "CommandStatus",
                    json!({
                        "mode": "all"
                    })
                ),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    cancel
        .execute(
            ToolCall::function(
                "cancel",
                "StopCommands",
                json!({
                    "job_ids": [job.job_id]
                }),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let wait = CommandJobs::new(first, CommandJobAction::Wait);
    let result = wait
        .execute(
            ToolCall::function(
                "wait",
                "WaitCommands",
                json!({
                    "job_ids": [job.job_id],
                    "mode": "all"
                }),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let value: Value = serde_json::from_str(&result.content).unwrap();
    assert_eq!(value["jobs"][0]["status"], "cancelled");
    assert_eq!(value["wait_timed_out"], false);
}

#[tokio::test]
async fn cancelling_a_live_native_command_waits_for_process_cleanup() {
    let root = tempfile::tempdir().unwrap();
    let outputs = orbit_runtime::execution::output::CommandOutputs::default();
    let task = outputs.begin_run().unwrap();
    let jobs = BackgroundJobs::default();
    let (sender, mut approvals) = approval::channel();
    let approver = tokio::spawn(async move {
        approvals
            .recv()
            .await
            .unwrap()
            .decision
            .send(true)
            .unwrap();
    });
    let command = RunCommand::new(Workspace::new(root.path()).unwrap(), sender)
        .with_background_jobs(jobs.clone())
        .with_output_store(outputs.clone());
    let output = command
        .execute(
            ToolCall::function(
                "orbit-cancel",
                "Exec",
                json!({
                    "command": "touch orbit.started; sleep 30; touch orbit.finished",
                    "background": true,
                    "workspace_access": "read_write",
                }),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    approver.await.unwrap();
    let id = output.background_job.unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !root
            .path()
            .join("orbit.started")
            .exists()
        {
            let snapshots = jobs
                .snapshots(std::slice::from_ref(&id), true)
                .unwrap();
            assert_eq!(
                snapshots[0].status,
                JobStatus::Running,
                "command launch failed: {snapshots:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    jobs.cancel(std::slice::from_ref(&id))
        .unwrap();
    let completed = tokio::time::timeout(
        Duration::from_secs(5),
        jobs.wait(&[id], true, CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(completed[0].status, JobStatus::Cancelled);
    assert_eq!(completed[0].result.as_ref().unwrap()["cancelled"], true);
    assert!(
        !root
            .path()
            .join("orbit.finished")
            .exists()
    );
    let directory = outputs
        .lease()
        .unwrap()
        .path()
        .to_owned();
    drop(task);
    assert!(!directory.exists());
}

#[tokio::test]
async fn background_completion_emits_original_verification_evidence() {
    use orbit_agent::{
        Agent, AgentConfig, events::AgentEvent, model::MockModel, tool::ToolRegistry,
    };
    use orbit_model::types::{ChatCompletion, ChatMessage, ChatRequest};
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("orbit.rs"), "fn orbit() {}\n").unwrap();
    let (sender, mut approvals) = approval::channel();
    let approver = tokio::spawn(async move {
        approvals
            .recv()
            .await
            .unwrap()
            .decision
            .send(true)
            .unwrap();
    });
    let mut tools = ToolRegistry::new();
    tools.register(
        RunCommand::new(Workspace::new(root.path()).unwrap(), sender)
            .with_background_jobs(tools.background_jobs().clone()),
    );
    let response = |message| ChatCompletion {
        id: None,
        model: Some("orbit-test".into()),
        message,
        finish_reason: Some("stop".into()),
        usage: None,
    };
    let model = MockModel::new([
        response(ChatMessage::assistant_tool_calls(vec![ToolCall::function(
            "orbit-verify",
            "Exec",
            json!({
                "command": "printf 'Orbit verified\\n'",
                "background": true
            }),
        )])),
        response(ChatMessage::assistant("Orbit is checking.")),
        response(ChatMessage::assistant("Orbit checked.")),
    ]);
    let agent = Agent::new(model, tools, AgentConfig::default());
    let (sender, mut events) = tokio::sync::mpsc::channel(128);
    agent
        .run_with_agent_events(
            ChatRequest::new(vec![ChatMessage::user("Check Orbit")]),
            CancellationToken::new(),
            sender,
        )
        .await
        .unwrap();
    approver.await.unwrap();
    let mut evidence_seen = false;
    let mut finished = false;
    while let Some(event) = events.recv().await {
        match event {
            AgentEvent::VerificationRecorded { call_id, evidence } => {
                assert_eq!(call_id, "orbit-verify");
                assert!(!evidence.input.digest.is_empty());
                assert_eq!(evidence.output.unwrap().digest, evidence.input.digest);
                evidence_seen = true;
            }

            AgentEvent::ToolFinished { call, content, .. } if call.id == "orbit-verify" => {
                assert!(evidence_seen);
                let value: Value = serde_json::from_str(&content).unwrap();
                assert_eq!(value["exit_code"], 0);
                finished = true;
            }

            _ => {}
        }
    }

    assert!(finished && evidence_seen);
}
