use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::Path,
    process::Command,
};

use orbit_runtime::filesystem::Workspace;
use orbit_tools::git_inspect::{
    review_index_patch, review_patch, review_status, update_review_index,
};
use tokio_util::sync::CancellationToken;

fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn repository() -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    git(directory.path(), &["init", "-q"]);
    git(directory.path(), &["config", "user.name", "Orbit test"]);
    git(
        directory.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    fs::write(directory.path().join("tracked.txt"), "base\n").unwrap();
    fs::write(directory.path().join("other.txt"), "other base\n").unwrap();
    git(directory.path(), &["add", "."]);
    git(directory.path(), &["commit", "-qm", "base"]);
    directory
}

fn receipt(workspace: &Workspace, path: &str, staged: bool, untracked: bool) -> String {
    let reviewed = review_index_patch(workspace, path, staged, untracked).unwrap();
    reviewed
        .index_receipt
        .unwrap_or_else(|| panic!("{}", reviewed.index_note.unwrap_or_default()))
}

fn update(workspace: &Workspace, path: &str, receipt: &str, staged: bool) {
    update_review_index(workspace, path, receipt, staged, CancellationToken::new()).unwrap();
}

fn no_locks(root: &Path) {
    assert!(!root.join(".git/index.lock").exists());
    assert!(!root.join(".git/HEAD.lock").exists());
    let head = fs::read_to_string(root.join(".git/HEAD")).unwrap();
    if let Some(name) = head.trim().strip_prefix("ref: ") {
        assert!(
            !root
                .join(".git")
                .join(format!("{name}.lock"))
                .exists()
        );
    }
}

fn configure_promisor_uploadpack(root: &Path) -> std::path::PathBuf {
    let helper = root.join("promisor-upload-pack");
    fs::write(
        &helper,
        "#!/bin/sh\nprintf 'ran\\n' > \"${0}.marker\"\nexit 1\n",
    )
    .unwrap();
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o755)).unwrap();
    git(root, &["config", "remote.origin.promisor", "true"]);
    git(
        root,
        &["config", "remote.origin.url", root.to_str().unwrap()],
    );
    git(
        root,
        &[
            "config",
            "remote.origin.uploadpack",
            helper.to_str().unwrap(),
        ],
    );
    root.join("promisor-upload-pack.marker")
}

#[test]
fn one_file_stage_and_unstage_preserve_worktree_and_other_staged_entries() {
    let directory = repository();
    let root = directory.path();
    fs::write(root.join("other.txt"), "other staged\n").unwrap();
    git(root, &["add", "other.txt"]);
    let other = git(root, &["ls-files", "--stage", "--", "other.txt"]);
    fs::write(root.join("tracked.txt"), "working\n").unwrap();
    fs::set_permissions(root.join("tracked.txt"), fs::Permissions::from_mode(0o755)).unwrap();
    let workspace = Workspace::new(root).unwrap();
    let reviewed = receipt(&workspace, "tracked.txt", false, false);
    update(&workspace, "tracked.txt", &reviewed, true);
    assert_eq!(git(root, &["show", ":tracked.txt"]), b"working\n");
    assert!(
        String::from_utf8(git(root, &["ls-files", "--stage", "tracked.txt"]))
            .unwrap()
            .starts_with("100755 ")
    );
    assert_eq!(
        git(root, &["ls-files", "--stage", "--", "other.txt"]),
        other
    );
    assert_eq!(fs::read(root.join("tracked.txt")).unwrap(), b"working\n");
    let reviewed = receipt(&workspace, "tracked.txt", true, false);
    update(&workspace, "tracked.txt", &reviewed, false);
    assert_eq!(git(root, &["show", ":tracked.txt"]), b"base\n");
    assert_eq!(
        git(root, &["ls-files", "--stage", "--", "other.txt"]),
        other
    );
    assert_eq!(fs::read(root.join("tracked.txt")).unwrap(), b"working\n");
    assert_eq!(
        fs::metadata(root.join("tracked.txt"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o755
    );
    no_locks(root);
}

#[test]
fn new_nested_file_in_an_unborn_repository_and_deletion_are_index_only() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    git(root, &["init", "-q"]);
    fs::create_dir(root.join("nested")).unwrap();
    fs::write(root.join("nested/new.txt"), "new\n").unwrap();
    let workspace = Workspace::new(root).unwrap();
    let reviewed = receipt(&workspace, "nested/new.txt", false, true);
    update(&workspace, "nested/new.txt", &reviewed, true);
    assert_eq!(git(root, &["show", ":nested/new.txt"]), b"new\n");
    let reviewed = receipt(&workspace, "nested/new.txt", true, false);
    update(&workspace, "nested/new.txt", &reviewed, false);
    assert!(git(root, &["ls-files"]).is_empty());
    assert_eq!(fs::read(root.join("nested/new.txt")).unwrap(), b"new\n");
    no_locks(root);

    let directory = repository();
    let root = directory.path();
    let workspace = Workspace::new(root).unwrap();
    fs::remove_file(root.join("tracked.txt")).unwrap();
    let reviewed = receipt(&workspace, "tracked.txt", false, false);
    update(&workspace, "tracked.txt", &reviewed, true);
    assert!(git(root, &["ls-files", "tracked.txt"]).is_empty());
    let reviewed = receipt(&workspace, "tracked.txt", true, false);
    update(&workspace, "tracked.txt", &reviewed, false);
    assert_eq!(git(root, &["show", ":tracked.txt"]), b"base\n");
    assert!(!root.join("tracked.txt").exists());
    no_locks(root);
}

#[test]
fn content_index_mode_and_head_changes_all_invalidate_the_review() {
    for change in ["content", "index", "mode", "head"] {
        let directory = repository();
        let root = directory.path();
        fs::write(root.join("tracked.txt"), "reviewed\n").unwrap();
        let workspace = Workspace::new(root).unwrap();
        let reviewed = receipt(&workspace, "tracked.txt", false, false);
        match change {
            "content" => fs::write(root.join("tracked.txt"), "later\n").unwrap(),
            "index" => {
                fs::write(root.join("other.txt"), "later staged\n").unwrap();
                git(root, &["add", "other.txt"]);
            }
            "mode" => {
                fs::set_permissions(root.join("tracked.txt"), fs::Permissions::from_mode(0o755))
                    .unwrap()
            }
            "head" => {
                git(root, &["commit", "--allow-empty", "-qm", "new head"]);
            }
            _ => unreachable!(),
        }
        let index_before = fs::read(root.join(".git/index")).unwrap();
        let bytes_before = fs::read(root.join("tracked.txt")).unwrap();
        let error = update_review_index(
            &workspace,
            "tracked.txt",
            &reviewed,
            true,
            CancellationToken::new(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("已变更"), "{change}: {error}");
        assert_eq!(fs::read(root.join(".git/index")).unwrap(), index_before);
        assert_eq!(fs::read(root.join("tracked.txt")).unwrap(), bytes_before);
        no_locks(root);
    }
}

#[test]
fn existing_git_lock_is_not_removed_and_cancelled_update_does_not_write() {
    let directory = repository();
    let root = directory.path();
    fs::write(root.join("tracked.txt"), "reviewed\n").unwrap();
    let workspace = Workspace::new(root).unwrap();
    let reviewed = receipt(&workspace, "tracked.txt", false, false);
    let index_before = fs::read(root.join(".git/index")).unwrap();
    fs::write(root.join(".git/index.lock"), "another writer").unwrap();
    assert!(
        update_review_index(
            &workspace,
            "tracked.txt",
            &reviewed,
            true,
            CancellationToken::new()
        )
        .unwrap_err()
        .to_string()
        .contains("锁已存在")
    );
    assert_eq!(
        fs::read(root.join(".git/index.lock")).unwrap(),
        b"another writer"
    );
    fs::remove_file(root.join(".git/index.lock")).unwrap();
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(matches!(
        update_review_index(&workspace, "tracked.txt", &reviewed, true, cancellation),
        Err(orbit_agent::tool::ToolError::Cancelled)
    ));
    assert_eq!(fs::read(root.join(".git/index")).unwrap(), index_before);
    no_locks(root);
}

#[test]
fn literal_filename_is_the_only_entry_changed_and_filters_and_hooks_never_run() {
    let directory = repository();
    let root = directory.path();
    let path = "literal[1],name.txt";
    fs::write(root.join(path), "raw\r\nbytes\n").unwrap();
    fs::write(root.join("literal1,name.txt"), "unrelated\n").unwrap();
    fs::write(root.join(".gitattributes"), "* filter=probe text eol=lf\n").unwrap();
    let marker = root.join("filter-ran");
    let command = format!("touch '{}'; cat", marker.display());
    git(root, &["config", "filter.probe.clean", &command]);
    git(root, &["config", "filter.probe.process", &command]);
    git(root, &["config", "filter.probe.required", "true"]);
    fs::write(
        root.join(".git/hooks/post-index-change"),
        "#!/bin/sh\ntouch hook-ran\n",
    )
    .unwrap();
    fs::set_permissions(
        root.join(".git/hooks/post-index-change"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let workspace = Workspace::new(root).unwrap();
    let reviewed = receipt(&workspace, path, false, true);
    update(&workspace, path, &reviewed, true);
    assert_eq!(git(root, &["show", &format!(":{path}")]), b"raw\r\nbytes\n");
    assert!(git(root, &["ls-files", "--", "literal1,name.txt"]).is_empty());
    let reviewed = receipt(&workspace, path, true, false);
    update(&workspace, path, &reviewed, false);
    assert!(!marker.exists());
    assert!(!root.join("hook-ran").exists());
    assert_eq!(fs::read(root.join(path)).unwrap(), b"raw\r\nbytes\n");
    no_locks(root);
}

#[test]
fn unsupported_index_and_symlink_admin_preserve_readonly_review() {
    let directory = repository();
    let root = directory.path();
    fs::write(root.join("tracked.txt"), "working\n").unwrap();
    let workspace = Workspace::new(root).unwrap();
    git(root, &["update-index", "--index-version=4"]);
    let reviewed = review_index_patch(&workspace, "tracked.txt", false, false).unwrap();
    assert!(reviewed.patch.contains("+working"));
    assert!(reviewed.index_receipt.is_none());
    assert!(
        reviewed
            .index_note
            .unwrap()
            .contains("v2/v3")
    );
    git(root, &["update-index", "--index-version=2"]);
    git(root, &["update-index", "--split-index"]);
    let reviewed = review_index_patch(&workspace, "tracked.txt", false, false).unwrap();
    assert!(reviewed.index_receipt.is_none());
    assert!(
        reviewed
            .index_note
            .unwrap()
            .contains("split/sparse")
    );

    let directory = repository();
    let root = directory.path();
    fs::write(root.join("tracked.txt"), "working\n").unwrap();
    let workspace = Workspace::new(root).unwrap();
    fs::rename(root.join(".git/index"), root.join(".git/real-index")).unwrap();
    symlink("real-index", root.join(".git/index")).unwrap();
    let reviewed = review_index_patch(&workspace, "tracked.txt", false, false).unwrap();
    assert!(reviewed.patch.contains("+working"));
    assert!(reviewed.index_receipt.is_none());
}

#[test]
fn protected_alias_and_symlink_worktree_cannot_receive_mutation_receipts() {
    let directory = repository();
    let root = directory.path();
    let secret = root.join("private.txt");
    fs::write(&secret, "private\n").unwrap();
    fs::hard_link(&secret, root.join("alias.txt")).unwrap();
    let mut workspace = Workspace::new(root).unwrap();
    workspace.protect(secret).unwrap();
    assert!(review_index_patch(&workspace, "alias.txt", false, true).is_err());
    symlink("tracked.txt", root.join("link.txt")).unwrap();
    let reviewed = review_index_patch(&workspace, "link.txt", false, true).unwrap();
    assert!(reviewed.patch.contains("120000"));
    assert!(reviewed.index_receipt.is_none());
    assert!(
        reviewed
            .index_note
            .unwrap()
            .contains("符号链接")
    );
    no_locks(root);
}

#[test]
fn native_review_and_stage_preserve_raw_crlf_despite_git_text_conversion() {
    for conversion in ["attributes", "autocrlf"] {
        let directory = repository();
        let root = directory.path();
        let path = "nested/quoted[1],\"name\".txt";
        fs::create_dir(root.join("nested")).unwrap();
        fs::write(root.join(path), b"base\n").unwrap();
        git(root, &["add", "--", path]);
        git(root, &["commit", "-qm", "nested path baseline"]);
        match conversion {
            "attributes" => fs::write(root.join(".gitattributes"), "* text eol=lf\n").unwrap(),
            "autocrlf" => {
                git(root, &["config", "core.autocrlf", "true"]);
            }
            _ => unreachable!(),
        }
        fs::write(root.join(path), b"line\r\n").unwrap();
        let workspace = Workspace::new(root).unwrap();
        let reviewed = review_index_patch(&workspace, path, false, false).unwrap();
        assert!(
            reviewed.patch.contains("+line\r\n"),
            "{conversion}: {:?}",
            reviewed.patch
        );
        let receipt = reviewed.index_receipt.unwrap();
        update(&workspace, path, &receipt, true);
        assert_eq!(git(root, &["show", &format!(":{path}")]), b"line\r\n");
        assert_eq!(fs::read(root.join(path)).unwrap(), b"line\r\n");
        let unstaged = review_index_patch(&workspace, path, false, false).unwrap();
        assert!(
            unstaged.patch.is_empty(),
            "{conversion}: {:?}",
            unstaged.patch
        );
        let staged = review_index_patch(&workspace, path, true, false).unwrap();
        assert!(staged.patch.contains("+line\r\n"), "{conversion}");
        no_locks(root);
    }
}

#[test]
fn native_review_shows_and_stages_actual_mode_when_git_ignores_filemode() {
    let directory = repository();
    let root = directory.path();
    git(root, &["config", "core.filemode", "false"]);
    fs::set_permissions(root.join("tracked.txt"), fs::Permissions::from_mode(0o755)).unwrap();
    let workspace = Workspace::new(root).unwrap();
    let reviewed = review_index_patch(&workspace, "tracked.txt", false, false).unwrap();
    assert!(
        reviewed
            .patch
            .contains("old mode 100644")
    );
    assert!(
        reviewed
            .patch
            .contains("new mode 100755")
    );
    let receipt = reviewed.index_receipt.unwrap();
    update(&workspace, "tracked.txt", &receipt, true);
    assert!(
        String::from_utf8(git(root, &["ls-files", "--stage", "--", "tracked.txt"]))
            .unwrap()
            .starts_with("100755 ")
    );
    assert_eq!(git(root, &["show", ":tracked.txt"]), b"base\n");
    assert!(
        review_index_patch(&workspace, "tracked.txt", false, false)
            .unwrap()
            .patch
            .is_empty()
    );
    no_locks(root);
}

#[test]
fn missing_promisor_objects_never_launch_uploadpack_during_review_or_status() {
    let directory = repository();
    let root = directory.path();
    let tree = String::from_utf8(git(root, &["rev-parse", "HEAD^{tree}"])).unwrap();
    let tree = tree.trim();
    let marker = configure_promisor_uploadpack(root);
    fs::remove_file(
        root.join(".git/objects")
            .join(&tree[..2])
            .join(&tree[2..]),
    )
    .unwrap();
    let workspace = Workspace::new(root).unwrap();
    let index_before = fs::read(root.join(".git/index")).unwrap();
    assert!(review_patch(&workspace, "tracked.txt", true, false).is_err());
    assert!(!marker.exists());
    assert!(review_status(&workspace, 0).is_err());
    assert!(!marker.exists());
    assert!(review_index_patch(&workspace, "tracked.txt", true, false).is_err());
    assert!(!marker.exists());
    assert_eq!(fs::read(root.join(".git/index")).unwrap(), index_before);
    assert_eq!(fs::read(root.join("tracked.txt")).unwrap(), b"base\n");
    no_locks(root);
}

#[test]
fn enabling_promisor_after_review_refuses_stage_without_changing_index() {
    let directory = repository();
    let root = directory.path();
    fs::write(root.join("tracked.txt"), b"reviewed\n").unwrap();
    let workspace = Workspace::new(root).unwrap();
    let reviewed = receipt(&workspace, "tracked.txt", false, false);
    let index_before = fs::read(root.join(".git/index")).unwrap();
    let marker = configure_promisor_uploadpack(root);
    assert!(
        update_review_index(
            &workspace,
            "tracked.txt",
            &reviewed,
            true,
            CancellationToken::new(),
        )
        .is_err()
    );
    assert_eq!(fs::read(root.join(".git/index")).unwrap(), index_before);
    assert_eq!(fs::read(root.join("tracked.txt")).unwrap(), b"reviewed\n");
    assert!(!marker.exists());
    no_locks(root);
}
