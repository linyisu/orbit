use std::{fs, path::Path, process::Command};

use orbit_agent::tool::{Tool, ToolError};
use orbit_model::types::ToolCall;
use orbit_tools::{
    git_inspect::{GitInspect, capture_baseline, working_tree_patch},
    workspace::Workspace,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

fn git(directory: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

async fn run(tool: &GitInspect, arguments: Value) -> Value {
    let output = tool
        .execute(
            ToolCall::function("git", "git_inspect", arguments),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    serde_json::from_str(&output.content).unwrap()
}

fn repository() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    git(directory.path(), &["init", "-q"]);
    git(directory.path(), &["config", "user.name", "Orbit test"]);
    git(
        directory.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    fs::write(directory.path().join("tracked.txt"), "first\n").unwrap();
    git(directory.path(), &["add", "tracked.txt"]);
    git(directory.path(), &["commit", "-qm", "initial commit"]);
    directory
}

#[test]
fn exports_complete_patch_for_tracked_and_untracked_files() {
    let directory = repository();
    fs::write(directory.path().join("tracked.txt"), "second\n").unwrap();
    fs::write(directory.path().join("new.txt"), "new\n").unwrap();

    let patch = working_tree_patch(directory.path()).unwrap();
    assert!(patch.contains("diff --git a/tracked.txt b/tracked.txt"));
    assert!(patch.contains("-first"));
    assert!(patch.contains("+second"));
    assert!(patch.contains("diff --git a/new.txt b/new.txt"));
    assert!(patch.contains("+new"));
}

#[tokio::test]
async fn reports_branch_status_diff_and_history_without_writing() {
    let directory = repository();
    fs::write(directory.path().join("tracked.txt"), "second\n").unwrap();
    fs::write(directory.path().join("new.txt"), "new\n").unwrap();
    let baseline = capture_baseline(directory.path()).expect("Git baseline");
    assert_eq!(baseline["entries"].as_array().unwrap().len(), 2);
    assert!(capture_baseline(Path::new("/tmp")).is_none());
    let tool = GitInspect::new(Workspace::new(directory.path()).unwrap());

    let status = run(&tool, json!({"view":"status","limit":1})).await;
    assert!(status["branch"]["oid"].as_str().unwrap().len() >= 7);
    assert_eq!(status["entries"].as_array().unwrap().len(), 1);
    assert_eq!(status["has_more"], true);
    let next = run(&tool, json!({"view":"status","offset":1})).await;
    assert_eq!(next["entries"].as_array().unwrap().len(), 1);

    let diff = run(&tool, json!({"view":"diff","path":"tracked.txt"})).await;
    assert!(
        diff["lines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line == "-first")
    );
    assert!(
        diff["lines"]
            .as_array()
            .unwrap()
            .iter()
            .any(|line| line == "+second")
    );

    let history = run(&tool, json!({"view":"history"})).await;
    assert_eq!(history["commits"][0]["subject"], "initial commit");
    assert_eq!(
        fs::read_to_string(directory.path().join("tracked.txt")).unwrap(),
        "second\n"
    );
}

#[tokio::test]
async fn reports_both_paths_for_a_staged_rename() {
    let directory = repository();
    git(directory.path(), &["mv", "tracked.txt", "renamed.txt"]);
    let tool = GitInspect::new(Workspace::new(directory.path()).unwrap());

    let status = run(&tool, json!({"view":"status"})).await;
    let entry = &status["entries"][0];
    assert_eq!(entry["kind"], "renamed_or_copied");
    assert_eq!(entry["path"], "renamed.txt");
    assert_eq!(entry["original_path"], "tracked.txt");
    assert_eq!(entry["score"], "R100");
}

#[tokio::test]
async fn rejects_paths_outside_workspace_and_non_root_workspaces() {
    let directory = repository();
    fs::create_dir(directory.path().join("nested")).unwrap();
    let root_tool = GitInspect::new(Workspace::new(directory.path()).unwrap());
    assert!(matches!(
        root_tool
            .execute(
                ToolCall::function("git", "git_inspect", json!({"path":".."})),
                CancellationToken::new(),
            )
            .await,
        Err(ToolError::ExecutionFailed(_))
    ));
    let nested_tool = GitInspect::new(Workspace::new(directory.path().join("nested")).unwrap());
    assert!(matches!(
        nested_tool
            .execute(
                ToolCall::function("git", "git_inspect", json!({})),
                CancellationToken::new(),
            )
            .await,
        Err(ToolError::ExecutionFailed(_))
    ));
}
