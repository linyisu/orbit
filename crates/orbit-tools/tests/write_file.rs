use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    time::Duration,
};

use orbit_agent::tool::{FileChange, Tool, ToolError, ToolOutput};
use orbit_model::types::ToolCall;
use orbit_runtime::filesystem::Workspace;
use orbit_tools::{
    approval::{self, ApprovalReceiver},
    write_file::WriteFile,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

struct Fixture(tempfile::TempDir);

impl Fixture {
    fn new() -> Self {
        Self(tempfile::tempdir().unwrap())
    }

    fn tool(&self) -> (WriteFile, ApprovalReceiver) {
        let (sender, receiver) = approval::channel();
        (
            WriteFile::new(Workspace::new(self.0.path()).unwrap(), sender),
            receiver,
        )
    }
}

fn call(path: &str, content: &str) -> ToolCall {
    ToolCall::function(
        "write-1",
        "WriteFile",
        json!({
            "path": path,
            "content": content
        }),
    )
}

fn start(
    tool: WriteFile,
    path: &str,
    content: &str,
    token: CancellationToken,
) -> tokio::task::JoinHandle<Result<ToolOutput, ToolError>> {
    let call = call(path, content);
    tokio::spawn(async move { tool.execute(call, token).await })
}

async fn request(receiver: &mut ApprovalReceiver) -> approval::ApprovalRequest {
    tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .unwrap()
        .unwrap()
}

fn no_temp(root: &Fixture) {
    assert!(
        fs::read_dir(root.0.path())
            .unwrap()
            .all(|e| {
                !e.unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".orbit-write-")
            })
    );
}

#[tokio::test]
async fn requires_approval_then_creates_and_replaces_exact_utf8_with_mode_preserved() {
    let root = Fixture::new();

    for before in [None, Some("你好\n")] {
        let (tool, mut approvals) = root.tool();
        let after = if before.is_none() {
            "你好\n"
        } else {
            "第二版"
        };

        if before.is_some() {
            fs::set_permissions(
                root.0.path().join("file"),
                fs::Permissions::from_mode(0o750),
            )
            .unwrap();
        }

        let task = start(tool, "file", after, CancellationToken::new());
        let approval = request(&mut approvals).await;
        let approval::ApprovalPreview::Write(preview) = &approval.preview else {
            panic!("expected file preview")
        };

        assert_eq!(preview.before.as_deref(), before);
        assert_eq!(preview.after, after);
        assert_eq!(
            fs::read_to_string(root.0.path().join("file"))
                .ok()
                .as_deref(),
            before
        );

        approval.decision.send(true).unwrap();
        let output = task.await.unwrap().unwrap();

        assert_eq!(output.changes.len(), 1);
        assert_eq!(output.changes[0].path, "file");
        assert_eq!(output.changes[0].before.as_deref(), before);
        assert_eq!(output.changes[0].after, after);
        assert_eq!(output.files.written, ["file".into()].into());
        assert!(output.files.read.is_empty());

        let result: serde_json::Value = serde_json::from_str(&output).unwrap();

        assert_eq!(result["written_bytes"], after.len());
        assert_eq!(
            fs::read_to_string(root.0.path().join("file")).unwrap(),
            after
        );
        assert_eq!(
            fs::metadata(root.0.path().join("file"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            if before.is_none() { 0o600 } else { 0o750 }
        );

        no_temp(&root);
    }
}

#[tokio::test]
async fn deny_cancel_and_closed_ui_never_write() {
    let root = Fixture::new();

    for decision in 0..3 {
        let (tool, mut approvals) = root.tool();
        let token = CancellationToken::new();
        let task = start(tool, "new", "text", token.clone());
        let approval = request(&mut approvals).await;

        match decision {
            0 => {
                approval.decision.send(false).unwrap();
            }
            1 => {
                token.cancel();
                drop(approval);
            }

            _ => drop(approval),
        }

        assert!(task.await.unwrap().is_err());
        assert!(!root.0.path().join("new").exists());

        no_temp(&root);
    }

    let (tool, receiver) = root.tool();
    drop(receiver);

    assert!(
        tool.execute(call("new", "text"), CancellationToken::new())
            .await
            .is_err()
    );
    assert!(!root.0.path().join("new").exists());
}

#[tokio::test]
async fn restore_requires_a_matching_checkpoint_and_can_remove_created_files() {
    let root = Fixture::new();
    let (tool, mut approvals) = root.tool();
    fs::write(root.0.path().join("existing"), "Orbit version").unwrap();
    let task = tokio::spawn({
        let tool = tool.clone();

        async move {
            tool.restore(
                FileChange {
                    path: "existing".into(),
                    before: Some("original".into()),
                    after: "Orbit version".into(),
                },
                CancellationToken::new(),
            )
            .await
        }
    });
    let approval = request(&mut approvals).await;
    let approval::ApprovalPreview::Write(preview) = &approval.preview else {
        panic!("expected restore write preview")
    };

    assert_eq!(preview.before.as_deref(), Some("Orbit version"));
    assert_eq!(preview.after, "original");

    approval.decision.send(true).unwrap();

    assert!(
        task.await
            .unwrap()
            .unwrap()
            .contains("\"restored\":true")
    );
    assert_eq!(
        fs::read_to_string(root.0.path().join("existing")).unwrap(),
        "original"
    );

    fs::write(root.0.path().join("created"), "created by Orbit").unwrap();
    let task = tokio::spawn({
        let tool = tool.clone();

        async move {
            tool.restore(
                FileChange {
                    path: "created".into(),
                    before: None,
                    after: "created by Orbit".into(),
                },
                CancellationToken::new(),
            )
            .await
        }
    });
    let approval = request(&mut approvals).await;

    assert!(matches!(
        &approval.preview,
        approval::ApprovalPreview::Delete(_)
    ));

    approval.decision.send(true).unwrap();

    assert!(
        task.await
            .unwrap()
            .unwrap()
            .contains("\"created_file_removed\":true")
    );
    assert!(!root.0.path().join("created").exists());

    fs::write(root.0.path().join("external"), "human version").unwrap();

    assert!(
        tool.restore(
            FileChange {
                path: "external".into(),
                before: Some("original".into()),
                after: "Orbit version".into(),
            },
            CancellationToken::new(),
        )
        .await
        .is_err()
    );
    assert!(approvals.try_recv().is_err());
}

#[tokio::test]
async fn restore_refuses_a_file_changed_after_its_preview() {
    let root = Fixture::new();
    let (tool, mut approvals) = root.tool();
    fs::write(root.0.path().join("created"), "created by Orbit").unwrap();
    let task = tokio::spawn(async move {
        tool.restore(
            FileChange {
                path: "created".into(),
                before: None,
                after: "created by Orbit".into(),
            },
            CancellationToken::new(),
        )
        .await
    });
    let approval = request(&mut approvals).await;
    fs::write(root.0.path().join("created"), "human changed it").unwrap();
    approval.decision.send(true).unwrap();

    assert!(task.await.unwrap().is_err());
    assert_eq!(
        fs::read_to_string(root.0.path().join("created")).unwrap(),
        "human changed it"
    );
}

#[tokio::test]
async fn refuses_changes_and_file_replacements_during_approval() {
    let root = Fixture::new();

    for kind in 0..4 {
        let path = root.0.path().join("file");

        if path.exists() {
            fs::remove_file(&path).unwrap();
        }

        if kind != 2 {
            fs::write(&path, "before").unwrap();
        }

        let (tool, mut approvals) = root.tool();
        let task = start(tool, "file", "after", CancellationToken::new());
        let approval = request(&mut approvals).await;

        match kind {
            0 | 2 => fs::write(&path, "external edit").unwrap(),
            1 => {
                let replacement = root.0.path().join("replacement");
                fs::write(&replacement, "before").unwrap();
                fs::rename(replacement, &path).unwrap();
            }

            _ => {
                fs::remove_file(&path).unwrap();
                symlink("elsewhere", &path).unwrap();
            }
        }

        approval.decision.send(true).unwrap();

        assert!(task.await.unwrap().is_err());
        assert_ne!(
            fs::read_to_string(&path)
                .ok()
                .as_deref(),
            Some("after")
        );

        no_temp(&root);
    }
}

#[tokio::test]
async fn rejects_unsafe_paths_links_binary_large_files_and_missing_parents_before_asking() {
    let root = Fixture::new();
    fs::write(root.0.path().join("binary"), [0, 1, 2]).unwrap();
    fs::write(root.0.path().join("large"), vec![b'x'; 1_048_577]).unwrap();
    fs::write(root.0.path().join("original"), "text").unwrap();
    fs::hard_link(root.0.path().join("original"), root.0.path().join("hard")).unwrap();
    symlink("original", root.0.path().join("symlink")).unwrap();
    symlink("/tmp", root.0.path().join("linked-dir")).unwrap();
    fs::create_dir(root.0.path().join("directory")).unwrap();

    for path in [
        "../escape",
        "/tmp/escape",
        "binary",
        "large",
        "hard",
        "symlink",
        "linked-dir/file",
        "directory",
        "missing/file",
    ] {
        let (tool, mut approvals) = root.tool();

        assert!(
            tokio::time::timeout(
                Duration::from_secs(2),
                tool.execute(call(path, "text"), CancellationToken::new()),
            )
            .await
            .expect("unsafe path unexpectedly waited for approval")
            .is_err(),
            "{path}"
        );
        assert!(approvals.try_recv().is_err());
    }

    let (tool, _) = root.tool();

    assert!(matches!(
        tool.execute(
            call("file", &"x".repeat(1_048_577)),
            CancellationToken::new()
        )
        .await,
        Err(ToolError::ExecutionFailed(message)) if message.contains("UTF-8 text without NUL")
    ));
    assert!(matches!(
        tool.execute(call("file", "a\0b"), CancellationToken::new())
            .await,
        Err(ToolError::ExecutionFailed(message)) if message.contains("UTF-8 text without NUL")
    ));

    no_temp(&root);
}

#[tokio::test]
async fn ordinary_config_filename_requires_approval_instead_of_a_blanket_ban() {
    let root = Fixture::new();
    let (tool, mut approvals) = root.tool();
    let task = start(
        tool,
        "config.toml",
        "setting = true",
        CancellationToken::new(),
    );

    request(&mut approvals)
        .await
        .decision
        .send(false)
        .unwrap();

    assert!(task.await.unwrap().is_err());
    assert!(
        !root
            .0
            .path()
            .join("config.toml")
            .exists()
    );
}

#[tokio::test]
async fn parent_replacement_after_approval_is_rejected() {
    let root = Fixture::new();
    fs::create_dir(root.0.path().join("dir")).unwrap();
    let (tool, mut approvals) = root.tool();
    let task = start(tool, "dir/file", "x", CancellationToken::new());
    let approval = request(&mut approvals).await;
    fs::rename(root.0.path().join("dir"), root.0.path().join("moved")).unwrap();
    fs::create_dir(root.0.path().join("dir")).unwrap();
    approval.decision.send(true).unwrap();

    assert!(task.await.unwrap().is_err());
    assert!(!root.0.path().join("dir/file").exists());
    assert!(
        !root
            .0
            .path()
            .join("moved/file")
            .exists()
    );
}
