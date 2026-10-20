use std::time::Duration;

use orbit_runtime::execution::{
    ExecutionEnvironment, ExecutionPolicy, ExecutionRequest, execution_backend,
};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn environment_is_the_single_source_of_command_working_directory() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("nested");
    std::fs::create_dir(&directory).unwrap();
    let mut environment = ExecutionEnvironment::new(root.path(), &directory).unwrap();
    configure_shell(&mut environment);
    let request = ExecutionRequest::new(
        native_command(
            "printf ready > marker",
            "[IO.File]::WriteAllText('marker', 'ready')",
        ),
        Duration::from_secs(5),
    );
    let result = execution_backend()
        .execute(
            &request,
            &ExecutionPolicy::default(),
            &environment,
            None,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(result.is_success(), "{result:?}");
    assert_eq!(std::fs::read(directory.join("marker")).unwrap(), b"ready");
    assert!(!root.path().join("marker").exists());
}

#[tokio::test]
async fn native_execution_uses_host_paths_and_retains_output() {
    let root = tempfile::tempdir().unwrap();
    let mut env = ExecutionEnvironment::new(root.path(), root.path()).unwrap();
    configure_shell(&mut env);
    env.set_var("HOME", root.path().to_string_lossy());
    let request = ExecutionRequest::new(
        native_command(
            "printf '%s' \"$HOME\"; printf 'saved' > result; /bin/sh -c 'printf child > child'; printf 'err' >&2",
            "[Console]::Write($env:HOME); [IO.File]::WriteAllText('result', 'saved'); & powershell.exe -NoProfile -NonInteractive -Command \"[IO.File]::WriteAllText('child', 'child')\"; [Console]::Error.Write('err')",
        ),
        Duration::from_secs(10),
    );
    let output = orbit_runtime::execution::OutputLog::create(&root.path().join("logs"))
        .await
        .unwrap();
    let policy = ExecutionPolicy::default();
    let result = execution_backend()
        .execute(
            &request,
            &policy,
            &env,
            Some(output),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert!(result.is_success(), "{result:?}");
    assert_eq!(result.sandbox, "native");
    assert_eq!(result.stdout, root.path().to_string_lossy());
    assert_eq!(result.stderr, "err");
    assert!(result.output_id.is_some());
    assert_eq!(std::fs::read(root.path().join("result")).unwrap(), b"saved");
    assert_eq!(std::fs::read(root.path().join("child")).unwrap(), b"child");
}

#[tokio::test]
async fn timeout_and_cancellation_stop_descendants_holding_output_pipes() {
    for cancel in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let mut env = ExecutionEnvironment::new(root.path(), root.path()).unwrap();
        configure_shell(&mut env);
        let token = CancellationToken::new();
        let signal = token.clone();
        let marker = root.path().join("started");
        let notifier = tokio::spawn(async move {
            if cancel {
                tokio::time::timeout(Duration::from_secs(3), async {
                    while !marker.exists() {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                })
                .await
                .expect("command must start before cancellation");
                signal.cancel();
            }
        });
        let request = ExecutionRequest::new(
            native_command(
                ": > started; sleep 30 & wait",
                "[IO.File]::WriteAllText('started', ''); & powershell.exe -NoProfile -NonInteractive -Command 'Start-Sleep 30'",
            ),
            if cancel {
                Duration::from_secs(10)
            } else {
                Duration::from_millis(500)
            },
        );
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            execution_backend()
                .execute(&request, &ExecutionPolicy::default(), &env, None, token)
                .await
                .unwrap()
        })
        .await
        .expect("descendant pipes must close promptly");
        notifier.await.unwrap();
        assert_eq!(result.cancelled, cancel);
        assert_eq!(result.timed_out, !cancel);
    }
}

#[tokio::test]
async fn aborting_execution_future_still_terminates_descendants() {
    let root = tempfile::tempdir().unwrap();
    let mut environment = ExecutionEnvironment::new(root.path(), root.path()).unwrap();
    configure_shell(&mut environment);
    let task = tokio::spawn(async move {
        let request = ExecutionRequest::new(
            native_command(
                "(printf ready > started; sleep 1; printf escaped > orphan) & wait",
                "& powershell.exe -NoProfile -NonInteractive -Command \"[IO.File]::WriteAllText('started', 'ready'); Start-Sleep 1; [IO.File]::WriteAllText('orphan', 'escaped')\"",
            ),
            Duration::from_secs(30),
        );
        execution_backend()
            .execute(
                &request,
                &ExecutionPolicy::default(),
                &environment,
                None,
                CancellationToken::new(),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while !root.path().join("started").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("descendant must start before aborting the supervisor");
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::sleep(Duration::from_millis(1_200)).await;
    assert!(!root.path().join("orphan").exists());
}

fn configure_shell(environment: &mut ExecutionEnvironment) {
    #[cfg(unix)]
    environment.set_var("SHELL", "/bin/sh");
    #[cfg(windows)]
    environment.set_var("SHELL", "powershell.exe");
}

fn native_command(unix: &str, windows: &str) -> String {
    if cfg!(windows) {
        windows.into()
    } else {
        unix.into()
    }
}

#[cfg(windows)]
#[tokio::test]
async fn unsupported_limits_are_rejected_before_process_creation() {
    let root = tempfile::tempdir().unwrap();
    let mut environment = ExecutionEnvironment::new(root.path(), root.path()).unwrap();
    configure_shell(&mut environment);
    let request = ExecutionRequest::new(
        "[IO.File]::WriteAllText('marker', 'started')",
        Duration::from_secs(5),
    );
    let mut policy = ExecutionPolicy::default();
    policy.resources.max_open_files = Some(1_024);
    let error = execution_backend()
        .execute(
            &request,
            &policy,
            &environment,
            None,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(error, orbit_runtime::error::RuntimeError::Io(error) if error.kind() == std::io::ErrorKind::Unsupported)
    );
    assert!(!root.path().join("marker").exists());
}

#[cfg(windows)]
#[tokio::test]
async fn native_cmd_preserves_operators_and_spaces_without_a_posix_shell() {
    let root = tempfile::tempdir().unwrap();
    let mut environment = ExecutionEnvironment::new(root.path(), root.path()).unwrap();
    environment.set_var("SHELL", "cmd.exe");
    let request = ExecutionRequest::new(
        "echo native-output & echo native-error 1>&2 & echo saved>\"file with spaces.txt\"",
        Duration::from_secs(5),
    );
    let result = execution_backend()
        .execute(
            &request,
            &ExecutionPolicy::default(),
            &environment,
            None,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(result.is_success(), "{result:?}");
    assert_eq!(result.stdout.trim(), "native-output");
    assert_eq!(result.stderr.trim(), "native-error");
    assert_eq!(
        std::fs::read_to_string(root.path().join("file with spaces.txt"))
            .unwrap()
            .trim(),
        "saved"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn native_windows_decodes_unicode_and_oem_output_without_encoding_probe_commands() {
    let root = tempfile::tempdir().unwrap();
    for (shell, script, expected) in [
        ("cmd.exe", "echo café", "café"),
        (
            "powershell.exe",
            concat!(
                "$b=[Text.Encoding]::Unicode.GetBytes('target=就绪🦀'); ",
                "[Console]::OpenStandardOutput().Write($b,0,$b.Length)",
            ),
            "target=就绪🦀",
        ),
    ] {
        let mut environment = ExecutionEnvironment::new(root.path(), root.path()).unwrap();
        environment.set_var("SHELL", shell);
        let request = ExecutionRequest::new(script, Duration::from_secs(10));
        let result = execution_backend()
            .execute(
                &request,
                &ExecutionPolicy::default(),
                &environment,
                None,
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert!(result.is_success(), "{result:?}");
        assert_eq!(result.stdout.trim(), expected);
    }
}
