//! Native file transactions without Unix-only fixture helpers.

use std::{fs, path::Path, process::Command, time::Duration};

use orbit_agent::tool::Tool;
use orbit_model::types::ToolCall;
use orbit_runtime::filesystem::Workspace;
use orbit_tools::{
    approval,
    git_inspect::{review_index_patch, update_review_index},
    write_file::WriteFile,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[tokio::test]
async fn approved_native_writes_preserve_content_and_report_completed_durability() {
    let directory = tempfile::tempdir().unwrap();
    for before in [None, Some("initial\r\n")] {
        let (sender, mut receiver) = approval::channel();
        let tool = WriteFile::new(Workspace::new(directory.path()).unwrap(), sender);
        let call = ToolCall::function(
            "native-write",
            "WriteFile",
            json!({
                "path": "document.txt",
                "content": if before.is_none() { "initial\r\n" } else { "updated\r\n" }
            }),
        );
        let task = tokio::spawn(async move {
            tool.execute(call, CancellationToken::new())
                .await
        });
        let request = tokio::time::timeout(Duration::from_secs(10), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            fs::read_to_string(directory.path().join("document.txt"))
                .ok()
                .as_deref(),
            before
        );
        request.decision.send(true).unwrap();
        let output = task.await.unwrap().unwrap();
        let result: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(result["directory_synced"], true, "{result}");
        assert_eq!(
            fs::read_to_string(directory.path().join("document.txt")).unwrap(),
            output.changes[0].after
        );
        assert!(
            fs::read_dir(directory.path())
                .unwrap()
                .all(|entry| !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".orbit-write-"))
        );
    }
}

#[test]
fn native_index_transactions_preserve_raw_bytes_and_unrelated_entries() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.name", "Native fixture"]);
    git(root, &["config", "user.email", "native@example.invalid"]);
    git(root, &["config", "core.autocrlf", "false"]);
    fs::write(root.join("tracked.txt"), "base\n").unwrap();
    fs::write(root.join("other.txt"), "other\n").unwrap();
    git(root, &["add", "."]);
    git(root, &["commit", "-qm", "fixture"]);
    fs::write(root.join("tracked.txt"), "changed\r\n").unwrap();
    fs::write(root.join("other.txt"), "other staged\n").unwrap();
    git(root, &["add", "other.txt"]);
    let unrelated = git(root, &["show", ":other.txt"]);
    let workspace = Workspace::new(root).unwrap();
    let reviewed = review_index_patch(&workspace, "tracked.txt", false, false).unwrap();
    assert!(
        reviewed.patch.contains("+changed\r\n"),
        "{}",
        reviewed.patch
    );
    let receipt = reviewed
        .index_receipt
        .unwrap_or_else(|| panic!("{:?}", reviewed.index_note));
    update_review_index(
        &workspace,
        "tracked.txt",
        &receipt,
        true,
        CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(git(root, &["show", ":tracked.txt"]), b"changed\r\n");
    assert_eq!(git(root, &["show", ":other.txt"]), unrelated);
    let reviewed = review_index_patch(&workspace, "tracked.txt", true, false).unwrap();
    update_review_index(
        &workspace,
        "tracked.txt",
        &reviewed.index_receipt.unwrap(),
        false,
        CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(git(root, &["show", ":tracked.txt"]), b"base\n");
    assert_eq!(fs::read(root.join("tracked.txt")).unwrap(), b"changed\r\n");
    assert_eq!(git(root, &["show", ":other.txt"]), unrelated);
    assert!(!root.join(".git/index.lock").exists());
}

#[cfg(windows)]
#[test]
fn windows_staging_keeps_the_index_executable_mode() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    git(root, &["init", "-q"]);
    git(root, &["config", "core.filemode", "false"]);
    git(root, &["config", "core.autocrlf", "false"]);
    fs::write(root.join("script.txt"), "before\n").unwrap();
    git(root, &["add", "script.txt"]);
    git(root, &["update-index", "--chmod=+x", "script.txt"]);
    fs::write(root.join("script.txt"), "after\n").unwrap();
    let workspace = Workspace::new(root).unwrap();
    let reviewed = review_index_patch(&workspace, "script.txt", false, false).unwrap();
    assert!(!reviewed.patch.contains("old mode"));
    assert!(reviewed.patch.contains("100755"), "{}", reviewed.patch);
    update_review_index(
        &workspace,
        "script.txt",
        &reviewed.index_receipt.unwrap(),
        true,
        CancellationToken::new(),
    )
    .unwrap();
    assert!(
        String::from_utf8(git(root, &["ls-files", "--stage", "script.txt"]))
            .unwrap()
            .starts_with("100755 ")
    );
}
