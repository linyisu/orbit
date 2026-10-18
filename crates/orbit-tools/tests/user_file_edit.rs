use orbit_runtime::filesystem::Workspace;
use orbit_tools::{approval, write_file::WriteFile};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};
use tokio_util::sync::CancellationToken;

fn writer(path: &std::path::Path) -> WriteFile {
    let (sender, _receiver) = approval::channel();
    WriteFile::new(Workspace::new(path).unwrap(), sender)
}

#[tokio::test]
async fn user_save_preserves_raw_markdown_crlf_and_file_permissions() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("notes.md");
    let before = "```rust\r\nlet old = 1;\r\n```\r\n";
    let after = "```rust\r\nlet 新 = 2;\r\n```\r\n";
    fs::write(&path, before).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
    let output = writer(root.path())
        .save_user_edit(
            "notes.md".into(),
            before.into(),
            after.into(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), after);
    assert_eq!(
        fs::metadata(path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o640
    );
    assert_eq!(output.changes[0].before.as_deref(), Some(before));
    assert_eq!(output.changes[0].after, after);
}

#[tokio::test]
async fn user_save_refuses_stale_missing_binary_and_oversized_files() {
    let root = tempfile::tempdir().unwrap();
    let tool = writer(root.path());
    fs::write(root.path().join("note"), "external change").unwrap();
    assert!(
        tool.save_user_edit(
            "note".into(),
            "opened text".into(),
            "edited".into(),
            CancellationToken::new()
        )
        .await
        .is_err()
    );
    assert_eq!(
        fs::read_to_string(root.path().join("note")).unwrap(),
        "external change"
    );
    assert!(
        tool.save_user_edit(
            "missing".into(),
            "".into(),
            "edited".into(),
            CancellationToken::new()
        )
        .await
        .is_err()
    );
    assert!(!root.path().join("missing").exists());
    fs::write(root.path().join("binary"), b"one\0two").unwrap();
    assert!(
        tool.save_user_edit(
            "binary".into(),
            "one\0two".into(),
            "edited".into(),
            CancellationToken::new()
        )
        .await
        .is_err()
    );
    assert!(
        tool.save_user_edit(
            "note".into(),
            "external change".into(),
            "x".repeat(1_024 * 1_024 + 1),
            CancellationToken::new()
        )
        .await
        .is_err()
    );
    assert!(
        fs::read_dir(root.path())
            .unwrap()
            .all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".orbit-write-")
            })
    );
}

#[tokio::test]
async fn user_save_preserves_atomicity_and_external_authorization() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let tool = writer(root.path());
    fs::write(root.path().join("note"), "old").unwrap();
    fs::hard_link(root.path().join("note"), root.path().join("alias")).unwrap();
    assert!(
        tool.save_user_edit(
            "alias".into(),
            "old".into(),
            "new".into(),
            CancellationToken::new()
        )
        .await
        .is_err()
    );
    fs::write(outside.path().join("note"), "outside").unwrap();
    symlink(outside.path(), root.path().join("escape")).unwrap();
    assert!(
        tool.save_user_edit(
            "escape/note".into(),
            "outside".into(),
            "new".into(),
            CancellationToken::new()
        )
        .await
        .is_err()
    );
    fs::create_dir(root.path().join(".git")).unwrap();
    fs::write(root.path().join(".git/config"), "metadata").unwrap();
    tool.save_user_edit(
        ".git/config".into(),
        "metadata".into(),
        "new".into(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(
        fs::read_to_string(root.path().join(".git/config")).unwrap(),
        "new"
    );
    assert_eq!(
        fs::read_to_string(outside.path().join("note")).unwrap(),
        "outside"
    );
}
