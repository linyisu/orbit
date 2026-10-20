use std::{io::Write, process::Stdio, time::Duration};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};

use super::ProcessGroup;

const FIXTURE: &str = "execution::process::tests::process_tree_fixture";
const ROLE: &str = "ORBIT_PROCESS_TEST_ROLE";
const TEST_TIMEOUT: Duration = Duration::from_secs(10);

// Invoked only by the parent tests with independent generated input.
#[test]
#[ignore]
fn process_tree_fixture() {
    let Ok(role) = std::env::var(ROLE) else {
        return;
    };
    #[cfg(windows)]
    if role == "console" {
        // SAFETY: querying the current process console has no pointer arguments.
        assert!(unsafe { windows_sys::Win32::System::Console::GetConsoleWindow() }.is_null());
        println!("NO_CONSOLE");
        return;
    }
    if role == "descendant" {
        std::io::stdout()
            .write_all(b"DESCENDANT_READY\n")
            .unwrap();
        std::io::stdout().flush().unwrap();
        std::thread::sleep(Duration::from_secs(30));
        return;
    }
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", FIXTURE, "--nocapture"])
        .env(ROLE, "descendant")
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    if role == "exit" {
        // The descendant retains stdout after its group leader exits.
        std::process::exit(0);
    }
    child.wait().unwrap();
}

async fn tree(
    role: &str,
) -> (
    tokio::process::Child,
    ProcessGroup,
    BufReader<tokio::process::ChildStdout>,
) {
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--ignored", "--exact", FIXTURE, "--nocapture"])
        .env(ROLE, role)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let (mut child, group) = ProcessGroup::spawn(&mut command).unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    tokio::time::timeout(TEST_TIMEOUT, async {
        loop {
            let mut line = String::new();
            assert_ne!(
                output
                    .read_line(&mut line)
                    .await
                    .unwrap(),
                0,
                "fixture exited before readiness"
            );
            if line.trim() == "DESCENDANT_READY" {
                break;
            }
        }
    })
    .await
    .unwrap();
    (child, group, output)
}

#[tokio::test]
async fn dropping_group_closes_descendant_pipes() {
    let (mut child, group, mut output) = tree("wait").await;
    drop(group);
    tokio::time::timeout(TEST_TIMEOUT, async {
        child.wait().await.unwrap();
        output
            .read_to_end(&mut Vec::new())
            .await
            .unwrap();
    })
    .await
    .expect("descendants must be terminated with their owner");
}

#[tokio::test]
async fn group_still_owns_descendants_after_leader_exit() {
    let (mut child, mut group, mut output) = tree("exit").await;
    assert!(
        tokio::time::timeout(TEST_TIMEOUT, child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    group.kill();
    group.kill();
    tokio::time::timeout(TEST_TIMEOUT, output.read_to_end(&mut Vec::new()))
        .await
        .expect("exited leader must not detach descendants")
        .unwrap();
}

#[tokio::test]
async fn spawn_failure_does_not_create_an_owner() {
    let root = tempfile::tempdir().unwrap();
    let mut command = tokio::process::Command::new(root.path().join("missing-program"));
    assert!(ProcessGroup::spawn(&mut command).is_err());
}

#[cfg(windows)]
#[tokio::test]
async fn owned_background_process_does_not_allocate_a_console_window() {
    let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--ignored", "--exact", FIXTURE, "--nocapture"])
        .env(ROLE, "console")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (child, _group) = ProcessGroup::spawn(&mut command).unwrap();
    let result = tokio::time::timeout(TEST_TIMEOUT, child.wait_with_output())
        .await
        .unwrap()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("NO_CONSOLE"));
}
