use std::fs;

use orbit_agent::tool::Tool;
use orbit_model::types::ToolCall;
use orbit_runtime::filesystem::Workspace;
use orbit_tools::{approval, read_file::ReadFile, request_capability::RequestCapability};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn reserved_paths_never_prompt_and_nested_missing_paths_suggest_an_eligible_parent() {
    let ws_temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let (sender, mut receiver) = approval::channel();
    let tool = RequestCapability::new(Workspace::new(ws_temp.path()).unwrap(), sender);

    for path in ["/", "/tmp", "/etc", "/etc/passwd"] {
        let error = tool
            .execute(
                ToolCall::function(
                    "reserved",
                    "request_capability",
                    json!({
                        "path": path, "access": "read", "reason": "test reserved paths"
                    }),
                ),
                CancellationToken::new(),
            )
            .await
            .unwrap_err()
            .to_string();

        assert!(
            error.contains("permission permanently denied"),
            "{path}: {error}"
        );
        assert!(error.contains("cannot be overridden by user approval"));
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }

    let error = tool.execute(ToolCall::function("missing", "request_capability", json!({
        "path": outside.path().join("missing/nested/file.txt"), "access": "read_write", "reason": "test missing parents"
    })), CancellationToken::new()).await.unwrap_err().to_string();
    assert!(error.contains(&format!(
        "existing eligible parent '{}'",
        outside.path().display()
    )));
    assert!(!error.contains(&format!(
        "parent '{}'",
        outside.path().join("missing/nested").display()
    )));
    assert!(receiver.try_recv().is_err());
}

#[tokio::test]
async fn request_capability_prompts_approval_and_enables_subsequent_reads() {
    let ws_temp = tempfile::tempdir().unwrap();
    let outside_temp = tempfile::tempdir().unwrap();

    let workspace = Workspace::new(ws_temp.path()).unwrap();
    let outside_file = outside_temp.path().join("external_data.txt");
    fs::write(&outside_file, "external secret contents\n").unwrap();

    let (sender, mut receiver) = approval::channel();
    let tool = RequestCapability::new(workspace.clone(), sender);

    let read_tool = ReadFile::new(workspace.clone());
    let initial_err = read_tool
        .execute(
            ToolCall::function(
                "call_1",
                "read_file",
                json!({ "path": outside_file.display().to_string() }),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();

    assert!(initial_err.to_string().contains("permission denied"));

    let req_file = outside_file.display().to_string();
    let task = tokio::spawn(async move {
        tool.execute(
            ToolCall::function(
                "call_2",
                "request_capability",
                json!({ "path": req_file, "access": "read", "reason": "Need to inspect external configuration data" }),
            ),
            CancellationToken::new(),
        )
        .await
    });

    let request = receiver.recv().await.unwrap();

    match &request.preview {
        approval::ApprovalPreview::Capability(preview) => {
            assert_eq!(
                preview.reason,
                "Need to inspect external configuration data"
            );
            assert_eq!(
                preview.access,
                orbit_runtime::capability::FilesystemAccess::Read
            );
        }

        _ => panic!("unexpected preview type"),
    }

    request.decision.send(true).unwrap();

    let output = task.await.unwrap().unwrap();
    let result_json: Value = serde_json::from_str(&output.content).unwrap();

    assert_eq!(result_json["status"], "granted");
    assert_eq!(result_json["access"], "read");

    let read_output = read_tool
        .execute(
            ToolCall::function(
                "call_3",
                "read_file",
                json!({ "path": outside_file.display().to_string() }),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let read_json: Value = serde_json::from_str(&read_output.content).unwrap();

    assert_eq!(read_json["content"], "1: external secret contents\n");
}

#[tokio::test]
async fn request_capability_handles_user_denial_and_cancellation() {
    let ws_temp = tempfile::tempdir().unwrap();
    let outside_temp = tempfile::tempdir().unwrap();

    let workspace = Workspace::new(ws_temp.path()).unwrap();
    let outside_file = outside_temp.path().join("forbidden.txt");
    fs::write(&outside_file, "top secret\n").unwrap();

    let (sender, mut receiver) = approval::channel();
    let tool = RequestCapability::new(workspace.clone(), sender);

    let req_file = outside_file.display().to_string();
    let task = tokio::spawn({
        let tool = tool.clone();

        async move {
            tool.execute(
                ToolCall::function(
                    "call_deny",
                    "request_capability",
                    json!({ "path": req_file, "access": "write", "reason": "Want to edit system files" }),
                ),
                CancellationToken::new(),
            )
            .await
        }
    });

    let request = receiver.recv().await.unwrap();
    request.decision.send(false).unwrap();

    let err = task.await.unwrap().unwrap_err();

    assert!(err.to_string().contains("permission denied"));

    let (sender2, _receiver2) = approval::channel();
    let tool2 = RequestCapability::new(workspace, sender2);
    let token = CancellationToken::new();
    token.cancel();

    let cancel_err = tool2
        .execute(
            ToolCall::function(
                "call_cancel",
                "request_capability",
                json!({ "path": outside_file.display().to_string(), "access": "read", "reason": "cancelled" }),
            ),
            token,
        )
        .await
        .unwrap_err();

    assert!(matches!(
        cancel_err,
        orbit_agent::tool::ToolError::Cancelled
    ));
}

#[tokio::test]
async fn request_capability_validates_arguments() {
    let ws_temp = tempfile::tempdir().unwrap();
    let workspace = Workspace::new(ws_temp.path()).unwrap();
    let (sender, _receiver) = approval::channel();
    let tool = RequestCapability::new(workspace, sender);

    let err = tool
        .execute(
            ToolCall::function(
                "bad_path",
                "request_capability",
                json!({ "path": "   ", "access": "read", "reason": "test" }),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();

    assert!(err.to_string().contains("path must not be empty"));

    let err2 = tool
        .execute(
            ToolCall::function(
                "bad_access",
                "request_capability",
                json!({ "path": "/tmp", "access": "execute", "reason": "test" }),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();

    assert!(
        err2.to_string()
            .contains("access must be 'read' or 'read_write'")
    );

    let err3 = tool
        .execute(
            ToolCall::function(
                "bad_reason",
                "request_capability",
                json!({ "path": "/tmp", "access": "read", "reason": "  " }),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();

    assert!(err3.to_string().contains("reason must not be empty"));
}

#[tokio::test]
async fn request_capability_supports_read_write_access() {
    let ws_temp = tempfile::tempdir().unwrap();
    let outside_temp = tempfile::tempdir().unwrap();

    let workspace = Workspace::new(ws_temp.path()).unwrap();
    let outside_file = outside_temp.path().join("rw_data.txt");
    fs::write(&outside_file, "initial\n").unwrap();

    let (sender, mut receiver) = approval::channel();
    let tool = RequestCapability::new(workspace.clone(), sender);

    let req_file = outside_file.display().to_string();
    let task = tokio::spawn(async move {
        tool.execute(
            ToolCall::function(
                "call_rw",
                "request_capability",
                json!({ "path": req_file, "access": "read_write", "reason": "Modify external config" }),
            ),
            CancellationToken::new(),
        )
        .await
    });

    let request = receiver.recv().await.unwrap();

    match &request.preview {
        approval::ApprovalPreview::Capability(preview) => {
            assert_eq!(
                preview.access,
                orbit_runtime::capability::FilesystemAccess::ReadWrite
            );
            assert!(preview.access.allows_read());
            assert!(preview.access.allows_write());
        }

        _ => panic!("unexpected preview type"),
    }

    request.decision.send(true).unwrap();

    let output = task.await.unwrap().unwrap();
    let result_json: Value = serde_json::from_str(&output.content).unwrap();

    assert_eq!(result_json["status"], "granted");
    assert_eq!(result_json["access"], "read and write");
}
