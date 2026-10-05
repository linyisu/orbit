use orbit_agent::tool::{Tool, ToolError, ToolFuture, ToolOutput};
use orbit_model::types::{ToolCall, ToolDefinition};
use orbit_runtime::{
    capability::{EnvironmentId, FilesystemAccess, FilesystemCapability},
    execution::process::{RESERVED_CAPABILITY_PATHS, RESERVED_CAPABILITY_TREES},
    filesystem::{ResolvedPath, Workspace},
    policy::{FilesystemGrant, PolicyDecision},
};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::approval::{ApprovalPreview, ApprovalRequest, ApprovalSender, CapabilityPreview};

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
        let description = format!(
            "Request session permission for an existing file or directory outside the workspace. Only eligible paths prompt the user for approval. Existing workspace access needs no request. To create external files, request read_write for an existing eligible parent directory. Policy denials are permanent and never open an approval dialog: workspace ancestors, protected files/directories and these exact host paths: {}; these host directories and all descendants: {}. The whole host /tmp is reserved, but an existing specific /tmp child outside /tmp/home may be eligible. run_command already provides isolated /tmp and /scratch for temporary work; these are sandbox paths, not host capability requests. Approved grants last for this session.",
            RESERVED_CAPABILITY_PATHS.join(", "),
            RESERVED_CAPABILITY_TREES.join(", ")
        );

        ToolDefinition::function(
            "request_capability",
            description,
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path outside the workspace (e.g. '~/.cargo' or '/tmp/build')" },
                    "access": {
                        "type": "string",
                        "enum": ["read", "read_write"],
                        "description": "Requested access level: 'read' for read-only, 'read_write' for read and write"
                    },
                    "reason": { "type": "string", "description": "Explanation to the user for why this access is necessary" }
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
                        "access must be 'read' or 'read_write' (or 'write'), got '{other}'"
                    )));
                }
            };

            let expanded_path = expand_user_path(&args.path);
            let resolved = ResolvedPath::resolve(&expanded_path, Some(self.workspace.root()))
                .map_err(|e| {
                    ToolError::ExecutionFailed(format!("invalid path '{expanded_path}': {e}"))
                })?;

            if !resolved.canonical().exists() {
                let parent = resolved.canonical().ancestors().skip(1).find(|parent| {
                    parent.is_dir()
                        && !matches!(
                            self.workspace
                                .policy()
                                .check_filesystem(&FilesystemCapability {
                                    environment: EnvironmentId::Local,
                                    path: parent.to_path_buf(),
                                    access,
                                }),
                            PolicyDecision::Denied(_)
                        )
                });
                let guidance = match parent {
                    Some(parent) => format!(
                        "to create external files, request read_write permission for the existing eligible parent '{}'",
                        parent.display()
                    ),

                    None => "no existing eligible parent is available; use the workspace or run_command's isolated /tmp or /scratch for temporary work".into(),
                };

                return Err(ToolError::ExecutionFailed(format!(
                    "path '{expanded_path}' does not exist. Capability mounts require existing resources; {guidance}"
                )));
            }

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
                    "permission permanently denied: {reason}. This policy denial cannot be overridden by user approval; do not retry the same request. Use an eligible external path, the workspace, or run_command's isolated temporary directories."
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
                            write: cap.access.allows_write(),
                        };

                        self.workspace.policy().record_grant(grant);
                    }

                    Ok(ToolOutput::text(
                        json!({ "status": "granted", "path": resolved.canonical().display().to_string(), "access": capability.access.description() })
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
