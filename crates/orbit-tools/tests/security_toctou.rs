use std::fs;

use orbit_agent::tool::Tool;
use orbit_model::types::ToolCall;
use orbit_runtime::{
    capability::EnvironmentId,
    filesystem::{ResolvedPath, Workspace, WorkspaceError},
    policy::FilesystemGrant,
};
use orbit_tools::{approval, read_file::ReadFile, write_file::WriteFile};
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn protected_files_and_hardlinks_cannot_be_accessed_even_with_capability() {
    let ws_temp = tempfile::tempdir().unwrap();
    let ext_temp = tempfile::tempdir().unwrap();

    let mut workspace = Workspace::new(ws_temp.path()).unwrap();
    let secret = ext_temp.path().join("secret.key");
    fs::write(&secret, "super-secret-key").unwrap();

    // Protect the secret
    workspace.protect(&secret).unwrap();

    // Grant capability to ext_temp directory
    let resolved_ext = ResolvedPath::resolve(ext_temp.path(), None).unwrap();
    workspace.policy().record_grant(FilesystemGrant::read_write(
        EnvironmentId::Local,
        resolved_ext,
    ));

    // open_granted_read must reject protected path
    let err = workspace.open_granted_read(&secret).unwrap_err();

    assert!(matches!(err, WorkspaceError::ProtectedPath));

    // Hard link to the secret must also be rejected
    let link = ext_temp.path().join("link_to_secret.key");
    fs::hard_link(&secret, &link).unwrap();
    let err_link = workspace.open_granted_read(&link).unwrap_err();

    assert!(matches!(err_link, WorkspaceError::ProtectedPath));

    // Writing to protected path must also be rejected
    let err_write = workspace.open_granted_write_parent(&secret).unwrap_err();

    assert!(matches!(err_write, WorkspaceError::ProtectedPath));

    // ReadFile tool must also reject it
    let read_tool = ReadFile::new(workspace);
    let tool_err = read_tool
        .execute(
            ToolCall::function(
                "r1",
                "read_file",
                json!({ "path": secret.display().to_string() }),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();

    assert!(tool_err.to_string().contains("blocked"));
}

#[tokio::test]
async fn ungranted_symlink_pointing_outside_is_blocked() {
    let ws_temp = tempfile::tempdir().unwrap();
    let ext_temp = tempfile::tempdir().unwrap();

    let workspace = Workspace::new(ws_temp.path()).unwrap();
    let external_target = ext_temp.path().join("passwd");
    fs::write(&external_target, "root:x:0:0:...").unwrap();

    // Create a symlink inside workspace pointing outside
    let symlink = ws_temp.path().join("evil_link");
    std::os::unix::fs::symlink(&external_target, &symlink).unwrap();

    let read_tool = ReadFile::new(workspace.clone());
    let err = read_tool
        .execute(
            ToolCall::function("r2", "read_file", json!({ "path": "evil_link" })),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();

    // This link resolves outside the workspace without a capability grant.
    assert!(err.to_string().contains("symlink") || err.to_string().contains("permission denied"));

    // Writing to symlink inside workspace is rejected
    let (tx, _) = approval::channel();
    let write_tool = WriteFile::new(workspace, tx);
    let write_err = write_tool
        .execute(
            ToolCall::function(
                "w2",
                "write_file",
                json!({ "path": "evil_link", "content": "attack" }),
            ),
            CancellationToken::new(),
        )
        .await;

    assert!(write_err.is_err());
}
