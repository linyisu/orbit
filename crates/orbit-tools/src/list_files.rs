use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
};

use orbit_agent::tool::{Tool, ToolError, ToolFuture, ToolOutput};
use orbit_model::types::{ToolCall, ToolDefinition};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use orbit_runtime::capability::{EnvironmentId, FilesystemAccess, FilesystemCapability};
use orbit_runtime::filesystem::{Workspace, WorkspaceError};
use orbit_runtime::policy::PolicyDecision;

const MAX_VISITED: usize = 20_000;
const MAX_OUTPUT: usize = 32 * 1024;

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Arguments {
    path: String,
    depth: usize,
    offset: usize,
    limit: usize,
    include_hidden: bool,
}
impl Default for Arguments {
    fn default() -> Self {
        Self {
            path: ".".into(),
            depth: 1,
            offset: 0,
            limit: 200,
            include_hidden: false,
        }
    }
}

pub struct ListFiles {
    workspace: Workspace,
}
impl ListFiles {
    pub fn new(workspace: Workspace) -> Self {
        Self { workspace }
    }
}
impl Tool for ListFiles {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "list_files",
            "Browse files and directories in the workspace (or outside paths granted via request_capability) without running a shell. Paths are relative to the workspace root, or absolute paths granted via request_capability. Defaults to one directory level; depth 1–8, deterministic breadth-first ordering, paginated with offset/next_offset. Hidden entries are omitted by default; include_hidden includes hidden entries but never protected paths. Generated directories are listed but not recursively traversed; select their path explicitly to browse them. Does not apply .gitignore. Symlinks and special files are omitted. Results are bounded to 32 KiB and 20000 examined entries; narrow path if scan_truncated is true.",
            json!({"type":"object","properties":{
                "path":{"type":"string","default":".","description":"Directory path (workspace-relative, or outside path granted via request_capability)"},
                "depth":{"type":"integer","minimum":1,"maximum":8,"default":1},
                "offset":{"type":"integer","minimum":0,"maximum":20000,"default":0},
                "limit":{"type":"integer","minimum":1,"maximum":1000,"default":200},
                "include_hidden":{"type":"boolean","default":false}
            },"required":[],"additionalProperties":false}),
        )
    }
    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            let arguments: Arguments = serde_json::from_str(&call.function.arguments)
                .map_err(|error| invalid(error.to_string()))?;
            if !(1..=8).contains(&arguments.depth)
                || !(1..=1000).contains(&arguments.limit)
                || arguments.offset > MAX_VISITED
                || arguments.path.trim().is_empty()
            {
                return Err(invalid(
                    "path must be nonempty; depth 1–8, limit 1–1000, offset 0–20000",
                ));
            }
            if cancellation.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            let workspace = self.workspace.clone();
            let token = cancellation.clone();
            let task = tokio::task::spawn_blocking(move || list(workspace, arguments, token));
            tokio::select! {
                _ = cancellation.cancelled() => Err(ToolError::Cancelled),
                result = task => result.map_err(failed)?,
            }
            .map(ToolOutput::text)
        })
    }
}
fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidArguments {
        tool: "list_files".into(),
        message: message.into(),
    }
}
fn failed(error: impl std::fmt::Display) -> ToolError {
    ToolError::ExecutionFailed(error.to_string())
}

fn list(
    workspace: Workspace,
    args: Arguments,
    token: CancellationToken,
) -> Result<String, ToolError> {
    let target_path = Path::new(&args.path);
    if target_path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(invalid("use a workspace-relative path without '..'"));
    }

    let cap = FilesystemCapability {
        environment: EnvironmentId::Local,
        path: target_path.to_path_buf(),
        access: FilesystemAccess::Read,
    };
    match workspace.policy().check_filesystem(&cap) {
        PolicyDecision::Granted => {}
        PolicyDecision::NeedsApproval(_) => {
            return Err(failed(format!(
                "permission denied: '{}' is outside the workspace. Call request_capability to request access.",
                args.path
            )));
        }
        PolicyDecision::Denied(reason) => {
            return Err(failed(format!("permission denied: {reason}")));
        }
    }

    let is_curdir = target_path
        .components()
        .all(|part| part == std::path::Component::CurDir);

    let (root, is_external) = if is_curdir {
        (PathBuf::new(), false)
    } else {
        match workspace.checked_relative(target_path) {
            Ok(rel) => (rel, false),
            Err(WorkspaceError::ProtectedPath) => {
                return Err(failed(WorkspaceError::ProtectedPath));
            }
            Err(_) => {
                let canonical = target_path.canonicalize().map_err(failed)?;
                (canonical, true)
            }
        }
    };

    if is_external {
        workspace.open_granted_directory(&root).map_err(failed)?;
    } else {
        workspace.open_directory(&root).map_err(failed)?;
    }

    let mut pending = VecDeque::from([(root, 1)]);
    let mut entries: Vec<Value> = Vec::new();
    let mut visited = 0;
    let mut eligible = 0;
    let mut skipped = 0;
    let mut bytes = 0;
    let mut more = false;
    let mut scan_truncated = false;
    'scan: while let Some((directory, level)) = pending.pop_front() {
        if token.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let names = if is_external {
            workspace
                .open_granted_directory_entries(&directory)
                .map_err(failed)?
        } else {
            workspace.directory_entries(&directory).map_err(failed)?
        };
        for name in names {
            if token.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            if visited >= MAX_VISITED {
                scan_truncated = true;
                break 'scan;
            }
            visited += 1;
            let Some(name_text) = name.to_str() else {
                skipped += 1;
                continue;
            };
            if !args.include_hidden && name_text.starts_with('.') {
                continue;
            }
            let path = directory.join(&name);
            let kind = if is_external {
                match workspace.open_granted_read(&path) {
                    Ok(_) => "file",
                    Err(WorkspaceError::NotFile) => match workspace.open_granted_directory(&path) {
                        Ok(_) => {
                            if level < args.depth
                                && !matches!(
                                    name_text,
                                    "target" | "node_modules" | ".venv" | "__pycache__"
                                )
                            {
                                pending.push_back((path.clone(), level + 1));
                            }
                            "directory"
                        }
                        Err(_) => {
                            skipped += 1;
                            continue;
                        }
                    },
                    Err(_) => {
                        skipped += 1;
                        continue;
                    }
                }
            } else {
                match workspace.open_read(&path) {
                    Ok(_) => "file",
                    Err(WorkspaceError::NotFile) => match workspace.open_directory(&path) {
                        Ok(_) => {
                            if level < args.depth
                                && !matches!(
                                    name_text,
                                    "target" | "node_modules" | ".venv" | "__pycache__"
                                )
                            {
                                pending.push_back((path.clone(), level + 1));
                            }
                            "directory"
                        }
                        Err(_) => {
                            skipped += 1;
                            continue;
                        }
                    },
                    Err(_) => {
                        skipped += 1;
                        continue;
                    }
                }
            };
            eligible += 1;
            if eligible <= args.offset {
                continue;
            }
            let entry = json!({"path":path.to_str().expect("UTF-8 path components"),"kind":kind});
            let size = entry.to_string().len() + 1;
            if entries.len() == args.limit || bytes + size > MAX_OUTPUT - 1024 {
                more = true;
                break 'scan;
            }
            bytes += size;
            entries.push(entry);
        }
    }
    let next_offset = if more {
        Some(args.offset + entries.len())
    } else {
        None
    };
    Ok(
        json!({"entries":entries,"next_offset":next_offset,"has_more":more,
        "scan_truncated":scan_truncated,"skipped_entries":skipped})
        .to_string(),
    )
}
