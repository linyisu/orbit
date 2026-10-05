//! Export a complete worktree patch without granting commands access to Git metadata.

use std::{io::Write, path::PathBuf};

use orbit_agent::tool::{Tool, ToolError, ToolFuture, ToolOutput};
use orbit_model::types::{ToolCall, ToolDefinition};
use orbit_runtime::filesystem::Workspace;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::git_inspect::working_tree_patch;

pub struct GitExportPatch {
    workspace: Workspace,
    artifact_directory: PathBuf,
}

impl GitExportPatch {
    pub fn new(workspace: Workspace, artifact_directory: PathBuf) -> Self {
        Self {
            workspace,
            artifact_directory,
        }
    }
}

impl Tool for GitExportPatch {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "git_export_patch",
            "Export the complete uncommitted Git worktree patch, including new files, to an Orbit-private artifact for the user to apply or commit. Does not stage, commit or write .git. Requires an existing repository with a HEAD commit at the workspace root; for a new non-Git project, write generated files to /scratch and use export_artifact instead. Returns the host path and SHA-256. The patch is limited to 4 MiB.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        )
    }

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            let arguments: serde_json::Value = serde_json::from_str(&call.function.arguments)
                .map_err(|error| ToolError::InvalidArguments {
                    tool: "git_export_patch".into(),
                    message: error.to_string(),
                })?;

            if arguments != json!({}) {
                return Err(ToolError::InvalidArguments {
                    tool: "git_export_patch".into(),
                    message: "this tool takes no arguments".into(),
                });
            }

            let workspace = self.workspace.root().to_path_buf();
            let artifact_directory = self.artifact_directory.clone();
            let token = cancellation.clone();
            let task = tokio::task::spawn_blocking(move || {
                if token.is_cancelled() {
                    return Err(ToolError::Cancelled);
                }

                let patch = working_tree_patch(&workspace)?;

                if token.is_cancelled() {
                    return Err(ToolError::Cancelled);
                }
                if patch.is_empty() {
                    return Err(ToolError::ExecutionFailed(
                        "worktree has no patch to export".into(),
                    ));
                }

                use std::os::unix::fs::DirBuilderExt;

                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(&artifact_directory)
                    .map_err(|error| ToolError::ExecutionFailed(error.to_string()))?;

                if !std::fs::symlink_metadata(&artifact_directory)
                    .map_err(|error| ToolError::ExecutionFailed(error.to_string()))?
                    .is_dir()
                {
                    return Err(ToolError::ExecutionFailed(
                        "artifact storage must be a real directory".into(),
                    ));
                }

                let mut file = tempfile::Builder::new()
                    .prefix("orbit-patch-")
                    .suffix(".patch")
                    .tempfile_in(&artifact_directory)
                    .map_err(|error| ToolError::ExecutionFailed(error.to_string()))?;

                file.write_all(patch.as_bytes())
                    .and_then(|_| file.as_file().sync_all())
                    .map_err(|error| ToolError::ExecutionFailed(error.to_string()))?;

                if token.is_cancelled() {
                    return Err(ToolError::Cancelled);
                }

                let (_, path) = file
                    .keep()
                    .map_err(|error| ToolError::ExecutionFailed(error.to_string()))?;

                Ok::<_, ToolError>(ToolOutput::text(
                    json!({ "path": path, "bytes": patch.len(), "sha256": orbit_runtime::execution::content_hash(patch.as_bytes()) }).to_string(),
                ))
            });
            tokio::select! {
                _ = cancellation.cancelled() => Err(ToolError::Cancelled),
                result = task => result.map_err(|error| ToolError::ExecutionFailed(error.to_string()))?,
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, process::Command};

    use super::*;

    #[tokio::test]
    async fn exports_complete_patch_without_changing_git_state() {
        let root = tempfile::tempdir().unwrap();
        let artifacts = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .arg("-C")
                .arg(root.path())
                .args(args)
                .status()
                .unwrap();

            assert!(status.success());
        };

        git(&["init", "-q"]);
        git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "baseline",
        ]);
        fs::write(root.path().join("new.txt"), "new content\n").unwrap();
        let tool = GitExportPatch::new(
            Workspace::new(root.path()).unwrap(),
            artifacts.path().to_path_buf(),
        );
        let result = tool
            .execute(
                ToolCall::function("patch", "git_export_patch", json!({})),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let value: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        let patch = fs::read_to_string(value["path"].as_str().unwrap()).unwrap();

        assert!(patch.contains("new content"));
        assert_eq!(value["bytes"], patch.len());
        assert!(root.path().join(".git").is_dir());
        assert!(!root.path().join("new.txt").is_symlink());
    }
}
