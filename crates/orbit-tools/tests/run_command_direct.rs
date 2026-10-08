use orbit_agent::tool::Tool;
use orbit_model::types::ToolCall;
use orbit_runtime::filesystem::Workspace;
use orbit_tools::{
    approval,
    run_command::{CommandLimits, RunCommand},
};
use serde_json::json;
use std::fs;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn auto_native_network_enabled_script_executes_in_requested_workspace_directory() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("src")).unwrap();
    let (sender, mut receiver) = approval::channel();
    let tool = RunCommand::with_limits(
        Workspace::new(root.path()).unwrap(),
        sender,
        CommandLimits::default(),
    );
    let task = tokio::spawn(async move {
        tool.execute(ToolCall::function("script", "run_command", json!({
            "command": "for value in first second; do printf '%s\\n' \"$value\"; done > result.txt; pwd",
            "cwd": "src", "workspace_access": "read_write", "network": "allow"
        })), CancellationToken::new()).await
    });
    let request = tokio::time::timeout(std::time::Duration::from_secs(5), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(&request.preview, approval::ApprovalPreview::Command(command) if command.network_allowed)
    );
    let allow = approval::auto_allows(&request.preview, root.path());
    request.decision.send(allow).unwrap();
    assert!(allow);
    let output = task.await.unwrap().unwrap();
    let value: serde_json::Value = serde_json::from_str(&output.content).unwrap();
    assert_eq!(value["exit_code"], 0, "{value}");
    assert_eq!(
        fs::read_to_string(root.path().join("src/result.txt")).unwrap(),
        "first\nsecond\n"
    );
    assert!(!root.path().join("result.txt").exists());
}

#[tokio::test]
async fn auto_literal_text_pipeline_creates_and_appends_workspace_files_without_a_dialog() {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("existing")).unwrap();
    let (sender, mut receiver) = approval::channel();
    let tool = RunCommand::new(Workspace::new(root.path()).unwrap(), sender);
    let task = tokio::spawn(async move {
        tool.execute(ToolCall::function("text", "run_command", json!({
            "command": "mkdir -p demo && echo 'literal $(touch unexpected) | >' > demo/text.txt && printf '%s\\n' 'second line' | tee -a demo/text.txt ../mirror.txt > demo/output.txt && cat demo/text.txt | tee demo/copy.txt",
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
    let tool = RunCommand::new(workspace, sender);
    let task = tokio::spawn(async move {
        tool.execute(
            ToolCall::function(
                "mkdir",
                "run_command",
                json!({
                    "command": "mkdir -p 'demo folder/nested' ../another",
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
    let tool = RunCommand::new(workspace.clone(), sender);

    let task = tokio::spawn(async move {
        tool.execute(
            ToolCall::function(
                "test",
                "run_command",
                json!({ "command": "echo 'direct execution' > output.txt", "workspace_access": "read_write" }),
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
    assert!(evidence.isolation.contains("native user process"));

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
async fn run_command_reports_when_workspace_fingerprint_is_unavailable() {
    let ws_temp = tempfile::tempdir().unwrap();
    fs::write(
        ws_temp.path().join("large.txt"),
        vec![b'x'; 2 * 1024 * 1024],
    )
    .unwrap();
    let workspace = Workspace::new(ws_temp.path()).unwrap();
    let limits = CommandLimits {
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
    let tool = RunCommand::new(workspace.clone(), sender);

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
        sub.to_str().unwrap()
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
    let tool = RunCommand::new(workspace, sender);

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
async fn network_request_defaults_offline_and_cannot_exceed_policy() {
    let root = tempfile::tempdir().unwrap();
    let workspace = Workspace::new(root.path()).unwrap();
    let (sender, mut receiver) = approval::channel();
    let tool = RunCommand::new(workspace.clone(), sender);
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
    let tool = RunCommand::new(offline, sender);
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
async fn run_command_uses_project_git_configuration_for_status_and_commit() {
    let ws_temp = tempfile::tempdir().unwrap();
    let workspace = Workspace::new(ws_temp.path()).unwrap();

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
    let tool = RunCommand::new(workspace, sender);

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
