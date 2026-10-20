use std::{fs, path::Path, process::Command};

use orbit_agent::tool::{Tool, ToolError};
use orbit_model::types::ToolCall;
use orbit_runtime::filesystem::Workspace;
use orbit_tools::git_inspect::{
    GitInspect, capture_baseline, review_patch, review_status, working_tree_patch,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

fn git(directory: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", orbit_runtime::git::NULL_DEVICE)
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
fn history_publication_uses_remote_tracking_refs_without_contacting_a_remote() {
    use orbit_tools::git_inspect::history;
    let directory = repository();
    let root = directory.path();
    let workspace = Workspace::new(root).unwrap();
    let initial = history::list(&workspace, "HEAD", 0).unwrap();
    assert!(initial["commits"][0]["publication"].is_null());
    let published = initial["revision"].as_str().unwrap();
    git(root, &["update-ref", "refs/remotes/origin/main", published]);
    git(root, &["commit", "--allow-empty", "-qm", "local change"]);
    let index = fs::read(root.join(".git/index")).unwrap();
    let page = history::list(&workspace, "HEAD", 0).unwrap();
    assert_eq!(page["commits"][0]["publication"], "unpushed");
    assert_eq!(page["commits"][1]["publication"], "published");
    assert_eq!(
        history::detail(&workspace, "HEAD").unwrap()["commit"]["publication"],
        "unpushed"
    );
    git(
        root,
        &[
            "update-ref",
            "refs/remotes/origin/main",
            page["revision"].as_str().unwrap(),
        ],
    );
    assert_eq!(
        history::list(&workspace, "HEAD", 0).unwrap()["commits"][0]["publication"],
        "published"
    );
    assert_eq!(fs::read(root.join(".git/index")).unwrap(), index);
}

#[test]
fn history_pages_pin_identity_and_reject_nested_workspaces() {
    use orbit_tools::git_inspect::history;
    let directory = repository();
    fs::create_dir(directory.path().join("nested")).unwrap();
    fs::write(
        directory
            .path()
            .join("nested/sample.txt"),
        "historical\n",
    )
    .unwrap();
    git(directory.path(), &["add", "nested"]);
    git(directory.path(), &["commit", "-qm", "nested content"]);
    for _ in 0..51 {
        git(
            directory.path(),
            &["commit", "--allow-empty", "-qm", "generated history"],
        );
    }
    let workspace = Workspace::new(directory.path()).unwrap();
    let first = history::list(&workspace, "HEAD", 0).unwrap();
    assert_eq!(
        first["commits"]
            .as_array()
            .unwrap()
            .len(),
        50
    );
    assert_eq!(first["next"], 50);
    git(
        directory.path(),
        &["commit", "--allow-empty", "-qm", "later commit"],
    );
    let pinned = first["revision"].as_str().unwrap();
    let second = history::list(&workspace, pinned, 50).unwrap();
    assert_eq!(second["revision"], pinned);
    assert_eq!(
        second["commits"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert!(second["next"].is_null());
    assert_ne!(
        history::list(&workspace, "HEAD", 0).unwrap()["revision"],
        pinned
    );
    let nested = Workspace::new(directory.path().join("nested")).unwrap();
    assert!(history::list(&nested, "HEAD", 0).is_err());
    assert!(history::detail(&nested, "HEAD~52").is_err());
    assert!(history::file(&nested, "HEAD~52", "sample.txt").is_err());
    assert!(history::file(&nested, pinned, "../tracked.txt").is_err());
}

#[test]
fn history_reads_pinned_commits_and_deleted_files_without_touching_the_worktree() {
    use orbit_tools::git_inspect::history;
    let directory = repository();
    let root = directory.path();
    fs::write(
        root.join(".gitattributes"),
        "*.txt diff=generated filter=generated\n",
    )
    .unwrap();
    git(
        root,
        &[
            "config",
            "diff.generated.textconv",
            "orbit-nonexistent-filter",
        ],
    );
    git(
        root,
        &[
            "config",
            "filter.generated.smudge",
            "orbit-nonexistent-filter",
        ],
    );
    let workspace = Workspace::new(root).unwrap();
    let initial = history::list(&workspace, "HEAD", 0).unwrap();
    let initial_id = initial["revision"].as_str().unwrap();
    let first = history::detail(&workspace, initial_id).unwrap();
    assert_eq!(first["files"], json!(["tracked.txt"]));
    let file = history::file(&workspace, initial_id, "tracked.txt").unwrap();
    assert_eq!(file["content"], "first\n");
    assert!(
        file["patch"]
            .as_str()
            .unwrap()
            .contains("+first")
    );
    git(root, &["rm", "-q", "tracked.txt"]);
    fs::write(root.join("note with spaces.txt"), "second\n").unwrap();
    git(root, &["add", "note with spaces.txt"]);
    git(
        root,
        &["commit", "-qm", "replace file\n\nGenerated detail body"],
    );
    let index = fs::read(root.join(".git/index")).unwrap();
    let page = history::list(&workspace, "HEAD", 0).unwrap();
    let id = page["revision"].as_str().unwrap();
    let commit = history::detail(&workspace, id).unwrap();
    assert_eq!(
        commit["files"],
        json!(["note with spaces.txt", "tracked.txt"])
    );
    assert!(
        commit["message"]
            .as_str()
            .unwrap()
            .contains("Generated detail body")
    );
    let deleted = history::file(&workspace, id, "tracked.txt").unwrap();
    assert!(deleted["content"].is_null());
    assert!(
        deleted["patch"]
            .as_str()
            .unwrap()
            .contains("-first")
    );
    assert_eq!(
        history::list(&workspace, initial_id, 0).unwrap()["commits"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(history::file(&workspace, id, "../outside.txt").is_err());
    assert!(history::list(&workspace, "--output=injected", 0).is_err());
    assert!(!root.join("injected").exists());
    assert_eq!(fs::read(root.join(".git/index")).unwrap(), index);
    assert!(!root.join("tracked.txt").exists());
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

#[test]
fn desktop_review_distinguishes_index_worktree_and_untracked_content() {
    let directory = repository();
    fs::write(directory.path().join("tracked.txt"), "staged\n").unwrap();
    git(directory.path(), &["add", "tracked.txt"]);
    fs::write(directory.path().join("tracked.txt"), "working\n").unwrap();
    fs::write(directory.path().join("new.txt"), "new\n").unwrap();
    fs::create_dir(directory.path().join("new-directory")).unwrap();
    fs::write(
        directory
            .path()
            .join("new-directory/note.txt"),
        "nested\n",
    )
    .unwrap();
    let workspace = Workspace::new(directory.path()).unwrap();
    let status = review_status(&workspace, 0).unwrap();
    assert!(
        status["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == "tracked.txt" && entry["xy"] == "MM")
    );
    assert!(
        status["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["path"] == "new-directory/note.txt" && entry["kind"] == "untracked")
    );
    let staged = review_patch(&workspace, "tracked.txt", true, false).unwrap();
    assert!(staged.contains("-first") && staged.contains("+staged"));
    assert!(!staged.contains("+working"));
    let unstaged = review_patch(&workspace, "tracked.txt", false, false).unwrap();
    assert!(unstaged.contains("-staged") && unstaged.contains("+working"));
    let new = review_patch(&workspace, "new.txt", false, true).unwrap();
    assert!(new.contains("+new"));
    let nested = review_patch(&workspace, "new-directory/note.txt", false, true).unwrap();
    assert!(nested.contains("+nested"));
    assert!(review_patch(&workspace, "tracked.txt", false, true).is_err());
    assert!(review_patch(&workspace, "../outside", false, false).is_err());
}

#[test]
fn desktop_review_allows_config_and_keeps_external_boundaries() {
    use std::os::unix::fs::symlink;
    let directory = repository();
    let external = tempfile::tempdir().unwrap();
    fs::write(external.path().join("outside.txt"), "outside secret").unwrap();
    fs::write(directory.path().join("config.toml"), "original secret").unwrap();
    fs::create_dir(directory.path().join("private")).unwrap();
    fs::write(
        directory
            .path()
            .join("private/auth.json"),
        "credential",
    )
    .unwrap();
    fs::create_dir(directory.path().join("removed")).unwrap();
    fs::write(
        directory.path().join("removed/old.txt"),
        "deleted tracked text\n",
    )
    .unwrap();
    git(directory.path(), &["add", "config.toml", "removed/old.txt"]);
    git(
        directory.path(),
        &["commit", "-qm", "tracked protected file and deletions"],
    );
    fs::write(directory.path().join("config.toml"), "changed secret").unwrap();
    fs::hard_link(
        directory.path().join("config.toml"),
        directory
            .path()
            .join("config-alias.txt"),
    )
    .unwrap();
    symlink("private/auth.json", directory.path().join("auth-alias.txt")).unwrap();
    symlink(
        external.path().join("outside.txt"),
        directory
            .path()
            .join("outside-alias.txt"),
    )
    .unwrap();
    symlink(
        external.path(),
        directory
            .path()
            .join("outside-directory"),
    )
    .unwrap();
    fs::remove_file(directory.path().join("removed/old.txt")).unwrap();
    fs::remove_dir(directory.path().join("removed")).unwrap();
    let workspace = Workspace::new(directory.path()).unwrap();

    for path in ["outside-alias.txt", "outside-directory/outside.txt"] {
        assert!(
            review_patch(&workspace, path, false, false).is_err(),
            "{path}"
        );
        assert!(
            review_patch(&workspace, path, false, true).is_err(),
            "{path}"
        );
    }
    let listing = review_status(&workspace, 0).unwrap();
    let paths: Vec<_> = listing["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["path"].as_str().unwrap())
        .collect();
    assert!(paths.contains(&"removed/old.txt"));
    assert!(paths.contains(&"config.toml"));
    let deletion = review_patch(&workspace, "removed/old.txt", false, false).unwrap();
    assert!(deletion.contains("-deleted tracked text"));
    assert_eq!(
        fs::read_to_string(directory.path().join("config.toml")).unwrap(),
        "changed secret"
    );
    assert_eq!(
        fs::read_to_string(external.path().join("outside.txt")).unwrap(),
        "outside secret"
    );
}

#[test]
fn git_inspection_never_executes_repository_clean_or_process_filters() {
    let directory = repository();
    fs::write(directory.path().join("process.txt"), "process original\n").unwrap();
    fs::write(
        directory.path().join(".gitattributes"),
        "tracked.txt filter=clean_probe\nprocess.txt filter=process_probe\n",
    )
    .unwrap();
    git(directory.path(), &["add", ".gitattributes", "process.txt"]);
    git(
        directory.path(),
        &["commit", "-qm", "conversion filter attributes"],
    );
    git(
        directory.path(),
        &[
            "config",
            "filter.clean_probe.clean",
            "touch .clean-filter-executed; cat",
        ],
    );
    git(
        directory.path(),
        &["config", "filter.clean_probe.required", "true"],
    );
    git(
        directory.path(),
        &[
            "config",
            "filter.process_probe.process",
            "touch .process-filter-executed; exit 1",
        ],
    );
    git(
        directory.path(),
        &["config", "filter.process_probe.required", "true"],
    );
    fs::write(directory.path().join("tracked.txt"), "clean changed\n").unwrap();
    fs::write(directory.path().join("process.txt"), "process changed\n").unwrap();
    let workspace = Workspace::new(directory.path()).unwrap();
    let status = review_status(&workspace, 0).unwrap();
    assert_eq!(
        status["entries"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(
        review_patch(&workspace, "tracked.txt", false, false)
            .unwrap()
            .contains("+clean changed")
    );
    assert!(
        review_patch(&workspace, "process.txt", false, false)
            .unwrap()
            .contains("+process changed")
    );
    assert!(
        working_tree_patch(directory.path())
            .unwrap()
            .contains("+process changed")
    );
    assert!(
        !directory
            .path()
            .join(".clean-filter-executed")
            .exists()
    );
    assert!(
        !directory
            .path()
            .join(".process-filter-executed")
            .exists()
    );
}

#[test]
fn desktop_review_paginates_individual_nested_untracked_files_without_omissions() {
    let directory = repository();
    fs::create_dir(directory.path().join("new-directory")).unwrap();
    for index in 0..505 {
        fs::write(
            directory
                .path()
                .join(format!("new-directory/{index:04}.txt")),
            "new\n",
        )
        .unwrap();
    }
    let workspace = Workspace::new(directory.path()).unwrap();
    let first = review_status(&workspace, 0).unwrap();
    assert_eq!(
        first["entries"]
            .as_array()
            .unwrap()
            .len(),
        500
    );
    assert_eq!(first["next_offset"], 500);
    let second = review_status(&workspace, 500).unwrap();
    assert_eq!(
        second["entries"]
            .as_array()
            .unwrap()
            .len(),
        5
    );
    assert_eq!(second["next_offset"], Value::Null);
    let paths: std::collections::BTreeSet<_> = first["entries"]
        .as_array()
        .unwrap()
        .iter()
        .chain(second["entries"].as_array().unwrap())
        .map(|entry| entry["path"].as_str().unwrap())
        .collect();
    assert_eq!(paths.len(), 505);
    assert!(paths.contains("new-directory/0000.txt") && paths.contains("new-directory/0504.txt"));
}

#[tokio::test]
async fn reports_branch_status_diff_and_history_without_writing() {
    let directory = repository();
    fs::write(directory.path().join("tracked.txt"), "second\n").unwrap();
    fs::write(directory.path().join("new.txt"), "new\n").unwrap();
    let baseline = capture_baseline(directory.path()).expect("Git baseline");

    assert_eq!(
        baseline["entries"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let non_repository = tempfile::tempdir().unwrap();
    assert!(capture_baseline(non_repository.path()).is_none());

    let tool = GitInspect::new(Workspace::new(directory.path()).unwrap());

    let status = run(
        &tool,
        json!({
            "view": "status",
            "limit": 1
        }),
    )
    .await;

    assert!(
        status["branch"]["oid"]
            .as_str()
            .unwrap()
            .len()
            >= 7
    );
    assert_eq!(
        status["entries"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(status["has_more"], true);

    let next = run(
        &tool,
        json!({
            "view": "status",
            "offset": 1
        }),
    )
    .await;

    assert_eq!(
        next["entries"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    let diff = run(
        &tool,
        json!({
            "view": "diff",
            "path": "tracked.txt"
        }),
    )
    .await;

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

    let history = run(
        &tool,
        json!({
            "view": "history"
        }),
    )
    .await;

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

    let status = run(
        &tool,
        json!({
            "view": "status"
        }),
    )
    .await;
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
                ToolCall::function(
                    "git",
                    "git_inspect",
                    json!({
                        "path": ".."
                    })
                ),
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

#[tokio::test]
async fn history_keeps_commit_fields_aligned_across_records_and_pages() {
    let directory = repository();

    for subject in ["第二条提交", "", "fourth"] {
        git(
            directory.path(),
            &[
                "commit",
                "--allow-empty",
                "--allow-empty-message",
                "-qm",
                subject,
            ],
        );
    }

    let tool = GitInspect::new(Workspace::new(directory.path()).unwrap());
    let first = run(
        &tool,
        json!({
            "view": "history",
            "limit": 2
        }),
    )
    .await;

    assert_eq!(first["has_more"], true);
    assert_eq!(first["next_offset"], 2);

    let second = run(
        &tool,
        json!({
            "view": "history",
            "offset": 2,
            "limit": 2
        }),
    )
    .await;

    assert_eq!(second["has_more"], false);
    assert_eq!(second["next_offset"], Value::Null);

    let commits: Vec<_> = first["commits"]
        .as_array()
        .unwrap()
        .iter()
        .chain(second["commits"].as_array().unwrap())
        .collect();

    assert_eq!(commits.len(), 4);

    for (commit, subject) in commits
        .iter()
        .zip(["fourth", "", "第二条提交", "initial commit"])
    {
        assert_eq!(commit["subject"], subject);
        assert_eq!(commit["author"], "Orbit test");

        let id = commit["id"].as_str().unwrap();

        assert_eq!(id.len(), 40);
        assert!(
            id.bytes()
                .all(|b| b.is_ascii_hexdigit())
        );
        assert!(id.starts_with(commit["short_id"].as_str().unwrap()));
        assert!(
            commit["authored_at"]
                .as_str()
                .unwrap()
                .contains('T')
        );

        let actual = Command::new("git")
            .arg("-C")
            .arg(directory.path())
            .args(["show", "-s", "--format=%H%n%aI%n%an%n%s", id])
            .output()
            .unwrap();
        let expected = format!(
            "{}\n{}\n{}\n{}\n",
            id,
            commit["authored_at"].as_str().unwrap(),
            "Orbit test",
            subject
        );

        assert_eq!(String::from_utf8(actual.stdout).unwrap(), expected);
    }
}
