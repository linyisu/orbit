use std::fs;

use orbit_agent::tool::Tool;
use orbit_model::types::ToolCall;
use orbit_runtime::{
    capability::EnvironmentId,
    filesystem::{ResolvedPath, Workspace},
    policy::FilesystemGrant,
};
use orbit_tools::{approval, run_command::RunCommand};
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
                    "command": "echo 'direct execution' > /workspace/output.txt",
                    "workspace_access": "read_write"
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
async fn run_command_defaults_to_read_only_workspace() {
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
                "test",
                "run_command",
                json!({
                    "command": "echo 'should fail' > /workspace/readonly.txt"
                }),
            ),
            CancellationToken::new(),
        )
        .await
    });

    let request = receiver.recv().await.unwrap();
    match &request.preview {
        approval::ApprovalPreview::Command(preview) => {
            assert_eq!(
                preview.workspace_access,
                approval::CommandWorkspaceAccess::Read
            );
        }
        _ => panic!("expected command preview"),
    }
    request.decision.send(true).unwrap();

    let output = task.await.unwrap().unwrap();
    let value: serde_json::Value = serde_json::from_str(&output.content).unwrap();
    // Writing to read-only workspace fails
    assert_ne!(value["exit_code"], 0);

    let created = ws_temp.path().join("readonly.txt");
    assert!(!created.exists());
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
async fn run_command_rejects_deprecated_writable_snapshot_flag() {
    let ws_temp = tempfile::tempdir().unwrap();
    let workspace = match Workspace::new(ws_temp.path()) {
        Ok(w) => w,
        Err(_) => return,
    };

    let (sender, _receiver) = approval::channel();
    let tool = RunCommand::new(workspace, sender);

    let err = tool
        .execute(
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
        .unwrap_err()
        .to_string();

    assert!(err.contains("'writable_snapshot' is deprecated"));
    assert!(err.contains("workspace_access"));
}

#[tokio::test]
async fn run_command_masks_protected_files_and_git_directory_even_with_read_write() {
    let ws_temp = tempfile::tempdir().unwrap();
    let mut workspace = match Workspace::new(ws_temp.path()) {
        Ok(w) => w,
        Err(_) => return,
    };

    let env_file = ws_temp.path().join(".env");
    fs::write(&env_file, "SECRET_ENV_KEY=12345\n").unwrap();

    let git_dir = ws_temp.path().join(".git");
    fs::create_dir(&git_dir).unwrap();
    let git_config = git_dir.join("config");
    fs::write(&git_config, "SECRET_GIT_CONFIG\n").unwrap();

    let config_file = ws_temp.path().join("config.toml");
    fs::write(&config_file, "SECRET_CONFIG_DATA=xyz\n").unwrap();
    workspace.protect(&config_file).unwrap();

    let (sender, mut receiver) = approval::channel();
    let tool = RunCommand::new(workspace, sender);

    let task = tokio::spawn(async move {
        tool.execute(
            ToolCall::function(
                "test",
                "run_command",
                json!({
                    "command": "cat /workspace/.env 2>/dev/null || true; cat /workspace/.git/config 2>/dev/null || true; cat /workspace/config.toml 2>/dev/null || true; echo hacked > /workspace/.env 2>/dev/null || true; rm -rf /workspace/.git 2>/dev/null || true; rm -f /workspace/config.toml 2>/dev/null || true",
                    "workspace_access": "read_write"
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

    // Verify secrets were not leaked into stdout
    let stdout = value["stdout"].as_str().unwrap_or("");
    assert!(!stdout.contains("SECRET_ENV_KEY"));
    assert!(!stdout.contains("SECRET_GIT_CONFIG"));
    assert!(!stdout.contains("SECRET_CONFIG_DATA"));

    // Verify host files are unchanged
    assert_eq!(
        fs::read_to_string(&env_file).unwrap(),
        "SECRET_ENV_KEY=12345\n"
    );
    assert_eq!(
        fs::read_to_string(&git_config).unwrap(),
        "SECRET_GIT_CONFIG\n"
    );
    assert_eq!(
        fs::read_to_string(&config_file).unwrap(),
        "SECRET_CONFIG_DATA=xyz\n"
    );
}

#[tokio::test]
async fn run_command_succeeds_when_protecting_deep_opaque_runtime_root() {
    let ws_temp = tempfile::tempdir().unwrap();
    let mut workspace = match Workspace::new(ws_temp.path()) {
        Ok(w) => w,
        Err(_) => return,
    };

    // Construct an external opaque runtime root whose directory depth (35) exceeds
    // MAX_PROTECTED_DIR_DEPTH (32). If scanned recursively as a regular protected directory,
    // collect_dir_inodes would fail closed. Because it is registered as an opaque runtime root,
    // recursive descendant scanning is skipped, ensuring hermetic execution without host dependencies.
    let temp_storage = tempfile::tempdir().unwrap();
    let external_storage = temp_storage.path().join("orbit.workspaces");
    let mut curr = external_storage.clone();
    for i in 0..35 {
        curr = curr.join(format!("level_{i}"));
    }
    fs::create_dir_all(&curr).unwrap();
    fs::write(curr.join("deep.txt"), "deep content").unwrap();

    workspace
        .protect_opaque_runtime_root(&external_storage)
        .unwrap();

    let (sender, mut receiver) = approval::channel();
    let tool = RunCommand::new(workspace, sender);

    let task = tokio::spawn(async move {
        tool.execute(
            ToolCall::function(
                "test",
                "run_command",
                json!({
                    "command": "echo 'opaque root safe'",
                    "workspace_access": "read_write"
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
    assert!(
        value["stdout"]
            .as_str()
            .unwrap_or("")
            .contains("opaque root safe")
    );
}
