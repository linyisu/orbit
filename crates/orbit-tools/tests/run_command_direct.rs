use std::fs;

use orbit_agent::tool::Tool;
use orbit_model::types::ToolCall;
use orbit_runtime::{
    capability::EnvironmentId,
    filesystem::{ResolvedPath, Workspace},
    policy::FilesystemGrant,
};
use orbit_tools::{
    approval,
    run_command::{CommandLimits, RunCommand},
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn sandbox_command(workspace: Workspace, sender: approval::ApprovalSender) -> RunCommand {
    RunCommand::with_limits(
        workspace,
        sender,
        CommandLimits {
            sandbox: true,
            ..Default::default()
        },
    )
}

#[tokio::test]
async fn auto_literal_text_pipeline_creates_and_appends_workspace_files_without_a_dialog() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("existing")).unwrap();
    let (sender, mut receiver) = approval::channel();
    let tool = sandbox_command(Workspace::new(root.path()).unwrap(), sender);
    let task = tokio::spawn(async move {
        tool.execute(ToolCall::function("text", "run_command", json!({
            "command": "mkdir -p demo && echo 'literal $(touch unexpected) | >' > demo/text.txt && printf '%s\\n' 'second line' | tee -a demo/text.txt /workspace/mirror.txt > demo/output.txt && cat demo/text.txt | tee demo/copy.txt",
            "cwd": "existing", "workspace_access": "read_write"
        })), CancellationToken::new()).await
    });
    let request = receiver.recv().await.unwrap();
    assert!(approval::auto_allows(&request.preview, root.path()));
    request.decision.send(true).unwrap();
    let output = task.await.unwrap().unwrap();
    let value: serde_json::Value = serde_json::from_str(&output.content).unwrap();
    assert_eq!(value["exit_code"], 0, "{value}");
    let expected = "literal $(touch unexpected) | >\nsecond line\n";
    assert_eq!(
        fs::read_to_string(root.path().join("existing/demo/text.txt")).unwrap(),
        expected
    );
    assert_eq!(
        fs::read_to_string(root.path().join("existing/demo/copy.txt")).unwrap(),
        expected
    );
    assert_eq!(
        fs::read_to_string(root.path().join("mirror.txt")).unwrap(),
        "second line\n"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("existing/demo/output.txt")).unwrap(),
        "second line\n"
    );
    assert!(!root.path().join("existing/unexpected").exists());
}

#[tokio::test]
async fn auto_local_mkdir_creates_nested_and_quoted_directories_in_the_requested_cwd() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("existing")).unwrap();
    let workspace = Workspace::new(root.path()).unwrap();
    let (sender, mut receiver) = approval::channel();
    let tool = sandbox_command(workspace, sender);
    let task = tokio::spawn(async move {
        tool.execute(
            ToolCall::function(
                "mkdir",
                "run_command",
                json!({
                    "command": "mkdir -p 'demo folder/nested' /workspace/another",
                    "cwd": "existing",
                    "workspace_access": "read_write"
                }),
            ),
            CancellationToken::new(),
        )
        .await
    });

    let request = receiver.recv().await.unwrap();
    // The shared CLI/TUI policy answers this request without showing a dialog.
    assert!(approval::auto_allows(&request.preview, root.path()));
    request.decision.send(true).unwrap();
    let output = task.await.unwrap().unwrap();
    let value: serde_json::Value = serde_json::from_str(&output.content).unwrap();
    assert_eq!(value["exit_code"], 0, "{value}");
    assert!(root.path().join("existing/demo folder/nested").is_dir());
    assert!(root.path().join("another").is_dir());
    assert!(!root.path().join("demo folder").exists());
}

#[tokio::test]
async fn run_command_executes_directly_in_workspace_and_persists_changes() {
    let ws_temp = tempfile::tempdir().unwrap();
    let workspace = match Workspace::new(ws_temp.path()) {
        Ok(w) => w,
        Err(_) => return,
    };

    let (sender, mut receiver) = approval::channel();
    let tool = sandbox_command(workspace.clone(), sender);

    let task = tokio::spawn(async move {
        tool.execute(
            ToolCall::function(
                "test",
                "run_command",
                json!({ "command": "echo 'direct execution' > /workspace/output.txt", "workspace_access": "read_write" }),
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
    let tool = sandbox_command(workspace, sender);

    let task = tokio::spawn(async move {
        tool.execute(
            ToolCall::function(
                "test",
                "run_command",
                json!({ "command": "echo 'should fail' > /workspace/readonly.txt" }),
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
    let tool = sandbox_command(workspace.clone(), sender);

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

    let resolved = ResolvedPath::resolve(&outside_file, None).unwrap();
    workspace.policy().record_grant(FilesystemGrant {
        environment: EnvironmentId::Local,
        path: resolved,
        read: true,
        write: true,
    });

    let (sender2, mut receiver2) = approval::channel();
    let tool2 = sandbox_command(workspace, sender2);

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
async fn run_command_ignores_remote_session_grants_for_local_execution() {
    let ws_temp = tempfile::tempdir().unwrap();
    let outside_temp = tempfile::tempdir().unwrap();

    let workspace = match Workspace::new(ws_temp.path()) {
        Ok(w) => w,
        Err(_) => return,
    };

    let outside_file = outside_temp.path().join("remote_target.txt");
    fs::write(&outside_file, "initial").unwrap();

    let resolved = ResolvedPath::resolve(&outside_file, None).unwrap();
    workspace.policy().record_grant(FilesystemGrant {
        environment: EnvironmentId::Remote("cluster-1".into()),
        path: resolved,
        read: true,
        write: true,
    });

    let (sender, mut receiver) = approval::channel();
    let tool = sandbox_command(workspace, sender);

    let script = format!("echo 'attempt write' > {}", outside_file.display());
    let task = tokio::spawn(async move {
        tool.execute(
            ToolCall::function("test", "run_command", json!({ "command": script })),
            CancellationToken::new(),
        )
        .await
    });

    let request = receiver.recv().await.unwrap();
    match &request.preview {
        approval::ApprovalPreview::Command(preview) => {
            assert!(
                preview.external_capabilities.is_empty(),
                "remote session grants must not appear in local preview capabilities"
            );
            assert!(
                !preview.external_write,
                "remote session grants must not set local external_write"
            );
        }

        _ => panic!("expected command preview"),
    }

    request.decision.send(true).unwrap();

    let output = task.await.unwrap().unwrap();
    let value: serde_json::Value = serde_json::from_str(&output.content).unwrap();

    assert_ne!(value["exit_code"], 0);
    assert_eq!(fs::read_to_string(&outside_file).unwrap().trim(), "initial");
}

#[tokio::test]
async fn run_command_rejects_deprecated_writable_snapshot_flag() {
    let ws_temp = tempfile::tempdir().unwrap();
    let workspace = match Workspace::new(ws_temp.path()) {
        Ok(w) => w,
        Err(_) => return,
    };

    let (sender, _receiver) = approval::channel();
    let tool = sandbox_command(workspace, sender);

    let err = tool
        .execute(
            ToolCall::function(
                "legacy_call",
                "run_command",
                json!({ "command": "echo 'snapshot compat' > /workspace/legacy.txt", "writable_snapshot": true }),
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
async fn run_command_masks_sensitive_files_and_protects_git_config_even_with_read_write() {
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
    let tool = sandbox_command(workspace, sender);

    let task = tokio::spawn(async move {
        tool.execute(
            ToolCall::function(
                "test",
                "run_command",
                json!({
                    "command": "cat /workspace/.env 2>/dev/null || true; cat /workspace/config.toml 2>/dev/null || true; echo hacked > /workspace/.env 2>/dev/null || true; rm -rf /workspace/.git 2>/dev/null || true; echo hacked_config > /workspace/.git/config 2>/dev/null || true; rm -f /workspace/config.toml 2>/dev/null || true",
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

    let stdout = value["stdout"].as_str().unwrap_or("");

    assert!(!stdout.contains("SECRET_ENV_KEY"));
    assert!(!stdout.contains("SECRET_CONFIG_DATA"));

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
async fn run_command_allows_native_git_status_and_commit_in_sandbox() {
    let ws_temp = tempfile::tempdir().unwrap();
    let workspace = match Workspace::new(ws_temp.path()) {
        Ok(w) => w,
        Err(_) => return,
    };

    let status = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(ws_temp.path())
        .status()
        .unwrap();

    assert!(status.success());
    let _ = std::process::Command::new("git")
        .args(["config", "user.name", "Orbit Tester"])
        .current_dir(ws_temp.path())
        .status();
    let _ = std::process::Command::new("git")
        .args(["config", "user.email", "test@orbit.local"])
        .current_dir(ws_temp.path())
        .status();

    fs::write(ws_temp.path().join("initial.txt"), "hello git\n").unwrap();

    let (sender, mut receiver) = approval::channel();
    let tool = sandbox_command(workspace, sender);

    let call = ToolCall::function(
        "test",
        "run_command",
        json!({
            "command": "git status --short",
            "workspace_access": "read"
        }),
    );
    let t1 = tool.clone();
    let task = tokio::spawn(async move { t1.execute(call, CancellationToken::new()).await });
    let req = receiver.recv().await.unwrap();
    assert!(matches!(
        req.preview,
        approval::ApprovalPreview::Command(ref cmd) if cmd.git.is_some()
    ));
    req.decision.send(true).unwrap();

    let out = task.await.unwrap().unwrap();
    let val: serde_json::Value = serde_json::from_str(&out.content).unwrap();
    let stdout = val["stdout"].as_str().unwrap_or("");
    assert!(stdout.contains("initial.txt"));

    let call_commit = ToolCall::function(
        "test",
        "run_command",
        json!({
            "command": "git add initial.txt && git commit -m 'initial commit'",
            "workspace_access": "read_write"
        }),
    );
    let t2 = tool.clone();
    let task2 =
        tokio::spawn(async move { t2.execute(call_commit, CancellationToken::new()).await });
    let req2 = receiver.recv().await.unwrap();
    req2.decision.send(true).unwrap();

    let out2 = task2.await.unwrap().unwrap();
    let val2: serde_json::Value = serde_json::from_str(&out2.content).unwrap();
    assert_eq!(val2["exit_code"], 0);

    let log = std::process::Command::new("git")
        .args(["log", "--oneline"])
        .current_dir(ws_temp.path())
        .output()
        .unwrap();
    let log_str = String::from_utf8_lossy(&log.stdout);
    assert!(log_str.contains("initial commit"));
}

#[tokio::test]
async fn run_command_succeeds_when_protecting_deep_opaque_runtime_root() {
    let ws_temp = tempfile::tempdir().unwrap();
    let mut workspace = match Workspace::new(ws_temp.path()) {
        Ok(w) => w,
        Err(_) => return,
    };

    // This opaque root exceeds the regular scan depth; it is traversed only for possible aliases.
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
    let tool = sandbox_command(workspace, sender);

    let task = tokio::spawn(async move {
        tool.execute(
            ToolCall::function(
                "test",
                "run_command",
                json!({ "command": "echo 'opaque root safe'", "workspace_access": "read_write" }),
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

#[tokio::test]
async fn run_command_masks_hardlink_to_file_inside_external_opaque_root() {
    let ws_temp = tempfile::tempdir().unwrap();
    let storage = tempfile::tempdir().unwrap();
    let opaque = storage.path().join("orbit.workspaces");
    fs::create_dir(&opaque).unwrap();
    let secret = opaque.join("private.txt");
    fs::write(&secret, "PRIVATE_RUNTIME_DATA").unwrap();
    fs::hard_link(&secret, ws_temp.path().join("alias.txt")).unwrap();

    let mut workspace = Workspace::new(ws_temp.path()).unwrap();
    workspace.protect_opaque_runtime_root(&opaque).unwrap();
    let (sender, mut receiver) = approval::channel();
    let tool = sandbox_command(workspace, sender);
    let task = tokio::spawn(async move {
        tool.execute(
            ToolCall::function(
                "opaque_alias",
                "run_command",
                json!({ "command": "cat /workspace/alias.txt" }),
            ),
            CancellationToken::new(),
        )
        .await
    });

    receiver.recv().await.unwrap().decision.send(true).unwrap();
    let output = task.await.unwrap().unwrap();
    let value: serde_json::Value = serde_json::from_str(&output.content).unwrap();

    assert_eq!(value["stdout"], "");
    assert_eq!(fs::read_to_string(&secret).unwrap(), "PRIVATE_RUNTIME_DATA");
}

#[tokio::test]
async fn run_command_reports_when_workspace_fingerprint_is_unavailable() {
    let ws_temp = tempfile::tempdir().unwrap();
    fs::write(
        ws_temp.path().join("large.txt"),
        vec![b'x'; 2 * 1024 * 1024],
    )
    .unwrap();
    let workspace = Workspace::new(ws_temp.path()).unwrap();
    let limits = CommandLimits {
        sandbox: true,
        max_scan_mib: 1,
        ..CommandLimits::default()
    };
    let (sender, mut receiver) = approval::channel();
    let tool = RunCommand::with_limits(workspace, sender, limits);
    let task = tokio::spawn(async move {
        tool.execute(
            ToolCall::function("scan_limit", "run_command", json!({ "command": "true" })),
            CancellationToken::new(),
        )
        .await
    });

    receiver.recv().await.unwrap().decision.send(true).unwrap();
    let output = task.await.unwrap().unwrap();
    let result: serde_json::Value = serde_json::from_str(&output.content).unwrap();

    assert_eq!(result["exit_code"], 0);
    assert!(output.verification.is_none());

    let warning = result["verification_error"].as_str().unwrap();

    assert!(warning.contains("input workspace fingerprint unavailable"));
    assert!(warning.contains("output workspace fingerprint unavailable"));
}

#[tokio::test]
async fn run_command_executes_in_specified_cwd() {
    let ws_temp = tempfile::tempdir().unwrap();
    let sub = ws_temp.path().join("crates").join("my_crate");
    fs::create_dir_all(&sub).unwrap();

    let workspace = match Workspace::new(ws_temp.path()) {
        Ok(w) => w,
        Err(_) => return,
    };

    let (sender, mut receiver) = approval::channel();
    let tool = sandbox_command(workspace.clone(), sender);

    let task = tokio::spawn(async move {
        tool.execute(
            ToolCall::function(
                "test",
                "run_command",
                json!({
                    "command": "pwd && echo 'from_sub' > output.txt",
                    "cwd": "crates/my_crate",
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
    assert_eq!(
        value["stdout"].as_str().unwrap().lines().next().unwrap(),
        "/workspace/crates/my_crate"
    );
    assert_eq!(
        fs::read_to_string(sub.join("output.txt")).unwrap().trim(),
        "from_sub"
    );
}

#[tokio::test]
async fn run_command_rejects_nonexistent_cwd() {
    let ws_temp = tempfile::tempdir().unwrap();
    let workspace = match Workspace::new(ws_temp.path()) {
        Ok(w) => w,
        Err(_) => return,
    };

    let (sender, _receiver) = approval::channel();
    let tool = sandbox_command(workspace, sender);

    let result = tool
        .execute(
            ToolCall::function(
                "test",
                "run_command",
                json!({
                    "command": "true",
                    "cwd": "nonexistent_dir"
                }),
            ),
            CancellationToken::new(),
        )
        .await;

    assert!(result.is_err());
    match result {
        Err(orbit_agent::tool::ToolError::InvalidArguments { message, .. }) => {
            assert!(message.contains("invalid working directory"));
        }
        other => panic!("expected InvalidArguments error, got {:?}", other),
    }
}

#[tokio::test]
async fn run_command_rejects_mount_overlapping_protected_path() {
    let ws_temp = tempfile::tempdir().unwrap();
    let external_temp = tempfile::tempdir().unwrap();

    let secret_file = external_temp.path().join("secret.key");
    fs::write(&secret_file, "SUPER_SECRET_PAYLOAD").unwrap();

    let mut workspace = match Workspace::new(ws_temp.path()) {
        Ok(w) => w,
        Err(_) => return,
    };

    workspace.protect(&secret_file).unwrap();

    let (sender, mut receiver) = approval::channel();
    let mut limits = CommandLimits {
        sandbox: true,
        ..Default::default()
    };
    limits
        .dependency_caches
        .push(orbit_tools::run_command::DependencyCache {
            name: "leak_cache".to_string(),
            path: external_temp.path().to_path_buf(),
        });

    let tool = RunCommand::with_limits(workspace, sender, limits);

    let task = tokio::spawn(async move {
        tool.execute(
            ToolCall::function(
                "test",
                "run_command",
                json!({
                    "command": "cat /opt/orbit-cache/leak_cache/secret.key"
                }),
            ),
            CancellationToken::new(),
        )
        .await
    });

    let request = receiver.recv().await.unwrap();
    request.decision.send(true).unwrap();

    let result = task.await.unwrap();
    assert!(
        result.is_err(),
        "expected execution to fail due to protected path overlap"
    );
    match result {
        Err(orbit_agent::tool::ToolError::ExecutionFailed(msg)) => {
            assert!(
                msg.contains("overlaps protected path"),
                "expected 'overlaps protected path' error, got: {msg}"
            );
        }

        other => panic!("expected ExecutionFailed error, got {:?}", other),
    }
}

#[tokio::test]
async fn run_command_rejects_mount_containing_hardlink_to_protected_path() {
    let ws_temp = tempfile::tempdir().unwrap();
    let external_temp = tempfile::tempdir().unwrap();
    let cache_temp = tempfile::tempdir().unwrap();

    let secret_file = external_temp.path().join("secret.key");
    fs::write(&secret_file, "SUPER_SECRET_PAYLOAD").unwrap();

    let mut workspace = match Workspace::new(ws_temp.path()) {
        Ok(w) => w,
        Err(_) => return,
    };

    workspace.protect(&secret_file).unwrap();

    let alias_file = cache_temp.path().join("alias.key");
    fs::hard_link(&secret_file, &alias_file).unwrap();

    let (sender, mut receiver) = approval::channel();
    let mut limits = CommandLimits {
        sandbox: true,
        ..Default::default()
    };
    limits
        .dependency_caches
        .push(orbit_tools::run_command::DependencyCache {
            name: "leak_hardlink_cache".to_string(),
            path: cache_temp.path().to_path_buf(),
        });

    let tool = RunCommand::with_limits(workspace, sender, limits);

    let task = tokio::spawn(async move {
        tool.execute(
            ToolCall::function(
                "test",
                "run_command",
                json!({
                    "command": "cat /opt/orbit-cache/leak_hardlink_cache/alias.key"
                }),
            ),
            CancellationToken::new(),
        )
        .await
    });

    let request = receiver.recv().await.unwrap();
    request.decision.send(true).unwrap();

    let result = task.await.unwrap();
    assert!(
        result.is_err(),
        "expected execution to fail due to hardlink to protected path"
    );
    match result {
        Err(orbit_agent::tool::ToolError::ExecutionFailed(msg)) => {
            assert!(
                msg.contains("contains hardlink to protected path"),
                "expected 'contains hardlink to protected path' error, got: {msg}"
            );
        }

        other => panic!("expected ExecutionFailed error, got {:?}", other),
    }
}

#[tokio::test]
async fn network_request_defaults_offline_and_cannot_exceed_policy() {
    let root = tempfile::tempdir().unwrap();
    let workspace = Workspace::new(root.path()).unwrap();
    let (sender, mut receiver) = approval::channel();
    let tool = sandbox_command(workspace.clone(), sender);
    let task = tokio::spawn(async move {
        tool.execute(
            ToolCall::function("network", "run_command", json!({"command":"pwd"})),
            CancellationToken::new(),
        )
        .await
    });
    let request = receiver.recv().await.unwrap();
    if let approval::ApprovalPreview::Command(command) = &request.preview {
        assert!(!command.network_allowed);
        assert!(approval::auto_allows(&request.preview, root.path()));
    } else {
        panic!("command preview expected");
    }

    request.decision.send(false).unwrap();
    assert!(task.await.unwrap().is_err());
    let mut offline = workspace;
    offline
        .configure_policy(
            Default::default(),
            orbit_runtime::capability::NetworkPolicy::Deny,
        )
        .unwrap();
    let (sender, mut receiver) = approval::channel();
    let tool = sandbox_command(offline, sender);
    assert!(
        tool.execute(
            ToolCall::function(
                "network",
                "run_command",
                json!({"command":"pwd", "network":"allow"})
            ),
            CancellationToken::new()
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("network access is disabled")
    );
    assert!(receiver.try_recv().is_err());
}

#[tokio::test]
async fn read_only_templates_and_their_aliases_stay_read_only_in_commands() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join(".env.example"), "EXAMPLE=1\n").unwrap();
    fs::hard_link(root.path().join(".env.example"), root.path().join("alias")).unwrap();
    let mut workspace = Workspace::new(root.path()).unwrap();
    workspace
        .configure_policy(
            orbit_runtime::policy::WorkspacePermissions {
                read_templates: vec![".env.example".into()],
                ..Default::default()
            },
            orbit_runtime::capability::NetworkPolicy::Deny,
        )
        .unwrap();
    let (sender, mut receiver) = approval::channel();
    let tool = sandbox_command(workspace, sender);
    let task = tokio::spawn(async move {
        tool.execute(ToolCall::function("template", "run_command", json!({"command":"cat /workspace/.env.example; echo changed > /workspace/alias; echo changed > /workspace/.env.example", "workspace_access":"read_write"})), CancellationToken::new()).await
    });
    receiver.recv().await.unwrap().decision.send(true).unwrap();
    let output = task.await.unwrap().unwrap();
    let value: serde_json::Value = serde_json::from_str(&output.content).unwrap();
    assert!(value["stdout"].as_str().unwrap().contains("EXAMPLE=1"));
    assert_ne!(value["exit_code"], 0);
    assert_eq!(
        fs::read_to_string(root.path().join(".env.example")).unwrap(),
        "EXAMPLE=1\n"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("alias")).unwrap(),
        "EXAMPLE=1\n"
    );
}

#[tokio::test]
async fn git_config_symlink_cannot_expose_a_protected_host_file() {
    let root = tempfile::tempdir().unwrap();
    let private = tempfile::tempdir().unwrap();
    let secret = private.path().join("credential");
    fs::write(&secret, "SECRET_AUTH_VALUE").unwrap();
    fs::create_dir(root.path().join(".git")).unwrap();
    std::os::unix::fs::symlink(&secret, root.path().join(".git/config")).unwrap();
    let mut workspace = Workspace::new(root.path()).unwrap();
    workspace.protect(&secret).unwrap();
    let (sender, mut receiver) = approval::channel();
    let tool = sandbox_command(workspace, sender);
    let task = tokio::spawn(async move {
        tool.execute(
            ToolCall::function(
                "config",
                "run_command",
                json!({"command":"cat /workspace/.git/config","workspace_access":"read_write"}),
            ),
            CancellationToken::new(),
        )
        .await
    });

    receiver.recv().await.unwrap().decision.send(true).unwrap();
    let error = task.await.unwrap().unwrap_err();
    assert!(error.to_string().contains("protected path"));
    assert!(!error.to_string().contains("SECRET_AUTH_VALUE"));
}
