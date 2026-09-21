use std::fs;

use orbit_agent::tool::Tool;
use orbit_model::types::ToolCall;
use orbit_tools::{
    approval, capability::EnvironmentId, path_security::ResolvedPath, policy::FilesystemGrant,
    run_command::RunCommand, workspace::Workspace,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn run_command_executes_directly_in_workspace_and_persists_changes() {
    let ws_temp = tempfile::tempdir().unwrap();
    let workspace = match Workspace::new(ws_temp.path()) {
        Ok(w) => w,
        Err(_) => return,
    };

    let (sender, mut receiver) = approval::channel();
    let tool = RunCommand::new(workspace.clone(), sender);

    let task = tokio::spawn(async move {
        tool.execute(
            ToolCall::function(
                "test",
                "run_command",
                json!({
                    "command": "echo 'direct execution' > /workspace/output.txt"
                }),
            ),
            CancellationToken::new(),
        )
        .await
    });

    let request = receiver.recv().await.unwrap();
    request.decision.send(true).unwrap();

    let output = task.await.unwrap().unwrap();
    let value: serde_json::Value = serde_json::from_str(&output.content).unwrap();
    assert_eq!(value["exit_code"], 0);

    let evidence = output
        .verification
        .expect("verification evidence should be present");
    assert!(!evidence.input.digest.is_empty());
    assert!(!evidence.writable_snapshot);
    assert!(evidence.isolation.contains("capability policy sandbox"));
    let post_output = evidence
        .output
        .expect("output fingerprint should be present");
    assert!(!post_output.digest.is_empty());
    assert_ne!(post_output.digest, evidence.input.digest);
    assert!(post_output.files.contains_key("output.txt"));

    let created = ws_temp.path().join("output.txt");
    assert!(created.is_file());
    assert_eq!(
        fs::read_to_string(created).unwrap().trim(),
        "direct execution"
    );
}

#[tokio::test]
async fn run_command_respects_session_grants_for_outside_paths() {
    let ws_temp = tempfile::tempdir().unwrap();
    let outside_temp = tempfile::tempdir().unwrap();

    let workspace = match Workspace::new(ws_temp.path()) {
        Ok(w) => w,
        Err(_) => return,
    };

    let outside_file = outside_temp.path().join("grant_target.txt");
    fs::write(&outside_file, "initial").unwrap();

    let (sender, mut receiver) = approval::channel();
    let tool = RunCommand::new(workspace.clone(), sender);

    // 1. Without grant: writing to outside file fails
    let script_fail = format!("echo 'should fail' > {}", outside_file.display());
    let task_fail = tokio::spawn({
        let tool = tool.clone();
        async move {
            tool.execute(
                ToolCall::function("test", "run_command", json!({ "command": script_fail })),
                CancellationToken::new(),
            )
            .await
        }
    });

    let request = receiver.recv().await.unwrap();
    request.decision.send(true).unwrap();

    let output = task_fail.await.unwrap().unwrap();
    let value: serde_json::Value = serde_json::from_str(&output.content).unwrap();
    assert_ne!(value["exit_code"], 0);

    // 2. With grant: writing to outside file succeeds
    let resolved = ResolvedPath::resolve(&outside_file, None).unwrap();
    workspace.policy().record_grant(FilesystemGrant {
        environment: EnvironmentId::Local,
        path: resolved,
        read: true,
        write: true,
    });

    let (sender2, mut receiver2) = approval::channel();
    let tool2 = RunCommand::new(workspace, sender2);

    let script_ok = format!("echo 'granted write' > {}", outside_file.display());
    let task_ok = tokio::spawn(async move {
        tool2
            .execute(
                ToolCall::function("test", "run_command", json!({ "command": script_ok })),
                CancellationToken::new(),
            )
            .await
    });

    let request2 = receiver2.recv().await.unwrap();
    request2.decision.send(true).unwrap();

    let output2 = task_ok.await.unwrap().unwrap();
    let value2: serde_json::Value = serde_json::from_str(&output2.content).unwrap();
    assert_eq!(value2["exit_code"], 0);

    assert_eq!(
        fs::read_to_string(&outside_file).unwrap().trim(),
        "granted write"
    );
}

#[tokio::test]
async fn run_command_accepts_deprecated_writable_snapshot_flag() {
    let ws_temp = tempfile::tempdir().unwrap();
    let workspace = match Workspace::new(ws_temp.path()) {
        Ok(w) => w,
        Err(_) => return,
    };

    let (sender, mut receiver) = approval::channel();
    let tool = RunCommand::new(workspace, sender);

    let task = tokio::spawn(async move {
        tool.execute(
            ToolCall::function(
                "legacy_call",
                "run_command",
                json!({
                    "command": "echo 'snapshot compat' > /workspace/legacy.txt",
                    "writable_snapshot": true
                }),
            ),
            CancellationToken::new(),
        )
        .await
    });

    let request = receiver.recv().await.unwrap();
    request.decision.send(true).unwrap();

    let output = task.await.unwrap().unwrap();
    let value: serde_json::Value = serde_json::from_str(&output.content).unwrap();
    assert_eq!(value["exit_code"], 0);

    let created = ws_temp.path().join("legacy.txt");
    assert!(created.is_file());
    assert_eq!(
        fs::read_to_string(created).unwrap().trim(),
        "snapshot compat"
    );
}
