use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use orbit_agent::tool::{Tool, ToolError, ToolOutput};
use orbit_model::types::ToolCall;
use orbit_tools::{
    approval::{self, ApprovalReceiver},
    workspace::Workspace,
    write_file::WriteFile,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "orbit-write-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn tool(&self) -> (WriteFile, ApprovalReceiver) {
        let (sender, receiver) = approval::channel();
        (
            WriteFile::new(Workspace::new(&self.0).unwrap(), sender),
            receiver,
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn call(path: &str, content: &str) -> ToolCall {
    ToolCall::function(
        "write-1",
        "write_file",
        json!({"path":path,"content":content}),
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
    assert!(fs::read_dir(&root.0).unwrap().all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".orbit-write-")
    }));
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
            fs::set_permissions(root.0.join("file"), fs::Permissions::from_mode(0o750)).unwrap();
        }
        let task = start(tool, "file", after, CancellationToken::new());
        let approval = request(&mut approvals).await;
        let approval::ApprovalPreview::Write(preview) = &approval.preview else {
            panic!("expected file preview")
        };
        assert_eq!(preview.before.as_deref(), before);
        assert_eq!(preview.after, after);
        assert_eq!(
            fs::read_to_string(root.0.join("file")).ok().as_deref(),
            before
        );
        approval.decision.send(true).unwrap();
        let result: serde_json::Value =
            serde_json::from_str(&task.await.unwrap().unwrap()).unwrap();
        assert_eq!(result["written_bytes"], after.len());
        assert_eq!(fs::read_to_string(root.0.join("file")).unwrap(), after);
        assert_eq!(
            fs::metadata(root.0.join("file"))
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
        assert!(!root.0.join("new").exists());
        no_temp(&root);
    }
    let (tool, receiver) = root.tool();
    drop(receiver);
    assert!(
        tool.execute(call("new", "text"), CancellationToken::new())
            .await
            .is_err()
    );
    assert!(!root.0.join("new").exists());
}

#[tokio::test]
async fn refuses_changes_and_file_replacements_during_approval() {
    let root = Fixture::new();
    for kind in 0..4 {
        let path = root.0.join("file");
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
                fs::remove_file(&path).unwrap();
                fs::write(&path, "same content, new inode").unwrap();
            }
            _ => {
                fs::remove_file(&path).unwrap();
                symlink("elsewhere", &path).unwrap();
            }
        }
        approval.decision.send(true).unwrap();
        assert!(task.await.unwrap().is_err());
        assert_ne!(fs::read_to_string(&path).ok().as_deref(), Some("after"));
        no_temp(&root);
    }
}

#[tokio::test]
async fn rejects_unsafe_paths_links_binary_large_files_and_missing_parents_before_asking() {
    let root = Fixture::new();
    fs::write(root.0.join("binary"), [0, 1, 2]).unwrap();
    fs::write(root.0.join("large"), vec![b'x'; 1048577]).unwrap();
    fs::write(root.0.join("original"), "text").unwrap();
    fs::hard_link(root.0.join("original"), root.0.join("hard")).unwrap();
    symlink("original", root.0.join("symlink")).unwrap();
    symlink("/tmp", root.0.join("linked-dir")).unwrap();
    fs::create_dir(root.0.join("directory")).unwrap();
    for path in [
        "../escape",
        "/tmp/escape",
        ".git/new",
        ".env",
        "config.toml",
        ".orbit-write-owned",
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
            tool.execute(call(path, "text"), CancellationToken::new())
                .await
                .is_err(),
            "{path}"
        );
        assert!(approvals.try_recv().is_err());
    }
    let (tool, _) = root.tool();
    assert!(
        tool.execute(call("file", &"x".repeat(1048577)), CancellationToken::new())
            .await
            .is_err()
    );
    assert!(
        tool.execute(call("file", "a\0b"), CancellationToken::new())
            .await
            .is_err()
    );
    no_temp(&root);
}

#[tokio::test]
async fn protected_custom_files_and_parent_replacement_cannot_be_written() {
    let root = Fixture::new();
    let mut workspace = Workspace::new(&root.0).unwrap();
    workspace.protect(root.0.join("private.db")).unwrap();
    let (sender, mut receiver) = approval::channel();
    let tool = WriteFile::new(workspace, sender);
    assert!(
        tool.execute(call("private.db", "x"), CancellationToken::new())
            .await
            .is_err()
    );
    assert!(receiver.try_recv().is_err());
    fs::create_dir(root.0.join("dir")).unwrap();
    let (tool, mut approvals) = root.tool();
    let task = start(tool, "dir/file", "x", CancellationToken::new());
    let approval = request(&mut approvals).await;
    fs::rename(root.0.join("dir"), root.0.join("moved")).unwrap();
    fs::create_dir(root.0.join("dir")).unwrap();
    approval.decision.send(true).unwrap();
    assert!(task.await.unwrap().is_err());
    assert!(!root.0.join("dir/file").exists());
    assert!(!root.0.join("moved/file").exists());
}

#[tokio::test]
async fn protects_descendants_of_session_lock_directory() {
    let root = Fixture::new();
    fs::create_dir(root.0.join("session-locks")).unwrap();
    fs::write(root.0.join("session-locks/id"), "").unwrap();
    let mut workspace = Workspace::new(&root.0).unwrap();
    workspace.protect(root.0.join("session-locks")).unwrap();
    let (sender, mut receiver) = approval::channel();
    let tool = WriteFile::new(workspace, sender);
    assert!(
        tool.execute(call("session-locks/id", "x"), CancellationToken::new())
            .await
            .is_err()
    );
    assert!(receiver.try_recv().is_err());
}
