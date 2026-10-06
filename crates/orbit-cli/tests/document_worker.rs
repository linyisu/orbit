use orbit_agent::tool::Tool;
use orbit_model::types::ToolCall;
use orbit_runtime::filesystem::Workspace;
use orbit_tools::read_document::ReadDocument;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn isolated_document_worker_converts_pages_and_invalidates_changed_source() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("data.csv"),
        "name,value\nalpha,1\nbeta,2\n",
    )
    .unwrap();
    let tool = ReadDocument::new(Workspace::new(root.path()).unwrap())
        .with_worker(env!("CARGO_BIN_EXE_orbit-cli").into());
    let request = |args| ToolCall::function("document", "read_document", args);
    let first: Value = serde_json::from_str(
        &tool
            .execute(
                request(json!({"path":"data.csv","max_chars":12})),
                CancellationToken::new(),
            )
            .await
            .unwrap(),
    )
    .unwrap();

    assert_eq!(first["truncated"], true);
    let next: Value = serde_json::from_str(
        &tool
            .execute(
                request(json!({"path":"data.csv","start_char":first["next_start_char"]})),
                CancellationToken::new(),
            )
            .await
            .unwrap(),
    )
    .unwrap();

    assert_eq!(first["total_chars"], next["total_chars"]);
    std::fs::write(root.path().join("data.csv"), "name,value\nchanged,9\n").unwrap();
    assert!(
        tool.execute(
            request(json!({"path":"data.csv"})),
            CancellationToken::new()
        )
        .await
        .unwrap()
        .contains("changed")
    );
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    let token = CancellationToken::new();
    token.cancel();
    assert!(
        tool.execute(request(json!({"path":"data.csv"})), token)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn cancellation_terminates_a_running_conversion_worker() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("data.csv"), "a,b\n1,2\n").unwrap();
    let worker = root.path().join("slow-worker");
    std::fs::write(&worker, "#!/bin/sh\nexec /bin/sleep 30\n").unwrap();
    std::fs::set_permissions(&worker, std::fs::Permissions::from_mode(0o700)).unwrap();
    let tool = ReadDocument::new(Workspace::new(root.path()).unwrap()).with_worker(worker);
    let token = CancellationToken::new();
    let running = token.clone();
    let task = tokio::spawn(async move {
        tool.execute(
            ToolCall::function("cancel", "read_document", json!({"path":"data.csv"})),
            running,
        )
        .await
    });

    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    token.cancel();
    let result = tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.unwrap_err(), orbit_agent::tool::ToolError::Cancelled);
}
