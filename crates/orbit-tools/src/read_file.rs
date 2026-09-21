use std::{io::Read, path::Path};

use orbit_agent::tool::{Tool, ToolError, ToolFuture, ToolOutput};
use orbit_model::types::{ToolCall, ToolDefinition};
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::workspace::Workspace;

const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 32 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArguments {
    path: String,
    #[serde(default = "first_line")]
    start_line: usize,
    #[serde(default = "default_limit")]
    max_lines: usize,
}

fn first_line() -> usize {
    1
}
fn default_limit() -> usize {
    200
}

pub struct ReadFile {
    workspace: Workspace,
}

impl ReadFile {
    pub fn new(workspace: Workspace) -> Self {
        Self { workspace }
    }
}

impl Tool for ReadFile {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "read_file",
            "Read a UTF-8 file inside the current workspace. Use a relative path and optional 1-based start_line/max_lines. Returns JSON with numbered lines. Symlinks and protected configuration files are rejected. Maximum file size: 1 MiB; maximum content field: 32 KiB.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path relative to the workspace root" },
                    "start_line": { "type": "integer", "minimum": 1, "default": 1 },
                    "max_lines": { "type": "integer", "minimum": 1, "maximum": 500, "default": 200 }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        )
    }

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            let arguments: ReadArguments = serde_json::from_str(&call.function.arguments)
                .map_err(|error| invalid(error.to_string()))?;
            if arguments.path.trim().is_empty()
                || arguments.start_line == 0
                || !(1..=500).contains(&arguments.max_lines)
            {
                return Err(invalid(
                    "path must be nonempty, start_line >= 1, and max_lines between 1 and 500",
                ));
            }
            if cancellation.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            let workspace = self.workspace.clone();
            let token = cancellation.clone();
            let task = tokio::task::spawn_blocking(move || read(workspace, arguments, token));
            tokio::select! {
                _ = cancellation.cancelled() => Err(ToolError::Cancelled),
                result = task => result.map_err(|error| ToolError::ExecutionFailed(error.to_string()))?,
            }.map(ToolOutput::text)
        })
    }
}

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidArguments {
        tool: "read_file".into(),
        message: message.into(),
    }
}

fn read(
    workspace: Workspace,
    arguments: ReadArguments,
    cancellation: CancellationToken,
) -> Result<String, ToolError> {
    let cap = crate::capability::FilesystemCapability {
        environment: crate::capability::EnvironmentId::Local,
        path: std::path::PathBuf::from(&arguments.path),
        access: crate::capability::FilesystemAccess::Read,
    };

    match workspace.policy().check_filesystem(&cap) {
        crate::policy::PolicyDecision::Granted => {}
        crate::policy::PolicyDecision::NeedsApproval(_) => {
            return Err(ToolError::ExecutionFailed(format!(
                "permission denied: '{}' is outside the workspace. Call request_capability to request access.",
                arguments.path
            )));
        }
        crate::policy::PolicyDecision::Denied(reason) => {
            return Err(ToolError::ExecutionFailed(format!(
                "permission denied: {reason}"
            )));
        }
    }

    let file = workspace
        .open_granted_read(Path::new(&arguments.path))
        .map_err(|error| ToolError::ExecutionFailed(error.to_string()))?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| ToolError::ExecutionFailed(error.to_string()))?;
    if cancellation.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(ToolError::ExecutionFailed(
            "file exceeds the 1 MiB read limit".into(),
        ));
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| ToolError::ExecutionFailed("file is not UTF-8 text".into()))?;
    if text.contains('\0') {
        return Err(ToolError::ExecutionFailed(
            "binary files are not supported".into(),
        ));
    }
    let lines: Vec<_> = text.lines().collect();
    if arguments.start_line > lines.len().max(1) {
        return Err(invalid(format!(
            "start_line exceeds file length ({} lines)",
            lines.len()
        )));
    }
    let mut content = String::new();
    let mut end_line = arguments.start_line.saturating_sub(1);
    let mut output_truncated = false;
    for (index, line) in lines
        .iter()
        .enumerate()
        .skip(arguments.start_line - 1)
        .take(arguments.max_lines)
    {
        let numbered = format!("{}: {line}\n", index + 1);
        let available = MAX_OUTPUT_BYTES.saturating_sub(content.len());
        if numbered.len() > available {
            let mut boundary = available;
            while !numbered.is_char_boundary(boundary) {
                boundary -= 1;
            }
            content.push_str(&numbered[..boundary]);
            end_line = index + 1;
            output_truncated = true;
            break;
        }
        content.push_str(&numbered);
        end_line = index + 1;
    }
    Ok(json!({
        "path": arguments.path,
        "start_line": arguments.start_line,
        "end_line": end_line,
        "total_lines": lines.len(),
        "truncated": output_truncated || end_line < lines.len(),
        "content": content,
    })
    .to_string())
}
