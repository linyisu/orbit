use orbit_agent::tool::Tool;
use orbit_model::types::ToolCall;
use orbit_runtime::filesystem::Workspace;
use orbit_tools::read_document::ReadDocument;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn standalone_worker_returns_paginated_content_and_invalidates_source_cache() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("table.csv");
    std::fs::write(&path, "name,value\nalpha,1\nbeta,2\n").unwrap();
    let tool = ReadDocument::new(Workspace::new(dir.path()).unwrap())
        .with_worker(env!("CARGO_BIN_EXE_orbit-document-worker").into());
    let call = |arguments| ToolCall::function("document", "read_document", arguments);
    let first: Value = serde_json::from_str(
        &tool
            .execute(
                call(json!({"path":"table.csv","max_chars":10})),
                CancellationToken::new(),
            )
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(first["next_start_char"], 10);
    assert!(first["pdf"].is_null());
    std::fs::write(&path, "name,value\nchanged,42\n").unwrap();
    let next = tool
        .execute(call(json!({"path":"table.csv"})), CancellationToken::new())
        .await
        .unwrap();
    assert!(next.contains("changed"));
    let invalid = tool
        .execute(
            call(json!({"path":"table.csv","start_page":2})),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(invalid.to_string().contains("only for PDFs"));
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}
