use std::time::Duration;

use orbit_runtime::{
    Workspace,
    execution::{CommandSpec, run},
};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn command_uses_disposable_snapshot_without_protected_data() {
    let project = tempfile::tempdir().unwrap();
    std::fs::write(project.path().join("visible.txt"), "public\n").unwrap();
    std::fs::write(project.path().join("secret.txt"), "private\n").unwrap();
    std::fs::hard_link(
        project.path().join("secret.txt"),
        project.path().join("alias.txt"),
    )
    .unwrap();
    let mut workspace = Workspace::new(project.path()).unwrap();
    workspace
        .protect(project.path().join("secret.txt"))
        .unwrap();

    let result = run(
        workspace,
        CommandSpec {
            command: "cat visible.txt; test ! -e secret.txt; test ! -e alias.txt; touch /workspace/created".to_owned(),
            timeout: Duration::from_secs(10),
            rust_toolchain: None,
        },
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(
        result.exit_code,
        Some(0),
        "stdout: {}; stderr: {}",
        result.stdout,
        result.stderr
    );
    assert!(
        result.stdout.contains("public"),
        "stdout: {}; stderr: {}",
        result.stdout,
        result.stderr
    );
    assert!(!project.path().join("created").exists());
}

#[tokio::test]
async fn cancellation_stops_before_starting_a_command() {
    let project = tempfile::tempdir().unwrap();
    let workspace = Workspace::new(project.path()).unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let error = run(
        workspace,
        CommandSpec {
            command: "touch created".to_owned(),
            timeout: Duration::from_secs(10),
            rust_toolchain: None,
        },
        cancellation,
    )
    .await
    .unwrap_err();
    assert!(matches!(error, orbit_runtime::RuntimeError::Cancelled));
    assert!(!project.path().join("created").exists());
}

#[tokio::test]
async fn command_timeout_and_output_are_bounded() {
    let project = tempfile::tempdir().unwrap();
    let workspace = Workspace::new(project.path()).unwrap();

    let output = run(
        workspace.clone(),
        CommandSpec {
            command: "head -c 40000 /dev/zero".to_owned(),
            timeout: Duration::from_secs(10),
            rust_toolchain: None,
        },
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        output.exit_code,
        Some(0),
        "stdout: {}; stderr: {}",
        output.stdout,
        output.stderr
    );
    assert_eq!(output.stdout_total_bytes, 40_000);
    assert!(output.output_truncated);
    assert!(output.stdout.len() <= 32 * 1024);

    let timeout = run(
        workspace,
        CommandSpec {
            command: "sleep 2".to_owned(),
            timeout: Duration::from_millis(100),
            rust_toolchain: None,
        },
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert!(
        timeout.timed_out,
        "stdout: {}; stderr: {}",
        timeout.stdout, timeout.stderr
    );
}

#[tokio::test]
async fn explicit_toolchain_and_disposable_scratch_are_available() {
    use std::os::unix::fs::PermissionsExt;

    let project = tempfile::tempdir().unwrap();
    let toolchain = tempfile::tempdir().unwrap();
    std::fs::create_dir(toolchain.path().join("bin")).unwrap();
    for name in ["cargo", "rustc"] {
        let path = toolchain.path().join("bin").join(name);
        std::fs::write(&path, format!("#!/bin/sh\necho fake-{name}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    let output = run(
        Workspace::new(project.path()).unwrap(),
        CommandSpec {
            command: "rustc; echo build > /scratch/result; cat /scratch/result".to_owned(),
            timeout: Duration::from_secs(10),
            rust_toolchain: Some(toolchain.path().to_path_buf()),
        },
        CancellationToken::new(),
    )
    .await
    .unwrap();

    assert_eq!(
        output.exit_code,
        Some(0),
        "stdout: {}; stderr: {}",
        output.stdout,
        output.stderr
    );
    assert!(output.stdout.contains("fake-rustc"), "{}", output.stdout);
    assert!(output.stdout.contains("build"), "{}", output.stdout);
    assert!(!project.path().join("result").exists());
}

#[tokio::test]
async fn toolchain_inside_workspace_is_rejected() {
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir(project.path().join("bin")).unwrap();
    std::fs::write(project.path().join("bin/rustc"), "fake").unwrap();
    std::fs::write(project.path().join("bin/cargo"), "fake").unwrap();

    let error = run(
        Workspace::new(project.path()).unwrap(),
        CommandSpec {
            command: "touch /scratch/ran".to_owned(),
            timeout: Duration::from_secs(10),
            rust_toolchain: Some(project.path().to_path_buf()),
        },
        CancellationToken::new(),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("external directory"), "{error}");
}
