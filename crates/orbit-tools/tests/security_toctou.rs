use std::fs;

use orbit_agent::tool::Tool;
use orbit_model::types::ToolCall;
use orbit_runtime::filesystem::Workspace;
use orbit_tools::{approval, read_file::ReadFile, write_file::WriteFile};
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn ungranted_symlink_pointing_outside_is_blocked() {
    let ws_temp = tempfile::tempdir().unwrap();
    let ext_temp = tempfile::tempdir().unwrap();

    let workspace = Workspace::new(ws_temp.path()).unwrap();
    let external_target = ext_temp.path().join("passwd");
    fs::write(&external_target, "root:x:0:0:...").unwrap();

    let symlink = ws_temp.path().join("evil_link");
    std::os::unix::fs::symlink(&external_target, &symlink).unwrap();

    let read_tool = ReadFile::new(workspace.clone());
    let err = read_tool
        .execute(
            ToolCall::function(
                "r2",
                "ReadFile",
                json!({
                    "path": "evil_link"
                }),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();

    assert!(
        err.to_string().contains("symlink")
            || err
                .to_string()
                .contains("permission denied")
    );

    let (tx, mut approvals) = approval::channel();
    let write_tool = WriteFile::new(workspace, tx);
    let write_err = write_tool
        .execute(
            ToolCall::function(
                "w2",
                "WriteFile",
                json!({
                    "path": "evil_link",
                    "content": "attack"
                }),
            ),
            CancellationToken::new(),
        )
        .await;

    assert!(write_err.is_err());
    assert!(approvals.try_recv().is_err());
    assert_eq!(
        fs::read_to_string(external_target).unwrap(),
        "root:x:0:0:..."
    );
}
