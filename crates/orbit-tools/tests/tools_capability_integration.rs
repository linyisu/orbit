use std::fs;

use orbit_agent::tool::Tool;
use orbit_model::types::ToolCall;
use orbit_tools::{
    approval, capability::EnvironmentId, edit_file::EditFile, list_files::ListFiles,
    path_security::ResolvedPath, policy::FilesystemGrant, read_file::ReadFile,
    search_text::SearchText, workspace::Workspace, write_file::WriteFile,
};
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn external_path_integration_across_all_tools() {
    let ws_temp = tempfile::tempdir().unwrap();
    let ext_temp = tempfile::tempdir().unwrap();

    let workspace = Workspace::new(ws_temp.path()).unwrap();
    let ext_dir = ext_temp.path().join("external_project");
    fs::create_dir_all(&ext_dir).unwrap();

    let ext_file = ext_dir.join("config.txt");
    fs::write(&ext_file, "initial line\nsecond line\n").unwrap();

    let (write_approval_tx, mut write_approval_rx) = approval::channel();

    let read_tool = ReadFile::new(workspace.clone());
    let write_tool = WriteFile::new(workspace.clone(), write_approval_tx.clone());
    let edit_tool = EditFile::new(workspace.clone(), write_approval_tx);
    let list_tool = ListFiles::new(workspace.clone());
    let search_tool = SearchText::new(workspace.clone());

    let ext_file_str = ext_file.display().to_string();
    let ext_dir_str = ext_dir.display().to_string();

    // 1. Without capability grants, all tools must reject external paths with permission denied
    let read_err = read_tool
        .execute(
            ToolCall::function("r1", "read_file", json!({ "path": &ext_file_str })),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(read_err.to_string().contains("permission denied"));
    assert!(read_err.to_string().contains("request_capability"));

    let write_err = write_tool
        .execute(
            ToolCall::function(
                "w1",
                "write_file",
                json!({ "path": &ext_file_str, "content": "forbidden" }),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(write_err.to_string().contains("permission denied"));
    assert!(write_err.to_string().contains("request_capability"));

    let edit_err = edit_tool
        .execute(
            ToolCall::function(
                "e1",
                "edit_file",
                json!({
                    "path": &ext_file_str,
                    "old_text": "initial line",
                    "new_text": "hacked line"
                }),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(edit_err.to_string().contains("permission denied"));
    assert!(edit_err.to_string().contains("request_capability"));

    let list_err = list_tool
        .execute(
            ToolCall::function("l1", "list_files", json!({ "path": &ext_dir_str })),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(list_err.to_string().contains("permission denied"));
    assert!(list_err.to_string().contains("request_capability"));

    let search_err = search_tool
        .execute(
            ToolCall::function(
                "s1",
                "search_text",
                json!({ "path": &ext_dir_str, "query": "second" }),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(search_err.to_string().contains("permission denied"));
    assert!(search_err.to_string().contains("request_capability"));

    // 2. Grant ReadWrite capability on ext_dir for this session
    let resolved_ext_dir = ResolvedPath::resolve(&ext_dir, None).unwrap();
    workspace.policy().record_grant(FilesystemGrant::read_write(
        EnvironmentId::Local,
        resolved_ext_dir,
    ));

    // 3. Now read_file succeeds on the granted outside path
    let read_out = read_tool
        .execute(
            ToolCall::function("r2", "read_file", json!({ "path": &ext_file_str })),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(read_out.content.contains("initial line"));

    // 4. write_file to the granted outside path prompts for write approval and succeeds
    let write_fut = write_tool.execute(
        ToolCall::function(
            "w2",
            "write_file",
            json!({
                "path": &ext_file_str,
                "content": "replaced external content\nsecond line\n"
            }),
        ),
        CancellationToken::new(),
    );
    let approve_task = tokio::spawn(async move {
        let request = write_approval_rx.recv().await.unwrap();
        match request.preview {
            approval::ApprovalPreview::Write(preview) => {
                assert!(preview.after.contains("replaced external content"));
            }
            other => panic!("expected write preview, got {other:?}"),
        }
        let _ = request.decision.send(true);

        // Expect second approval for edit_file below
        let edit_request = write_approval_rx.recv().await.unwrap();
        match edit_request.preview {
            approval::ApprovalPreview::Write(preview) => {
                assert!(preview.after.contains("edited external line"));
            }
            other => panic!("expected edit write preview, got {other:?}"),
        }
        let _ = edit_request.decision.send(true);
    });

    let write_result = write_fut.await.unwrap();
    assert!(write_result.content.contains("written_bytes"));
    assert_eq!(
        fs::read_to_string(&ext_file).unwrap(),
        "replaced external content\nsecond line\n"
    );

    // 5. edit_file succeeds on the granted outside path
    let edit_result = edit_tool
        .execute(
            ToolCall::function(
                "e2",
                "edit_file",
                json!({
                    "path": &ext_file_str,
                    "old_text": "second line",
                    "new_text": "edited external line"
                }),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(edit_result.content.contains("written_bytes"));
    assert_eq!(
        fs::read_to_string(&ext_file).unwrap(),
        "replaced external content\nedited external line\n"
    );

    approve_task.await.unwrap();

    // 6. list_files lists the granted outside directory
    let list_out = list_tool
        .execute(
            ToolCall::function("l2", "list_files", json!({ "path": &ext_dir_str })),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(list_out.content.contains("config.txt"));
    assert!(list_out.content.contains("\"kind\":\"file\""));

    // 7. search_text searches the granted outside directory
    let search_out = search_tool
        .execute(
            ToolCall::function(
                "s2",
                "search_text",
                json!({
                    "path": &ext_dir_str,
                    "query": "edited external"
                }),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(search_out.content.contains("config.txt"));
    assert!(search_out.content.contains("edited external line"));
}
