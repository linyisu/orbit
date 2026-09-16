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
    limits: CommandLimits,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    command: String,
    #[serde(default)]
    writable_snapshot: bool,
    #[serde(default)]
    timeout_seconds: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct CommandLimits {
    pub default_timeout_seconds: u64,
    pub max_timeout_seconds: u64,
}

impl Default for CommandLimits {
    fn default() -> Self {
        Self {
            default_timeout_seconds: 120,
            max_timeout_seconds: 600,
        }
    }
}

impl CommandLimits {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.default_timeout_seconds == 0
            || self.default_timeout_seconds > self.max_timeout_seconds
        {
            return Err(
                "command timeout defaults must be positive and not exceed max_timeout_seconds",
            );
        }
        Ok(())
    }
}

impl RunCommand {
    pub fn new(workspace: Workspace, approvals: ApprovalSender) -> Self {
        Self::with_limits(workspace, approvals, CommandLimits::default())
    }

    pub fn with_limits(
        workspace: Workspace,
        approvals: ApprovalSender,
        limits: CommandLimits,
    ) -> Self {
        Self {
            workspace,
            approvals,
            limits,
        }
    }
}

impl Tool for RunCommand {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "run_command",
            "Run a /bin/sh command under the user-selected approval policy in a Linux Bubblewrap sandbox. No network, credentials or host home. /workspace is a filtered snapshot, read-only by default. Set writable_snapshot=true to build or test in a disposable writable copy (128 MiB total including source). All changes and build artifacts are discarded after the command; combine build and test in one command. /tmp is disposable. Cannot modify the original project. Metadata, symlinks, target/node_modules/.venv/__pycache__ are omitted. Snapshot limit 64 MiB/10000 entries. Only system tools in /usr/bin and /bin are available. Timeout defaults and maximum are specified in the tool schema. stdout/stderr each capped at 32 KiB; exceeding a cap stops the process. Never automatically retry a denied command.",
            json!({"type":"object","properties":{"writable_snapshot":{"type":"boolean","default":false},"command":{"type":"string"},"timeout_seconds":{"type":"integer","minimum":1,"maximum":self.limits.max_timeout_seconds,"default":self.limits.default_timeout_seconds}},"required":["command"],"additionalProperties":false}),
        )
    }

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            let args: Arguments = serde_json::from_str(&call.function.arguments)
                .map_err(|e| invalid(e.to_string()))?;
            self.limits.validate().map_err(invalid)?;
            let timeout_seconds = args
                .timeout_seconds
                .unwrap_or(self.limits.default_timeout_seconds);
            if args.command.trim().is_empty()
                || args.command.len() > 8192
                || args.command.contains('\0')
                || !(1..=self.limits.max_timeout_seconds).contains(&timeout_seconds)
            {
                return Err(invalid(format!(
                    "command must be nonempty, at most 8 KiB without NUL; timeout_seconds must be 1..{}",
                    self.limits.max_timeout_seconds
                )));
            }
            let (sender, receiver) = oneshot::channel();
            let request = ApprovalRequest {
                preview: ApprovalPreview::Command(CommandPreview {
                    command: args.command.clone(),
                    writable_snapshot: args.writable_snapshot,
                    timeout_seconds,
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
                Duration::from_secs(timeout_seconds),
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
