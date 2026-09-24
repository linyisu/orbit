use std::{fs, path::Path, process::Command};

use orbit_agent::tool::{Tool, ToolError};
use orbit_model::types::ToolCall;
use orbit_tools::{GitInspect, Workspace};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

fn git(root: &Path, args: &[&str]) {
    let output = Command::new("/usr/bin/git")
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?} failed; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn repository() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    git(root.path(), &["init", "-q"]);
    git(root.path(), &["config", "user.name", "Orbit test"]);
    git(
        root.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    fs::write(root.path().join("tracked.txt"), "before\n").unwrap();
    fs::write(root.path().join("secret.txt"), "private before\n").unwrap();
    git(root.path(), &["add", "tracked.txt", "secret.txt"]);
    git(root.path(), &["commit", "-qm", "initial commit"]);
    root
}

async fn inspect(tool: &GitInspect, arguments: Value) -> Value {
    let result = tool
        .execute(
            ToolCall::function("git-call", "git_inspect", arguments),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    serde_json::from_str(&result).unwrap()
}

#[tokio::test]
async fn reports_status_diff_staged_changes_and_history_without_git_locks() {
    let root = repository();
    fs::write(root.path().join("tracked.txt"), "after\n").unwrap();
    fs::write(root.path().join("new.txt"), "untracked\n").unwrap();
    let tool = GitInspect::new(Workspace::new(root.path()).unwrap());

    let status = inspect(&tool, json!({ "limit": 1 })).await;
    assert_eq!(status["total_entries"], 2);
    assert_eq!(status["entries"].as_array().unwrap().len(), 1);
    assert_eq!(status["has_more"], true);
    let next = inspect(&tool, json!({ "view": "status", "offset": 1 })).await;
    assert_eq!(next["entries"].as_array().unwrap().len(), 1);

    let diff = inspect(&tool, json!({ "view": "diff", "path": "tracked.txt" })).await;
    assert!(diff["lines"].to_string().contains("-before"), "{diff}");
    assert!(diff["lines"].to_string().contains("+after"), "{diff}");
    let first = inspect(&tool, json!({ "view": "diff", "limit": 2 })).await;
    assert_eq!(first["has_more"], true);
    assert_eq!(first["next_offset"], 2);
    let second = inspect(&tool, json!({ "view": "diff", "offset": 2, "limit": 200 })).await;
    assert!(second["lines"].to_string().contains("+after"), "{second}");

    git(root.path(), &["add", "tracked.txt"]);
    let staged = inspect(&tool, json!({ "view": "diff", "staged": true })).await;
    assert!(staged["lines"].to_string().contains("+after"), "{staged}");
    let history = inspect(&tool, json!({ "view": "history" })).await;
    assert_eq!(history["commits"][0]["subject"], "initial commit");
    assert!(!root.path().join(".git/index.lock").exists());
}

#[tokio::test]
async fn excludes_protected_files_from_unfiltered_status_and_diff() {
    let root = repository();
    fs::write(root.path().join("tracked.txt"), "after\n").unwrap();
    fs::write(root.path().join("secret.txt"), "private after\n").unwrap();
    fs::hard_link(
        root.path().join("secret.txt"),
        root.path().join("alias.txt"),
    )
    .unwrap();
    let mut workspace = Workspace::new(root.path()).unwrap();
    workspace.protect(root.path().join("secret.txt")).unwrap();
    let tool = GitInspect::new(workspace);

    let status = inspect(&tool, json!({ "view": "status" })).await;
    assert_eq!(status["total_entries"], 1, "{status}");
    assert_eq!(status["entries"][0]["path"], "tracked.txt");
    let diff = inspect(&tool, json!({ "view": "diff" })).await;
    assert!(diff["lines"].to_string().contains("+after"), "{diff}");
    assert!(!diff.to_string().contains("private after"), "{diff}");

    let error = tool
        .execute(
            ToolCall::function(
                "protected",
                "git_inspect",
                json!({ "view": "diff", "path": "secret.txt" }),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("blocked"), "{error}");
    let error = tool
        .execute(
            ToolCall::function(
                "alias",
                "git_inspect",
                json!({ "view": "status", "path": "alias.txt" }),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("blocked"), "{error}");
}

#[tokio::test]
async fn rejects_escape_paths_and_non_root_workspaces() {
    let root = repository();
    fs::create_dir(root.path().join("nested")).unwrap();
    let tool = GitInspect::new(Workspace::new(root.path()).unwrap());
    let error = tool
        .execute(
            ToolCall::function("escape", "git_inspect", json!({ "path": "../outside" })),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(matches!(error, ToolError::ExecutionFailed(_)), "{error}");

    let nested = GitInspect::new(Workspace::new(root.path().join("nested")).unwrap());
    let error = nested
        .execute(
            ToolCall::function("nested", "git_inspect", json!({})),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("root differs"), "{error}");
}

#[tokio::test]
async fn status_keeps_rename_paths_and_hides_a_protected_original_path() {
    let root = repository();
    git(root.path(), &["mv", "tracked.txt", "renamed.txt"]);
    let tool = GitInspect::new(Workspace::new(root.path()).unwrap());

    let status = inspect(&tool, json!({ "view": "status" })).await;
    assert_eq!(status["entries"][0]["kind"], "renamed_or_copied");
    assert_eq!(status["entries"][0]["path"], "renamed.txt");
    assert_eq!(status["entries"][0]["original_path"], "tracked.txt");
    assert_eq!(status["entries"][0]["score"], "R100");

    let mut protected = Workspace::new(root.path()).unwrap();
    protected.protect(root.path().join("tracked.txt")).unwrap();
    let protected_tool = GitInspect::new(protected);
    let hidden = inspect(&protected_tool, json!({ "view": "status" })).await;
    assert_eq!(hidden["total_entries"], 0, "{hidden}");
}
