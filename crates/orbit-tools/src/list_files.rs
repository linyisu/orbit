use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
};

use orbit_agent::tool::{Tool, ToolError, ToolFuture};
use orbit_model::types::{ToolCall, ToolDefinition};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::workspace::{Workspace, WorkspaceError};

const MAX_VISITED: usize = 20_000;
const MAX_OUTPUT: usize = 32 * 1024;
const DEFAULT_LIST_DEPTH: usize = 1;
const DEFAULT_LIST_OFFSET: usize = 0;
const DEFAULT_LIST_LIMIT: usize = 200;

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
            depth: DEFAULT_LIST_DEPTH,
            offset: DEFAULT_LIST_OFFSET,
            limit: DEFAULT_LIST_LIMIT,
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
            "Browse workspace files and directories without running a shell. Paths are relative to the workspace root. Defaults to one directory level; depth 1–8, deterministic breadth-first ordering, paginated with offset/next_offset. Hidden entries are omitted by default; include_hidden includes hidden entries but never protected paths. Generated directories are listed but not recursively traversed; select their path explicitly to browse them. Does not apply .gitignore. Symlinks and special files are omitted. Results are bounded to 32 KiB and 20000 examined entries; narrow path if scan_truncated is true.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "default": "." },
                    "depth": { "type": "integer", "minimum": 1, "maximum": 8, "default": DEFAULT_LIST_DEPTH },
                    "offset": { "type": "integer", "minimum": 0, "maximum": 20000, "default": DEFAULT_LIST_OFFSET },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 1000, "default": DEFAULT_LIST_LIMIT },
                    "include_hidden": { "type": "boolean", "default": false }
                },
                "required": [],
                "additionalProperties": false
            }),
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
    let root = if Path::new(&args.path)
        .components()
        .all(|part| part == std::path::Component::CurDir)
    {
        PathBuf::new()
    } else {
        workspace
            .checked_relative(Path::new(&args.path))
            .map_err(failed)?
    };

    workspace.open_directory(&root).map_err(failed)?;

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

        let names = workspace.directory_entries(&directory).map_err(failed)?;
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

            // Use the same descriptor-based opens as read_file, including protected
            // hard-link checks. Never inspect contents or follow directory symlinks.
            let kind = match workspace.open_read(&path) {
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
            };

            eligible += 1;
            if eligible <= args.offset {
                continue;
            }

            let entry = json!({
                "path": path.to_str().expect("UTF-8 path components"),
                "kind": kind,
            });
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

    Ok(json!({
        "entries": entries,
        "next_offset": next_offset,
        "has_more": more,
        "scan_truncated": scan_truncated,
        "skipped_entries": skipped,
    })
    .to_string())
}
