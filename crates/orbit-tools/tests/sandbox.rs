use std::{fs, os::unix::fs::symlink, time::Duration};

use orbit_agent::tool::Tool;
use orbit_model::types::ToolCall;
use orbit_tools::{approval, run_command::RunCommand, sandbox, workspace::Workspace};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn fixture() -> (tempfile::TempDir, Workspace) {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("source.txt"), "hello sandbox\n").unwrap();
    let workspace = Workspace::new(root.path()).unwrap();
    (root, workspace)
}

#[tokio::test]
async fn command_denial_and_cancellation_do_not_start_a_process() {
    let (_root, workspace) = fixture();
    let (sender, mut receiver) = approval::channel();
    let tool = RunCommand::new(workspace.clone(), sender);
    let task = tokio::spawn(async move {
        tool.execute(
            ToolCall::function("run", "run_command", json!({"command":"echo no"})),
            CancellationToken::new(),
        )
        .await
    });
    let request = receiver.recv().await.unwrap();
    assert!(matches!(
        request.preview,
        approval::ApprovalPreview::Command(_)
    ));
    request.decision.send(false).unwrap();
    assert!(
        task.await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("denied")
    );
    let (sender, mut receiver) = approval::channel();
    let tool = RunCommand::new(workspace, sender);
    let token = CancellationToken::new();
    let clone = token.clone();
    let task = tokio::spawn(async move {
        tool.execute(
            ToolCall::function("run", "run_command", json!({"command":"echo no"})),
            clone,
        )
        .await
    });
    let _request = receiver.recv().await.unwrap();
    token.cancel();
    assert!(task.await.unwrap().is_err());
}

#[tokio::test]
async fn validates_command_parameters_before_approval() {
    let (_root, workspace) = fixture();
    let (sender, mut receiver) = approval::channel();
    let tool = RunCommand::new(workspace, sender);
    for args in [
        json!({"command":""}),
        json!({"command":"echo hi","timeout_seconds":0}),
        json!({"command":"echo hi","timeout_seconds":601}),
        json!({"command":"echo hi","writable_snapshot":"yes"}),
        json!({"command":"x\u{0000}"}),
        json!({"command":"echo hi","unknown":true}),
    ] {
        assert!(
            tool.execute(
                ToolCall::function("run", "run_command", args),
                CancellationToken::new()
            )
            .await
            .is_err()
        );
        assert!(receiver.try_recv().is_err());
    }
}

#[tokio::test]
#[ignore = "requires Linux user namespaces; run explicitly outside the outer tool sandbox"]
async fn reads_snapshot_but_cannot_write_project_or_read_protected_files() {
    let (root, mut workspace) = fixture();
    fs::write(root.path().join(".env"), "secret").unwrap();
    fs::create_dir(root.path().join("nested")).unwrap();
    fs::write(root.path().join("nested/.env.local"), "secret").unwrap();
    fs::write(root.path().join("private.db"), "secret").unwrap();
    workspace.protect(root.path().join("private.db")).unwrap();
    fs::hard_link(root.path().join("private.db"), root.path().join("alias")).unwrap();
    symlink("/etc/passwd", root.path().join("outside")).unwrap();
    fs::create_dir(root.path().join(".git")).unwrap();
    fs::write(root.path().join(".git/config"), "secret").unwrap();
    let result = sandbox::run(workspace,"pwd; cat source.txt; test ! -e .env && test ! -e private.db && test ! -e alias && test ! -e outside && test ! -e .git && test ! -e nested/.env.local || exit 9; echo changed > source.txt; test \"$(cat source.txt)\" = 'hello sandbox'".into(),Duration::from_secs(10),CancellationToken::new()).await.unwrap();
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert!(result.stdout.contains("/workspace\nhello sandbox"));
    assert!(result.stderr.contains("Read-only file system"));
    assert_eq!(
        fs::read_to_string(root.path().join("source.txt")).unwrap(),
        "hello sandbox\n"
    );
    assert!(result.skipped_entries >= 6);
}

#[tokio::test]
#[ignore = "requires Linux user namespaces"]
async fn isolates_host_files_network_environment_and_processes() {
    let (root, workspace) = fixture();
    let secret = tempfile::NamedTempFile::new().unwrap();
    fs::write(secret.path(), "host secret").unwrap();
    let script = format!(
        "test ! -e '{}' && test ! -e '{}' && test ! -e /etc/passwd || exit 7; test \"$HOME\" = /tmp/home || exit 8; test -z \"$HTTP_PROXY$HTTPS_PROXY$SSH_AUTH_SOCK$orbit_config\" || exit 9; python3 -c 'import socket; s=socket.socket(); s.settimeout(0.2);\ntry: s.connect((\"1.1.1.1\",443))\nexcept OSError: print(\"network blocked\")\nelse: raise SystemExit(42)'",
        secret.path().display(),
        root.path().display()
    );
    let result = sandbox::run(
        workspace,
        script,
        Duration::from_secs(10),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert!(result.stdout.contains("network blocked"));
}

#[tokio::test]
#[ignore = "requires Linux user namespaces"]
async fn timeout_stops_background_descendants_and_closes_pipes() {
    let (_root, workspace) = fixture();
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        sandbox::run(
            workspace,
            "sh -c 'sleep 60' & wait".into(),
            Duration::from_millis(150),
            CancellationToken::new(),
        ),
    )
    .await
    .expect("must terminate descendants")
    .unwrap();
    assert!(result.timed_out, "{result:?}");
}

#[tokio::test]
#[ignore = "requires Linux user namespaces"]
async fn cancellation_stops_a_running_command() {
    let (_root, workspace) = fixture();
    let token = CancellationToken::new();
    let clone = token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        clone.cancel();
    });
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        sandbox::run(workspace, "sleep 60".into(), Duration::from_secs(60), token),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(result.cancelled, "{result:?}");
}

#[tokio::test]
#[ignore = "requires Linux user namespaces"]
async fn verbose_output_does_not_kill_commands_and_preserves_final_status() {
    let (_root, workspace) = fixture();
    let result = sandbox::run(
        workspace.clone(),
        r#"python3 -c 'print("x" * 100000)'; printf FINAL_OK; exit 7"#.into(),
        Duration::from_secs(5),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(result.output_truncated);
    assert!(result.stdout.len() <= 32768);
    assert_eq!(result.exit_code, Some(7));
    assert!(result.stdout.ends_with("FINAL_OK"));
    assert_eq!(result.stdout_total_bytes, 100009);
    let result = sandbox::run(
        workspace,
        "printf out; printf err >&2; exit 7".into(),
        Duration::from_secs(5),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(result.exit_code, Some(7));
    assert_eq!(result.stdout, "out");
    assert_eq!(result.stderr, "err");
}

#[tokio::test]
#[ignore = "requires Linux user namespaces and /usr/bin/cc"]
async fn writable_snapshot_builds_without_changing_host_and_discards_outputs() {
    let (root, workspace) = fixture();
    fs::write(
        root.path().join("main.c"),
        "#include <stdio.h>\nint main(void) { puts(\"built safely\"); return 0; }\n",
    )
    .unwrap();
    fs::write(root.path().join(".env"), "private").unwrap();
    let result = sandbox::run_with_options(workspace.clone(),
        "set -e; mkdir build; cc main.c -o build/program; ./build/program; echo edited > source.txt; test ! -e .env; test ! -e /home; test ! -e /orbit-source/.env; test $(stat -f -c %T /workspace) = tmpfs; echo changed > /orbit-source/source.txt".into(),
        Duration::from_secs(20), CancellationToken::new(), true).await.unwrap();
    assert_ne!(result.exit_code, Some(0), "{result:?}"); // Attempt to modify the source mount fails.
    assert!(result.stdout.contains("built safely"), "{result:?}");
    assert!(
        result.stderr.contains("Read-only file system"),
        "{result:?}"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("source.txt")).unwrap(),
        "hello sandbox\n"
    );
    assert!(!root.path().join("build").exists());
    let second = sandbox::run_with_options(
        workspace,
        "test ! -e build && test \"$(cat source.txt)\" = 'hello sandbox'".into(),
        Duration::from_secs(10),
        CancellationToken::new(),
        true,
    )
    .await
    .unwrap();
    assert_eq!(second.exit_code, Some(0));
}

#[tokio::test]
#[ignore = "requires Linux user namespaces"]
async fn writable_workspace_has_a_total_size_limit() {
    let (_root, workspace) = fixture();
    let result = sandbox::run_with_options(workspace,
        "dd if=/dev/zero of=one bs=1M count=48 2>/dev/null; dd if=/dev/zero of=two bs=1M count=48 2>/dev/null; dd if=/dev/zero of=three bs=1M count=48".into(),
        Duration::from_secs(15), CancellationToken::new(), true).await.unwrap();
    assert_ne!(result.exit_code, Some(0));
    assert!(
        result.stderr.contains("No space left on device"),
        "{result:?}"
    );
}

#[tokio::test]
async fn configured_command_timeout_reaches_approval_and_rejects_excess() {
    use orbit_tools::approval::ApprovalPreview;
    use orbit_tools::run_command::CommandLimits;
    let (_root, workspace) = fixture();
    let (tx, mut rx) = approval::channel();
    let tool = RunCommand::with_limits(
        workspace,
        tx,
        CommandLimits {
            default_timeout_seconds: 300,
            max_timeout_seconds: 1800,
        },
    );
    assert!(
        tool.execute(
            ToolCall::function(
                "bad",
                "run_command",
                json!({"command":"true", "timeout_seconds":1801})
            ),
            CancellationToken::new()
        )
        .await
        .is_err()
    );
    let task = tokio::spawn(async move {
        tool.execute(
            ToolCall::function("run", "run_command", json!({"command":"true"})),
            CancellationToken::new(),
        )
        .await
    });
    let request = rx.recv().await.unwrap();
    let ApprovalPreview::Command(preview) = request.preview else {
        panic!("command preview")
    };
    assert_eq!(preview.timeout_seconds, 300);
    request.decision.send(false).unwrap();
    assert!(task.await.unwrap().is_err());
    assert!(
        CommandLimits {
            default_timeout_seconds: 601,
            max_timeout_seconds: 600
        }
        .validate()
        .is_err()
    );
}

#[tokio::test]
#[ignore = "requires Linux user namespaces"]
async fn process_limits_follow_the_command_budget() {
    let (_root, workspace) = fixture();
    let result = sandbox::run(
        workspace,
        "ulimit -t; ulimit -v; ulimit -n".into(),
        Duration::from_secs(600),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(result.exit_code, Some(0), "{result:?}");
    assert_eq!(result.stdout, "601\n4194304\n1024\n");
}

#[tokio::test]
#[ignore = "requires Linux user namespaces"]
async fn commands_larger_than_os_argument_limit_run_from_private_script() {
    for writable in [false, true] {
        let (root, workspace) = fixture();
        let script = format!("# {}\nprintf LONG_COMMAND_OK", "中文".repeat(40_000));
        let result = sandbox::run_with_options(
            workspace,
            script,
            Duration::from_secs(10),
            CancellationToken::new(),
            writable,
        )
        .await
        .unwrap();
        assert_eq!(result.exit_code, Some(0), "{result:?}");
        assert_eq!(result.stdout, "LONG_COMMAND_OK");
        assert!(!root.path().join("orbit-command.sh").exists());
    }
}

#[tokio::test]
#[ignore = "requires Linux user namespaces"]
async fn infinite_output_is_bounded_and_still_obeys_timeout() {
    let (_root, workspace) = fixture();
    let result = sandbox::run(
        workspace,
        "yes output".into(),
        Duration::from_millis(200),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(result.timed_out);
    assert!(result.output_truncated);
    assert!(result.stdout.len() <= 32768);
}

#[tokio::test]
async fn large_commands_reach_approval_without_an_arbitrary_tool_size_limit() {
    let (_root, workspace) = fixture();
    let (tx, mut rx) = approval::channel();
    let tool = RunCommand::new(workspace, tx);
    let command = format!("# {}\nprintf ok", "x".repeat(200_000));
    let expected = command.clone();
    let task = tokio::spawn(async move {
        tool.execute(
            ToolCall::function("long", "run_command", json!({"command":command})),
            CancellationToken::new(),
        )
        .await
    });
    let request = tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    let orbit_tools::approval::ApprovalPreview::Command(preview) = request.preview else {
        panic!("command preview");
    };
    assert_eq!(preview.command, expected);
    request.decision.send(false).unwrap();
    assert!(
        task.await
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("denied")
    );
}
