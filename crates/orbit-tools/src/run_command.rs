use std::time::Duration;

use orbit_agent::tool::{Tool, ToolError, ToolFuture};
use orbit_model::types::{ToolCall, ToolDefinition};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::{
    approval::{ApprovalPreview, ApprovalRequest, ApprovalSender, CommandPreview},
    sandbox,
    workspace::Workspace,
};

pub struct RunCommand {
    workspace: Workspace,
    approvals: ApprovalSender,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    command: String,
    #[serde(default)]
    writable_snapshot: bool,
    #[serde(default = "default_timeout")]
    timeout_seconds: u64,
}
fn default_timeout() -> u64 {
    30
}

impl RunCommand {
    pub fn new(workspace: Workspace, approvals: ApprovalSender) -> Self {
        Self {
            workspace,
            approvals,
        }
    }
}

impl Tool for RunCommand {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "run_command",
            "Run a /bin/sh command after one-time user approval in a Linux Bubblewrap sandbox. No network, credentials or host home. /workspace is a filtered snapshot, read-only by default. Set writable_snapshot=true to build or test in a disposable writable copy (128 MiB total including source). All changes and build artifacts are discarded after the command; combine build and test in one command. /tmp is disposable. Cannot modify the original project. Metadata, symlinks, target/node_modules/.venv/__pycache__ are omitted. Snapshot limit 64 MiB/10000 entries. Only system tools in /usr/bin and /bin are available. Default timeout 30 seconds; maximum 120. stdout/stderr each capped at 32 KiB; exceeding a cap stops the process. Never automatically retry a denied command.",
            json!({"type":"object","properties":{"writable_snapshot":{"type":"boolean","default":false},"command":{"type":"string"},"timeout_seconds":{"type":"integer","minimum":1,"maximum":120,"default":30}},"required":["command"],"additionalProperties":false}),
        )
    }

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            let args: Arguments = serde_json::from_str(&call.function.arguments)
                .map_err(|e| invalid(e.to_string()))?;
            if args.command.trim().is_empty()
                || args.command.len() > 8192
                || args.command.contains('\0')
                || !(1..=120).contains(&args.timeout_seconds)
            {
                return Err(invalid(
                    "command must be nonempty, at most 8 KiB without NUL; timeout_seconds must be 1..120",
                ));
            }
            let (sender, receiver) = oneshot::channel();
            let request = ApprovalRequest {
                preview: ApprovalPreview::Command(CommandPreview {
                    command: args.command.clone(),
                    writable_snapshot: args.writable_snapshot,
                    timeout_seconds: args.timeout_seconds,
                }),
                decision: sender,
            };
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(ToolError::Cancelled),
                result = self.approvals.send(request) => result.map_err(|_| ToolError::ExecutionFailed("command approval unavailable".into()))?,
            }
            let approved = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(ToolError::Cancelled),
                answer = receiver => answer.unwrap_or(false),
            };
            if !approved {
                return Err(ToolError::ExecutionFailed(
                    "user denied command; do not retry without a new user request".into(),
                ));
            }
            let result = sandbox::run_with_options(
                self.workspace.clone(),
                args.command,
                Duration::from_secs(args.timeout_seconds),
                cancellation,
                args.writable_snapshot,
            )
            .await?;
            serde_json::to_string(&result).map_err(|e| ToolError::ExecutionFailed(e.to_string()))
        })
    }
}

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidArguments {
        tool: "run_command".into(),
        message: message.into(),
    }
}
