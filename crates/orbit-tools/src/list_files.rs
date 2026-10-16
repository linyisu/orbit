use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
};

use orbit_agent::tool::{Tool, ToolError, ToolOutput};
use orbit_runtime::filesystem::{Workspace, WorkspaceError};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::limits::{MAX_DIRECTORY_ENTRIES, MAX_TEXT_OUTPUT_BYTES, OUTPUT_METADATA_RESERVE_BYTES};

const DEFAULT_PAGE_ENTRIES: usize = 200;
const MAX_PAGE_ENTRIES: usize = 1_000;
const MAX_DEPTH: usize = 8;

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct Arguments {
    /// Directory path, relative to the workspace or explicitly granted outside it.
    path: String,
    #[schemars(range(min = 1, max = MAX_DEPTH))]
    depth: usize,
    #[schemars(range(max = MAX_DIRECTORY_ENTRIES))]
    offset: usize,
    #[schemars(range(min = 1, max = MAX_PAGE_ENTRIES))]
    limit: usize,
    include_hidden: bool,
    /// Optional path glob, e.g. **/*.rs. Directories are traversed even when they do not match.
    glob: Option<String>,
}

impl Default for Arguments {
    fn default() -> Self {
        Self {
            path: ".".into(),
            depth: 1,
            offset: 0,
            limit: DEFAULT_PAGE_ENTRIES,
            include_hidden: false,
            glob: None,
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
    fn capabilities(&self) -> orbit_agent::tool::ToolCapabilities {
        orbit_agent::tool::ToolCapabilities::READ
    }

    /// Browse files and directories in the workspace (or outside paths granted via
    /// RequestAccess) without running a shell. Paths are relative to the workspace root,
    /// or absolute paths granted via RequestAccess. Defaults to one directory level; depth
    /// 1–8, deterministic breadth-first ordering, paginated with offset/next_offset. Hidden
    /// entries are omitted by default; include_hidden includes hidden entries but never
    /// protected paths. Generated directories are listed but not recursively traversed; select
    /// their path explicitly to browse them. Does not apply .gitignore. Symlinks and special
    /// files are omitted. Results are bounded to 32 KiB and 20000 examined entries; narrow path
    /// if scan_truncated is true.
    #[orbit_agent::tool(name = "ListFiles")]
    async fn execute(
        &self,
        arguments: Arguments,
        cancellation: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        if !(1..=MAX_DEPTH).contains(&arguments.depth)
            || !(1..=MAX_PAGE_ENTRIES).contains(&arguments.limit)
            || arguments.offset > MAX_DIRECTORY_ENTRIES
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
    }
}

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidArguments {
        tool: "ListFiles".into(),
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
    let glob = args
        .glob
        .as_deref()
        .map(|pattern| {
            globset::Glob::new(pattern)
                .map(|glob| glob.compile_matcher())
                .map_err(|error| invalid(error.to_string()))
        })
        .transpose()?;
    let target_path = Path::new(&args.path);

    if target_path
        .components()
        .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(invalid("use a workspace-relative path without '..'"));
    }

    let is_curdir = target_path
        .components()
        .all(|part| part == std::path::Component::CurDir);

    let (root, is_external) = if is_curdir {
        workspace
            .open_directory(Path::new(""))
            .map_err(failed)?;
        (PathBuf::new(), false)
    } else {
        match workspace.checked_relative(target_path) {
            Ok(rel) => {
                workspace
                    .open_directory(&rel)
                    .map_err(failed)?;
                (rel, false)
            }

            Err(WorkspaceError::ProtectedPath) => {
                return Err(failed(WorkspaceError::ProtectedPath));
            }
            Err(_) => {
                let canonical = target_path
                    .canonicalize()
                    .map_err(failed)?;
                workspace
                    .authorize_directory(&canonical)
                    .map_err(failed)?;
                (canonical, true)
            }
        }
    };

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
                .authorize_directory_entries(&directory)
                .map_err(failed)?
        } else {
            workspace
                .directory_entries(&directory)
                .map_err(failed)?
        };

        for name in names {
            if token.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            if visited >= MAX_DIRECTORY_ENTRIES {
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
                match workspace.authorize_read(&path) {
                    Ok(_) => "file",
                    Err(WorkspaceError::NotFile) => match workspace.authorize_directory(&path) {
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

            if glob
                .as_ref()
                .is_some_and(|glob| !glob.is_match(&path))
            {
                continue;
            }
            eligible += 1;

            if eligible <= args.offset {
                continue;
            }

            let Some(path) = path.to_str() else {
                skipped += 1;
                continue;
            };

            let entry = json!({
                "path": path,
                "kind": kind
            });
            let size = entry.to_string().len() + 1;

            if entries.len() == args.limit
                || bytes + size > MAX_TEXT_OUTPUT_BYTES - OUTPUT_METADATA_RESERVE_BYTES
            {
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
        "skipped_entries": skipped
    })
    .to_string())
}
