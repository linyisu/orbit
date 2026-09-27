use std::{fs, time::Duration};

use orbit_agent::tool::{Tool, ToolError};
use orbit_model::types::ToolCall;
use orbit_tools::{
    approval::{self, ApprovalPreview},
    edit_file::EditFile,
    workspace::Workspace,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

fn start(
    root: &std::path::Path,
    args: Value,
    token: CancellationToken,
) -> (
    tokio::task::JoinHandle<Result<String, ToolError>>,
    approval::ApprovalReceiver,
) {
    let (tx, rx) = approval::channel();
    let tool = EditFile::new(Workspace::new(root).unwrap(), tx);
    let task = tokio::spawn(async move {
        tool.execute(ToolCall::function("edit", "edit_file", args), token)
            .await
    });

    (task, rx)
}

#[tokio::test]
async fn replaces_inserts_deletes_and_explicitly_replaces_all_without_normalizing_text() {
    for (before, old, new, all, after) in [
        (
            "前缀\r\n你好\r\n后缀",
            "你好",
            "您好",
            false,
            "前缀\r\n您好\r\n后缀",
        ),
        ("a\nb", "a\n", "a\n插入\n", false, "a\n插入\nb"),
        ("a\nb", "a\n", "", false, "b"),
        ("x x", "x", "y", true, "y y"),
        ("aaa", "aa", "b", true, "ba"),
    ] {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("file"), before).unwrap();

        let (task, mut rx) = start(
            root.path(),
            json!({ "path": "file", "old_text": old, "new_text": new, "replace_all": all }),
            CancellationToken::new(),
        );

        let request = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();
        let ApprovalPreview::Write(preview) = request.preview else {
            panic!("file preview")
        };
        assert_eq!(preview.before.as_deref(), Some(before));
        assert_eq!(preview.after, after);
        assert_eq!(
            fs::read_to_string(root.path().join("file")).unwrap(),
            before
        );

        request.decision.send(true).unwrap();

        let result: Value = serde_json::from_str(&task.await.unwrap().unwrap()).unwrap();
        assert_eq!(result["created"], false);
        assert_eq!(fs::read_to_string(root.path().join("file")).unwrap(), after);
    }
}

#[tokio::test]
async fn edit_output_carries_the_exact_committed_change_receipt() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file"), "one\n").unwrap();
    let (sender, mut approvals) = approval::channel();
    let tool = EditFile::new(Workspace::new(root.path()).unwrap(), sender);
    let task = tokio::spawn(async move {
        tool.execute_output(
            ToolCall::function(
                "edit-1",
                "edit_file",
                json!({ "path": "file", "old_text": "one", "new_text": "two" }),
            ),
            CancellationToken::new(),
        )
        .await
    });

    let approval = tokio::time::timeout(Duration::from_secs(2), approvals.recv())
        .await
        .unwrap()
        .unwrap();
    approval.decision.send(true).unwrap();
    let output = task.await.unwrap().unwrap();

    assert_eq!(output.changes.len(), 1);
    assert_eq!(output.changes[0].path, "file");
    assert_eq!(output.changes[0].before.as_deref(), Some("one\n"));
    assert_eq!(output.changes[0].after, "two\n");
    assert_eq!(
        fs::read_to_string(root.path().join("file")).unwrap(),
        "two\n"
    );
}

#[tokio::test]
async fn rejects_ambiguous_missing_invalid_noop_and_oversized_edits_before_approval() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file"), "aaa\r\nx x").unwrap();

    for args in [
        json!({ "path": "file", "old_text": "aa", "new_text": "b" }),
        json!({ "path": "file", "old_text": "x", "new_text": "y" }),
        json!({ "path": "file", "old_text": "absent", "new_text": "y" }),
        json!({ "path": "file", "old_text": "aaa\n", "new_text": "y" }),
        json!({ "path": "file", "old_text": "", "new_text": "y" }),
        json!({ "path": "file", "old_text": "aaa", "new_text": "aaa" }),
        json!({ "path": "file", "old_text": "aaa", "new_text": "\u{0}" }),
        json!({ "path": "file", "old_text": "x", "new_text": "z".repeat(20000), "replace_all": true }),
        json!({ "path": "missing", "old_text": "x", "new_text": "y" }),
        json!({ "path": "../file", "old_text": "x", "new_text": "y" }),
        json!({ "path": "file", "old_text": "aaa", "new_text": "y", "unknown": true }),
    ] {
        let (task, mut rx) = start(root.path(), args, CancellationToken::new());

        assert!(task.await.unwrap().is_err());
        assert!(rx.try_recv().is_err());
        assert_eq!(
            fs::read_to_string(root.path().join("file")).unwrap(),
            "aaa\r\nx x"
        );
    }
}

#[tokio::test]
async fn denial_cancellation_and_concurrent_changes_never_overwrite_the_file() {
    for action in ["deny", "cancel", "conflict"] {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("file"), "original").unwrap();

        let token = CancellationToken::new();
        let (task, mut rx) = start(
            root.path(),
            json!({ "path": "file", "old_text": "original", "new_text": "edited" }),
            token.clone(),
        );

        let request = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .unwrap()
            .unwrap();

        match action {
            "deny" => request.decision.send(false).unwrap(),
            "cancel" => token.cancel(),
            _ => {
                fs::write(root.path().join("file"), "external change").unwrap();
                request.decision.send(true).unwrap();
            }
        }

        assert!(task.await.unwrap().is_err());
        assert_eq!(
            fs::read_to_string(root.path().join("file")).unwrap(),
            if action == "conflict" {
                "external change"
            } else {
                "original"
            }
        );
    }
}

#[tokio::test]
async fn rejects_protected_files_and_links() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("config.toml"), "secret").unwrap();
    fs::write(root.path().join("file"), "text").unwrap();
    std::os::unix::fs::symlink("file", root.path().join("sym")).unwrap();
    fs::hard_link(root.path().join("file"), root.path().join("hard")).unwrap();

    for path in ["config.toml", "sym", "hard"] {
        let (task, mut rx) = start(
            root.path(),
            json!({ "path": path, "old_text": "text", "new_text": "changed" }),
            CancellationToken::new(),
        );

        assert!(task.await.unwrap().is_err());
        assert!(rx.try_recv().is_err());
    }
}
