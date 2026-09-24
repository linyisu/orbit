use orbit_agent::tool::{Tool, ToolError, ToolFuture};
use orbit_model::types::{ToolCall, ToolDefinition};
use orbit_runtime::execution::fingerprint_workspace;
use orbit_store::Store;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::workspace::Workspace;

pub struct VerificationStatus {
    workspace: Workspace,
    store: Store,
    session_id: String,
}

impl VerificationStatus {
    pub fn new(workspace: Workspace, store: Store, session_id: String) -> Self {
        Self {
            workspace,
            store,
            session_id,
        }
    }
}

impl Tool for VerificationStatus {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "verification_status",
            "Compare this session's recorded sandbox command inputs with the current protected workspace snapshot. Reports exit status, timeout, command output ID and whether source inputs still match. Call after final edits before claiming verification. A zero exit code only says that exact command succeeded; inspect its output and do not claim broader tests ran. This does not run commands or change files.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        )
    }

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            let args: serde_json::Value =
                serde_json::from_str(&call.function.arguments).map_err(|error| {
                    ToolError::InvalidArguments {
                        tool: "verification_status".to_owned(),
                        message: error.to_string(),
                    }
                })?;
            if !args.as_object().is_some_and(serde_json::Map::is_empty) {
                return Err(ToolError::InvalidArguments {
                    tool: "verification_status".to_owned(),
                    message: "no arguments expected".to_owned(),
                });
            }
            if cancellation.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            let snapshot = fingerprint_workspace(self.workspace.clone(), cancellation)
                .await
                .map_err(|error| match error {
                    orbit_runtime::RuntimeError::Cancelled => ToolError::Cancelled,
                    other => ToolError::ExecutionFailed(other.to_string()),
                })?;
            let records = self
                .store
                .verification_records(&self.session_id, 20)
                .await
                .map_err(|error| ToolError::ExecutionFailed(error.to_string()))?;
            let commands = records
                .iter()
                .map(|record| {
                    let source_state = if record.input_snapshot_id == snapshot {
                        "current"
                    } else {
                        "stale"
                    };
                    json!({
                        "output_id": record.output_id,
                        "command": record.command,
                        "exit_code": record.exit_code,
                        "timed_out": record.timed_out,
                        "elapsed_ms": record.elapsed_ms,
                        "input_snapshot_id": record.input_snapshot_id,
                        "source_state": source_state,
                        "succeeded": record.exit_code == Some(0) && !record.timed_out
                    })
                })
                .collect::<Vec<_>>();
            Ok(json!({
                "current_snapshot_id": snapshot,
                "commands": commands,
                "returned_commands": commands.len(),
                "limitations": "Source state compares filtered workspace inputs only. It does not prove what a command tested, account for external tools or network changes, or verify edits made inside disposable sandboxes. Read command output before reporting success; rerun checks after source edits."
            }).to_string())
        })
    }
}
