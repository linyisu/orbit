use std::{fs, time::Duration};

use orbit_agent::tool::{Tool, ToolError, ToolOutput};
use orbit_model::types::ToolCall;
use orbit_runtime::filesystem::Workspace;
use orbit_tools::{
    approval::{self, ApprovalPreview},
    edit_file::EditFile,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn batch_edits_commit_once_and_failure_leaves_the_original_file() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("module.rs");
    fs::write(&path, "alpha\nbeta\n").unwrap();
    let (task, mut approvals) = start(
        root.path(),
        json!({
            "path": "module.rs", "edits": [
                { "old_text": "alpha", "new_text": "gamma" },
                { "old_text": "gamma\nbeta", "new_text": "gamma\ndelta" }
            ]
        }),
        CancellationToken::new(),
    );
    approvals
        .recv()
        .await
        .unwrap()
        .decision
        .send(true)
        .unwrap();
    let output = task.await.unwrap().unwrap();
    assert_eq!(output.changes.len(), 1);
    let receipt: Value = serde_json::from_str(&output.content).unwrap();
    assert!(
        receipt["diff"]
            .as_str()
            .unwrap()
            .contains("-alpha")
    );
    assert!(
        receipt["diff"]
            .as_str()
            .unwrap()
            .contains("+gamma")
    );
    assert_eq!(receipt["diff_truncated"], false);
    assert_eq!(receipt["edits_applied"], 2);
    assert_eq!(receipt["inserted_lines"], 2);
    assert_eq!(receipt["deleted_lines"], 2);
    assert_eq!(output.changes[0].before.as_deref(), Some("alpha\nbeta\n"));
    assert_eq!(fs::read_to_string(&path).unwrap(), "gamma\ndelta\n");
    let (task, mut approvals) = start(
        root.path(),
        json!({
            "path": "module.rs", "edits": [
                { "old_text": "gamma", "new_text": "changed" },
                { "old_text": "missing", "new_text": "never" }
            ]
        }),
        CancellationToken::new(),
    );
    assert!(task.await.unwrap().is_err());
    assert!(approvals.try_recv().is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "gamma\ndelta\n");
}

fn start(
    root: &std::path::Path,
    args: Value,
    token: CancellationToken,
) -> (
    tokio::task::JoinHandle<Result<ToolOutput, ToolError>>,
    approval::ApprovalReceiver,
) {
    let (tx, rx) = approval::channel();
    let tool = EditFile::new(Workspace::new(root).unwrap(), tx);
    let task = tokio::spawn(async move {
        tool.execute(ToolCall::function("edit", "EditFile", args), token)
            .await
    });

    (task, rx)
}

#[tokio::test]
async fn large_committed_diff_is_bounded_without_truncating_the_file_receipt() {
    let root = tempfile::tempdir().unwrap();
    let before = "old line\n".repeat(10_000);
    let after = "new line\n".repeat(10_000);
    fs::write(root.path().join("large.txt"), &before).unwrap();
    let (task, mut approvals) = start(
        root.path(),
        json!({
            "path": "large.txt", "old_text": before, "new_text": after
        }),
        CancellationToken::new(),
    );
    approvals
        .recv()
        .await
        .unwrap()
        .decision
        .send(true)
        .unwrap();
    let output = task.await.unwrap().unwrap();
    let receipt: Value = serde_json::from_str(&output.content).unwrap();
    assert_eq!(receipt["diff_truncated"], true);
    assert!(receipt["diff"].as_str().unwrap().len() <= 16 * 1_024);
    assert_eq!(output.changes[0].after, after);
    assert_eq!(
        fs::read_to_string(root.path().join("large.txt")).unwrap(),
        after
    );
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
            json!({
                "path": "file",
                "old_text": old,
                "new_text": new,
                "replace_all": all
            }),
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
        let output = task.await.unwrap().unwrap();

        assert_eq!(output.changes.len(), 1);
        assert_eq!(output.changes[0].path, "file");
        assert_eq!(output.changes[0].before.as_deref(), Some(before));
        assert_eq!(output.changes[0].after, after);

        let result: Value = serde_json::from_str(&output).unwrap();

        assert_eq!(result["created"], false);
        assert_eq!(fs::read_to_string(root.path().join("file")).unwrap(), after);
    }
}

#[tokio::test]
async fn rejects_ambiguous_missing_invalid_noop_and_oversized_edits_before_approval() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file"), "aaa\r\nx x").unwrap();

    for args in [
        json!({
            "path": "file",
            "old_text": "aa",
            "new_text": "b"
        }),
        json!({
            "path": "file",
            "old_text": "x",
            "new_text": "y"
        }),
        json!({
            "path": "file",
            "old_text": "absent",
            "new_text": "y"
        }),
        json!({
            "path": "file",
            "old_text": "aaa\n",
            "new_text": "y"
        }),
        json!({
            "path": "file",
            "old_text": "",
            "new_text": "y"
        }),
        json!({
            "path": "file",
            "old_text": "aaa",
            "new_text": "aaa"
        }),
        json!({
            "path": "file",
            "old_text": "aaa",
            "new_text": "\u{0}"
        }),
        json!({
            "path": "file",
            "old_text": "x",
            "new_text": "z".repeat(600_000),
            "replace_all": true
        }),
        json!({
            "path": "missing",
            "old_text": "x",
            "new_text": "y"
        }),
        json!({
            "path": "../file",
            "old_text": "x",
            "new_text": "y"
        }),
        json!({
            "path": "file",
            "old_text": "aaa",
            "new_text": "y",
            "unknown": true
        }),
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
async fn failed_edits_return_bounded_current_text_without_approval_or_modification() {
    let root = tempfile::tempdir().unwrap();
    let before = "// 中文\r\n    fn parse() {\r\n        finish();\r\n    }\r\n";
    fs::write(root.path().join("file"), before).unwrap();
    for (old, code) in [
        ("fn parse() {\n  finish();\n}", "text_not_found"),
        (" ", "ambiguous_match"),
    ] {
        let (task, mut rx) = start(
            root.path(),
            json!({
                "path": "file",
                "old_text": old,
                "new_text": "replacement"
            }),
            CancellationToken::new(),
        );
        let ToolError::ExecutionFailed(error) = task.await.unwrap().unwrap_err() else {
            panic!("match diagnostic")
        };
        let feedback: Value = serde_json::from_str(&error).unwrap();
        assert_eq!(feedback["error"], code);
        if code == "text_not_found" {
            assert_eq!(feedback["exact_matches"], 0);
        }
        let candidates = feedback["candidates"]
            .as_array()
            .unwrap();
        assert!(!candidates.is_empty() && candidates.len() <= 3);
        if code == "text_not_found" {
            assert_eq!(candidates[0]["line"], 2);
            assert!(
                candidates[0]["current_text"]
                    .as_str()
                    .unwrap()
                    .contains("        finish();\r\n")
            );
        }
        assert!(error.len() < 5_000);
        assert!(rx.try_recv().is_err());
        assert_eq!(
            fs::read_to_string(root.path().join("file")).unwrap(),
            before
        );
    }
}

#[tokio::test]
async fn mismatch_diagnostics_locate_long_blocks_and_inline_changes_without_writing() {
    let block = (0..150)
        .map(|n| format!("    process_{n}(value);\n"))
        .collect::<String>();
    let changed = block.replace("process_93(value)", "process_93(other)");
    for (before, old, line, column) in [
        (format!("// prefix\n{changed}// suffix\n"), block, 95, 16),
        (
            "结果 = parse(\"数据\",  other);\n".into(),
            "结果 = parse(\"数据\", other);".into(),
            1,
            18,
        ),
        (
            "unicode_甲甲甲甲甲甲甲甲();\n".into(),
            "unicode_甲乙甲甲甲甲甲甲();".into(),
            1,
            10,
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("file"), &before).unwrap();
        let (task, mut rx) = start(
            root.path(),
            json!({
                "path": "file",
                "old_text": old,
                "new_text": "replacement"
            }),
            CancellationToken::new(),
        );
        let ToolError::ExecutionFailed(error) = task.await.unwrap().unwrap_err() else {
            panic!("diagnostic expected")
        };
        let feedback: Value = serde_json::from_str(&error).unwrap();
        let candidate = &feedback["candidates"][0];
        assert!(candidate.is_object(), "{feedback}");
        assert_eq!(candidate["first_difference"]["line"], line);
        assert_eq!(candidate["first_difference"]["column"], column);
        assert!(error.len() < 6_000);
        assert!(rx.try_recv().is_err());
        assert_eq!(
            fs::read_to_string(root.path().join("file")).unwrap(),
            before
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
            json!({
                "path": "file",
                "old_text": "original",
                "new_text": "edited"
            }),
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

        let error = task.await.unwrap().unwrap_err();
        match action {
            "deny" => assert!(matches!(error, ToolError::PermissionDenied(_))),
            "cancel" => assert_eq!(error, ToolError::Cancelled),
            _ => assert!(matches!(error, ToolError::Conflict(_))),
        }
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
async fn rejects_non_atomic_link_edits() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("file"), "text").unwrap();
    std::os::unix::fs::symlink("file", root.path().join("sym")).unwrap();
    fs::hard_link(root.path().join("file"), root.path().join("hard")).unwrap();
    let workspace = Workspace::new(root.path()).unwrap();
    let (sender, mut approvals) = approval::channel();
    let tool = EditFile::new(workspace, sender);

    for path in ["sym", "hard"] {
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            tool.execute(
                ToolCall::function(
                    "edit",
                    "EditFile",
                    json!({
                        "path": path,
                        "old_text": "text",
                        "new_text": "changed"
                    }),
                ),
                CancellationToken::new(),
            ),
        )
        .await
        .expect("non-atomic edit unexpectedly requested approval");
        assert!(result.is_err(), "{path}");
        assert!(approvals.try_recv().is_err());
        assert_eq!(fs::read_to_string(root.path().join(path)).unwrap(), "text");
    }
}

#[tokio::test]
async fn edits_large_source_without_replacing_unrelated_content() {
    let root = tempfile::tempdir().unwrap();
    let before = format!("{}\nunique anchor\n", "x".repeat(128 * 1_024));
    fs::write(root.path().join("large"), &before).unwrap();
    let (task, mut rx) = start(
        root.path(),
        json!({
            "path": "large",
            "old_text": "unique anchor",
            "new_text": "updated anchor"
        }),
        CancellationToken::new(),
    );

    rx.recv()
        .await
        .unwrap()
        .decision
        .send(true)
        .unwrap();
    task.await.unwrap().unwrap();

    assert_eq!(
        fs::read_to_string(root.path().join("large")).unwrap(),
        before.replace("unique anchor", "updated anchor")
    );
}
