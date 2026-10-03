use std::fs;

use orbit_agent::tool::Tool;
use orbit_model::types::ToolCall;
use orbit_runtime::{
    capability::EnvironmentId,
    filesystem::{ResolvedPath, Workspace},
    policy::FilesystemGrant,
};
use orbit_tools::{
    approval, edit_file::EditFile, image_assets::ImageAssets, list_files::ListFiles,
    read_document::ReadDocument, read_file::ReadFile, read_image::ReadImage,
    search_text::SearchText, write_file::WriteFile,
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
                json!({ "path": &ext_file_str, "old_text": "initial line", "new_text": "hacked line" }),
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

    let resolved_ext_dir = ResolvedPath::resolve(&ext_dir, None).unwrap();
    workspace.policy().record_grant(FilesystemGrant::read_write(
        EnvironmentId::Local,
        resolved_ext_dir,
    ));

    let read_out = read_tool
        .execute(
            ToolCall::function("r2", "read_file", json!({ "path": &ext_file_str })),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert!(read_out.content.contains("initial line"));

    let write_fut = write_tool.execute(
        ToolCall::function(
            "w2",
            "write_file",
            json!({ "path": &ext_file_str, "content": "replaced external content\nsecond line\n" }),
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

    let edit_result = edit_tool
        .execute(
            ToolCall::function(
                "e2",
                "edit_file",
                json!({ "path": &ext_file_str, "old_text": "second line", "new_text": "edited external line" }),
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

    let list_out = list_tool
        .execute(
            ToolCall::function("l2", "list_files", json!({ "path": &ext_dir_str })),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert!(list_out.content.contains("config.txt"));
    assert!(list_out.content.contains("\"kind\":\"file\""));

    let search_out = search_tool
        .execute(
            ToolCall::function(
                "s2",
                "search_text",
                json!({ "path": &ext_dir_str, "query": "edited external" }),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert!(search_out.content.contains("config.txt"));
    assert!(search_out.content.contains("edited external line"));
}

#[tokio::test]
async fn external_special_readers_integration() {
    let ws_temp = tempfile::tempdir().unwrap();
    let ext_temp = tempfile::tempdir().unwrap();
    let db_temp = tempfile::tempdir().unwrap();

    let workspace = Workspace::new(ws_temp.path()).unwrap();
    let ext_dir = ext_temp.path().join("external_docs");
    fs::create_dir_all(&ext_dir).unwrap();

    let ext_doc = ext_dir.join("data.csv");
    fs::write(&ext_doc, "name,value\nalpha,1\nbeta,2\n").unwrap();

    let ext_rtf = ext_dir.join("orbit-guide.rtf");
    fs::write(
        &ext_rtf,
        "{\\rtf1\\ansi\\deff0 {\\fonttbl {\\f0 Courier;}}\\f0\\fs24 Orbit tool guide}",
    )
    .unwrap();

    let ext_img = ext_dir.join("orbit-fixture.png");
    image::RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 255]))
        .save(&ext_img)
        .unwrap();

    let doc_tool = ReadDocument::new(workspace.clone())
        .with_worker(env!("CARGO_BIN_EXE_orbit-document-worker").into());
    let img_tool = ReadImage::new(
        workspace.clone(),
        ImageAssets::for_database(&db_temp.path().join("orbit.db")),
    );

    let ext_doc_str = ext_doc.display().to_string();
    let ext_rtf_str = ext_rtf.display().to_string();
    let ext_img_str = ext_img.display().to_string();

    let doc_err = doc_tool
        .execute(
            ToolCall::function("d1", "read_document", json!({ "path": &ext_doc_str })),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();

    assert!(doc_err.to_string().contains("permission denied"));
    assert!(doc_err.to_string().contains("request_capability"));

    let rtf_err = doc_tool
        .execute(
            ToolCall::function("d_rtf", "read_document", json!({ "path": &ext_rtf_str })),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();

    assert!(rtf_err.to_string().contains("permission denied"));
    assert!(rtf_err.to_string().contains("request_capability"));

    let img_err = img_tool
        .execute(
            ToolCall::function("i1", "read_image", json!({ "path": &ext_img_str })),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();

    assert!(img_err.to_string().contains("permission denied"));
    assert!(img_err.to_string().contains("request_capability"));

    let resolved_ext_dir = ResolvedPath::resolve(&ext_dir, None).unwrap();
    workspace.policy().record_grant(FilesystemGrant::read_only(
        EnvironmentId::Local,
        resolved_ext_dir,
    ));

    let doc_out = doc_tool
        .execute(
            ToolCall::function("d2", "read_document", json!({ "path": &ext_doc_str })),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert!(doc_out.content.contains("local_document_markdown"));
    assert!(doc_out.content.contains("alpha"));

    let rtf_out = doc_tool
        .execute(
            ToolCall::function("d2_rtf", "read_document", json!({ "path": &ext_rtf_str })),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert!(rtf_out.content.contains("local_document_markdown"));
    assert!(rtf_out.content.contains("Orbit tool guide"));

    let img_out = img_tool
        .execute(
            ToolCall::function("i2", "read_image", json!({ "path": &ext_img_str })),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert!(img_out.content.contains("local_image"));
    assert_eq!(img_out.attachments.len(), 1);

    let rel_doc = format!(
        "../{}/external_docs/data.csv",
        ext_temp.path().file_name().unwrap().to_str().unwrap()
    );
    let rel_doc_out = doc_tool
        .execute(
            ToolCall::function("d3_rel", "read_document", json!({ "path": rel_doc })),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert!(rel_doc_out.content.contains("local_document_markdown"));
    assert!(rel_doc_out.content.contains("alpha"));
}
