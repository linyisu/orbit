//! Read applicable repository instructions without creating workspace files.

use std::{
    io::Read,
    path::{Path, PathBuf},
};

use orbit_agent::tool::{Tool, ToolError, ToolFuture};
use orbit_model::types::{ToolCall, ToolDefinition};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::Workspace;

const MAX_TOTAL_BYTES: usize = 32 * 1024;
const CANDIDATES: [&str; 2] = ["AGENTS.override.md", "AGENTS.md"];

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct InstructionFile {
    pub path: String,
    pub content: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct InstructionSet {
    pub files: Vec<InstructionFile>,
}

impl InstructionSet {
    pub fn prompt(&self) -> Option<String> {
        if self.files.is_empty() {
            return None;
        }
        let mut content = String::from(
            "User and repository conventions. Apply only where relevant to the current task and path; direct user requests and Orbit's tool safety rules take precedence. Later files are more specific.\n\n",
        );
        for file in &self.files {
            content.push_str(&format!("--- {} ---\n{}\n\n", file.path, file.content));
        }
        Some(content)
    }
}

/// Returns rules from `workspace` down to the directory containing `target`.
/// The target may be a file that has not been created yet.
pub fn load_for_target(workspace: &Workspace, target: &Path) -> Result<InstructionSet, ToolError> {
    let target = workspace
        .checked_relative(target)
        .map_err(|error| ToolError::ExecutionFailed(error.to_string()))?;
    let mut directories = vec![PathBuf::new()];
    let mut current = PathBuf::new();
    for component in target.parent().into_iter().flat_map(Path::components) {
        current.push(component.as_os_str());
        directories.push(current.clone());
    }

    let mut result = InstructionSet::default();
    let mut total_bytes = 0usize;
    for directory in directories {
        for name in CANDIDATES {
            let candidate = directory.join(name);
            let mut file = match workspace.open_read(&candidate) {
                Ok(file) => file,
                Err(crate::WorkspaceError::Io(error))
                    if error.kind() == std::io::ErrorKind::NotFound =>
                {
                    continue;
                }
                Err(error) => {
                    return Err(ToolError::ExecutionFailed(format!(
                        "cannot read instruction file '{}': {error}",
                        candidate.display()
                    )));
                }
            };
            let mut bytes = Vec::new();
            file.by_ref()
                .take((MAX_TOTAL_BYTES - total_bytes + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|error| ToolError::ExecutionFailed(error.to_string()))?;
            if bytes.is_empty() {
                continue;
            }
            if total_bytes + bytes.len() > MAX_TOTAL_BYTES {
                return Err(ToolError::ExecutionFailed(format!(
                    "project instructions exceed {MAX_TOTAL_BYTES} bytes at '{}'",
                    candidate.display()
                )));
            }
            let content = String::from_utf8(bytes).map_err(|error| {
                ToolError::ExecutionFailed(format!(
                    "instruction file '{}' is not UTF-8: {error}",
                    candidate.display()
                ))
            })?;
            total_bytes += content.len();
            result.files.push(InstructionFile {
                path: candidate.to_string_lossy().into_owned(),
                content,
            });
            break;
        }
    }
    Ok(result)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    path: String,
}

pub struct ProjectInstructions {
    workspace: Workspace,
}

impl ProjectInstructions {
    pub fn new(workspace: Workspace) -> Self {
        Self { workspace }
    }
}

impl Tool for ProjectInstructions {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "project_instructions",
            "Read the AGENTS.md or AGENTS.override.md rules that apply to a workspace-relative target file. Call before editing a file in a directory whose instructions have not been read. More specific directories override or extend root rules. Does not follow symlinks or read protected paths.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Workspace-relative target file path, including files not created yet" }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        )
    }

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            let args: Arguments =
                serde_json::from_str(&call.function.arguments).map_err(|error| {
                    ToolError::InvalidArguments {
                        tool: "project_instructions".to_owned(),
                        message: error.to_string(),
                    }
                })?;
            if cancellation.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            let rules = load_for_target(&self.workspace, Path::new(&args.path))?;
            serde_json::to_string(&rules)
                .map_err(|error| ToolError::ExecutionFailed(error.to_string()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_rules_are_scoped_to_the_target_and_override_siblings() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src/parser")).unwrap();
        std::fs::create_dir_all(root.path().join("src/ui")).unwrap();
        std::fs::write(root.path().join("AGENTS.md"), "root rules\n").unwrap();
        std::fs::write(root.path().join("src/AGENTS.md"), "shared source rules\n").unwrap();
        std::fs::write(
            root.path().join("src/parser/AGENTS.md"),
            "old parser rules\n",
        )
        .unwrap();
        std::fs::write(
            root.path().join("src/parser/AGENTS.override.md"),
            "parser override\n",
        )
        .unwrap();
        std::fs::write(root.path().join("src/ui/AGENTS.md"), "UI only\n").unwrap();
        let workspace = Workspace::new(root.path()).unwrap();

        let parser = load_for_target(&workspace, Path::new("src/parser/new.rs")).unwrap();
        let paths = parser
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>();

        assert_eq!(
            paths,
            [
                "AGENTS.md",
                "src/AGENTS.md",
                "src/parser/AGENTS.override.md"
            ]
        );
        let prompt = parser.prompt().unwrap();
        assert!(prompt.contains("parser override"));
        assert!(!prompt.contains("old parser rules"));
        assert!(!prompt.contains("UI only"));
    }

    #[test]
    fn rejects_escape_symlink_and_protected_instruction_files() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("src")).unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("AGENTS.md"), "outside secret").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("AGENTS.md"),
            root.path().join("src/AGENTS.md"),
        )
        .unwrap();
        let workspace = Workspace::new(root.path()).unwrap();

        assert!(load_for_target(&workspace, Path::new("../escape.rs")).is_err());
        assert!(load_for_target(&workspace, Path::new("src/main.rs")).is_err());

        std::fs::remove_file(root.path().join("src/AGENTS.md")).unwrap();
        let secret = root.path().join("secret.txt");
        std::fs::write(&secret, "protected secret").unwrap();
        std::fs::hard_link(&secret, root.path().join("src/AGENTS.md")).unwrap();
        let mut workspace = Workspace::new(root.path()).unwrap();
        workspace.protect(&secret).unwrap();
        assert!(load_for_target(&workspace, Path::new("src/main.rs")).is_err());
    }

    #[test]
    fn oversized_instructions_fail_without_truncating_rules() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("AGENTS.md"),
            "x".repeat(MAX_TOTAL_BYTES + 1),
        )
        .unwrap();
        let workspace = Workspace::new(root.path()).unwrap();

        let error = load_for_target(&workspace, Path::new("main.rs")).unwrap_err();

        assert!(error.to_string().contains("exceed"), "{error}");
    }
}
