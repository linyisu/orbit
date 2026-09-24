use std::{path::PathBuf, time::Duration};

use orbit_agent::tool::{Tool, ToolError, ToolFuture};
use orbit_model::types::{ToolCall, ToolDefinition};
use orbit_runtime::execution::{CommandSpec, run};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::{
    approval::{ApprovalPreview, ApprovalRequest, ApprovalSender, CommandPreview},
    workspace::Workspace,
};

const DEFAULT_TIMEOUT_SECONDS: u64 = 120;
const MAX_TIMEOUT_SECONDS: u64 = 600;
const MAX_COMMAND_BYTES: usize = 16 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    command: String,
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

pub struct RunCommand {
    workspace: Workspace,
    approvals: ApprovalSender,
    rust_toolchain: Option<PathBuf>,
}

impl RunCommand {
    pub fn new(workspace: Workspace, approvals: ApprovalSender) -> Self {
        Self {
            workspace,
            approvals,
            rust_toolchain: None,
        }
    }

    pub fn with_toolchain(mut self, path: Option<PathBuf>) -> Self {
        self.rust_toolchain = path;
        self
    }
}

impl Tool for RunCommand {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "run_command",
            "Run a shell command in a fail-closed Bubblewrap sandbox with a disposable copy of the current workspace. Network access is allowed. Changes to the sandbox copy are discarded after this call; use write_file or edit_file for persistent project changes. /scratch is a disposable writable build directory for this invocation; Cargo uses it for its cache and target output. A host-configured Rust toolchain may be mounted read-only at /opt/orbit-rust. /tmp is temporary and writable. Protected configuration, session data, links to protected files, and Git metadata are excluded. Output is limited to 32 KiB per stream. Never retry a denied command without a new user request.",
            json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "Shell command to execute" },
                    "timeout_seconds": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_TIMEOUT_SECONDS,
                        "default": DEFAULT_TIMEOUT_SECONDS
                    }
                },
                "required": ["command"],
                "additionalProperties": false
            }),
        )
    }

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            let args: Arguments =
                serde_json::from_str(&call.function.arguments).map_err(|error| {
                    ToolError::InvalidArguments {
                        tool: "run_command".to_owned(),
                        message: error.to_string(),
                    }
                })?;
            let timeout_seconds = args.timeout_seconds.unwrap_or(DEFAULT_TIMEOUT_SECONDS);
            if args.command.trim().is_empty()
                || args.command.len() > MAX_COMMAND_BYTES
                || args.command.contains('\0')
                || !(1..=MAX_TIMEOUT_SECONDS).contains(&timeout_seconds)
            {
                return Err(ToolError::InvalidArguments {
                    tool: "run_command".to_owned(),
                    message: "command must be nonempty UTF-8 text without NUL, at most 16 KiB; timeout_seconds must be 1..600".to_owned(),
                });
            }
            if cancellation.is_cancelled() {
                return Err(ToolError::Cancelled);
            }

            let (decision, response) = oneshot::channel();
            let approval = ApprovalRequest {
                preview: ApprovalPreview::Command(CommandPreview {
                    command: args.command.clone(),
                    timeout_seconds,
                }),
                decision,
            };
            tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(ToolError::Cancelled),
                result = self.approvals.send(approval) => {
                    result.map_err(|_| ToolError::ExecutionFailed(
                        "command approval unavailable; no command was run".to_owned()
                    ))?;
                }
            }
            let allowed = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(ToolError::Cancelled),
                result = response => result.unwrap_or(false),
            };
            if !allowed {
                return Err(ToolError::ExecutionFailed(
                    "user denied command; do not retry without a new user request".to_owned(),
                ));
            }

            let result = run(
                self.workspace.clone(),
                CommandSpec {
                    command: args.command,
                    timeout: Duration::from_secs(timeout_seconds),
                    rust_toolchain: self.rust_toolchain.clone(),
                },
                cancellation,
            )
            .await
            .map_err(|error| match error {
                orbit_runtime::RuntimeError::Cancelled => ToolError::Cancelled,
                other => ToolError::ExecutionFailed(other.to_string()),
            })?;
            serde_json::to_string(&result)
                .map_err(|error| ToolError::ExecutionFailed(error.to_string()))
        })
    }
}
