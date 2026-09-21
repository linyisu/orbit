use orbit_agent::tool::{Tool, ToolError, ToolFuture, ToolOutput};
use orbit_model::types::{ToolCall, ToolDefinition};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::{
    approval::{ApprovalPreview, ApprovalRequest, ApprovalSender, CapabilityPreview},
    capability::{EnvironmentId, FilesystemAccess, FilesystemCapability},
    path_security::ResolvedPath,
    policy::{FilesystemGrant, PolicyDecision},
    workspace::Workspace,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    path: String,
    access: String,
    reason: String,
}

#[derive(Clone)]
pub struct RequestCapability {
    workspace: Workspace,
    approvals: ApprovalSender,
}

impl RequestCapability {
    pub fn new(workspace: Workspace, approvals: ApprovalSender) -> Self {
        Self {
            workspace,
            approvals,
        }
    }
}

impl Tool for RequestCapability {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "request_capability",
            "Request permission to access a file or directory outside the workspace. Prompts the user with an approval dialog with the requested path, access level ('read' or 'read_write'), and reason. If approved, access is granted for the remainder of this session.",
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path outside the workspace (e.g. '~/.cargo' or '/tmp/build')"
                    },
                    "access": {
                        "type": "string",
                        "enum": ["read", "read_write", "write"],
                        "description": "Requested access level: 'read' for read-only, 'read_write' (or 'write') for read and write"
                    },
                    "reason": {
                        "type": "string",
                        "description": "Explanation to the user for why this access is necessary"
                    }
                },
                "required": ["path", "access", "reason"],
                "additionalProperties": false
            }),
        )
    }

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            let args: Arguments = serde_json::from_str(&call.function.arguments)
                .map_err(|e| invalid(format!("invalid arguments: {e}")))?;

            if args.path.trim().is_empty() {
                return Err(invalid("path must not be empty"));
            }

            if args.reason.trim().is_empty() {
                return Err(invalid("reason must not be empty"));
            }

            let access = match args.access.trim() {
                "read" => FilesystemAccess::Read,
                "read_write" | "write" => FilesystemAccess::ReadWrite,
                other => {
                    return Err(invalid(format!(
                        "access must be 'read' or 'write' (or 'read_write'), got '{other}'"
                    )));
                }
            };

            let expanded_path = expand_user_path(&args.path);
            let resolved = ResolvedPath::resolve(&expanded_path, Some(self.workspace.root()))
                .map_err(|e| {
                    ToolError::ExecutionFailed(format!("invalid path '{expanded_path}': {e}"))
                })?;

            let capability = FilesystemCapability {
                environment: EnvironmentId::Local,
                path: resolved.canonical().to_path_buf(),
                access,
            };

            match self.workspace.policy().check_filesystem(&capability) {
                PolicyDecision::Granted => Ok(ToolOutput::text(
                    json!({
                        "status": "granted",
                        "path": resolved.canonical().display().to_string(),
                        "access": capability.access.description(),
                        "message": "Capability is already granted"
                    })
                    .to_string(),
                )),
                PolicyDecision::Denied(reason) => Err(ToolError::ExecutionFailed(format!(
                    "permission permanently denied: {reason}"
                ))),
                PolicyDecision::NeedsApproval(caps) => {
                    if cancellation.is_cancelled() {
                        return Err(ToolError::Cancelled);
                    }

                    let (decision_tx, decision_rx) = oneshot::channel();
                    let request = ApprovalRequest {
                        preview: ApprovalPreview::Capability(CapabilityPreview {
                            environment: capability.environment,
                            path: capability.path.clone(),
                            access: capability.access,
                            reason: args.reason.clone(),
                        }),
                        decision: decision_tx,
                    };

                    tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => return Err(ToolError::Cancelled),
                        sent = self.approvals.send(request) => {
                            sent.map_err(|_| ToolError::ExecutionFailed("approval channel closed".into()))?;
                        }
                    }

                    let approved = tokio::select! {
                        biased;
                        _ = cancellation.cancelled() => return Err(ToolError::Cancelled),
                        answer = decision_rx => answer.unwrap_or(false),
                    };

                    if !approved {
                        return Err(ToolError::ExecutionFailed(format!(
                            "permission denied: user denied capability request for '{}'",
                            resolved.canonical().display()
                        )));
                    }

                    for cap in caps {
                        let grant_path = ResolvedPath::resolve(&cap.path, None)
                            .map_err(|e| ToolError::ExecutionFailed(e.to_string()))?;

                        let grant = FilesystemGrant {
                            environment: cap.environment,
                            path: grant_path,
                            read: true,
                            write: cap.access.is_write(),
                        };

                        self.workspace.policy().record_grant(grant);
                    }

                    Ok(ToolOutput::text(
                        json!({
                            "status": "granted",
                            "path": resolved.canonical().display().to_string(),
                            "access": capability.access.description()
                        })
                        .to_string(),
                    ))
                }
            }
        })
    }
}

fn expand_user_path(path: &str) -> String {
    if path == "~" {
        if let Ok(home) = std::env::var("HOME") {
            return home;
        }
    } else if let Some(stripped) = path.strip_prefix("~/")
        && let Ok(home) = std::env::var("HOME")
    {
        return format!("{home}/{stripped}");
    }

    path.to_string()
}

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidArguments {
        tool: "request_capability".into(),
        message: message.into(),
    }
}
