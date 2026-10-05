//! Explicit export from a session's sandbox scratch or workspace to private host artifacts.
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

use orbit_agent::tool::{Tool, ToolError, ToolFuture, ToolOutput};
use orbit_model::types::{ToolCall, ToolDefinition};
use orbit_runtime::filesystem::Workspace;
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::run_command::ScratchDirectory;

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ArtifactLimits {
    pub max_file_bytes: u64,
    pub max_session_bytes: u64,
    pub max_files: usize,
}

impl Default for ArtifactLimits {
    fn default() -> Self {
        Self {
            max_file_bytes: 1024 * 1024 * 1024,
            max_session_bytes: 4 * 1024 * 1024 * 1024,
            max_files: 128,
        }
    }
}

pub struct ExportArtifact {
    scratch: ScratchDirectory,
    directory: PathBuf,
    workspace: Option<Workspace>,
    limits: ArtifactLimits,
}

impl ExportArtifact {
    pub fn new(scratch: ScratchDirectory, directory: PathBuf) -> Self {
        Self {
            scratch,
            directory,
            workspace: None,
            limits: ArtifactLimits::default(),
        }
    }

    pub fn with_limits(mut self, limits: ArtifactLimits) -> Self {
        self.limits = limits;
        self
    }

    pub fn with_workspace(mut self, workspace: Workspace) -> Self {
        self.workspace = Some(workspace);
        self
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    path: String,
    name: Option<String>,
}

impl Tool for ExportArtifact {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "export_artifact",
            "Export a regular file from the current session's /scratch or workspace to Orbit's private host artifact directory for the user to open. Supports binary files. path can be relative to /scratch (e.g. 'file.txt' or '/scratch/file.txt') or relative to the workspace root (e.g. 'dist/app.bin' or '/workspace/dist/app.bin'); configurable quotas (default 1 GiB per file, 128 files / 4 GiB per session). Does not overwrite project files or existing artifacts; internal symlinks are resolved safely; escaping links and traversal are rejected. Returns the actual host path, size and SHA-256.",
            json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file to export (e.g. '/scratch/demo.txt', 'demo.txt', or 'build/output.pdf')"
                    },
                    "name": {
                        "type": "string",
                        "description": "Optional filename for the exported artifact, without directories"
                    }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        )
    }

    fn execute<'a>(&'a self, call: ToolCall, token: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            let args: Arguments = serde_json::from_str(&call.function.arguments).map_err(failed)?;
            let scratch = self.scratch.read().expect("scratch lock poisoned").clone();
            let workspace = self.workspace.clone();
            let directory = self.directory.clone();
            let limits = self.limits.clone();

            tokio::task::spawn_blocking(move || {
                export_with_limits(
                    &scratch,
                    workspace.as_ref(),
                    &directory,
                    args,
                    token,
                    &limits,
                )
            })
            .await
            .map_err(failed)?
        })
    }
}

fn failed(error: impl std::fmt::Display) -> ToolError {
    ToolError::ExecutionFailed(error.to_string())
}

#[cfg(test)]
fn export(
    scratch: &Path,
    workspace: Option<&Workspace>,
    parent: &Path,
    args: Arguments,
    token: CancellationToken,
) -> Result<ToolOutput, ToolError> {
    export_with_limits(
        scratch,
        workspace,
        parent,
        args,
        token,
        &ArtifactLimits::default(),
    )
}

fn export_with_limits(
    scratch: &Path,
    workspace: Option<&Workspace>,
    parent: &Path,
    args: Arguments,
    token: CancellationToken,
    limits: &ArtifactLimits,
) -> Result<ToolOutput, ToolError> {
    if token.is_cancelled() {
        return Err(ToolError::Cancelled);
    }

    let raw_path = args.path.trim();
    if raw_path.is_empty() {
        return Err(failed("artifact path cannot be empty"));
    }

    if raw_path == "/scratch"
        || raw_path == "/scratch/"
        || raw_path == "scratch"
        || raw_path == "scratch/"
    {
        return Err(failed("path must point to a file, not /scratch directory"));
    }

    if raw_path == "/workspace"
        || raw_path == "/workspace/"
        || raw_path == "workspace"
        || raw_path == "workspace/"
    {
        return Err(failed(
            "path must point to a file, not /workspace directory",
        ));
    }

    let name = args
        .name
        .as_deref()
        .or_else(|| Path::new(raw_path).file_name().and_then(|s| s.to_str()))
        .ok_or_else(|| failed("artifact filename required"))?;

    if name.is_empty()
        || name.len() > 160
        || name == "."
        || name == ".."
        || name
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
    {
        return Err(failed("artifact name must be a filename up to 160 bytes"));
    }

    let scratch_ws = Workspace::new(scratch).map_err(failed)?;

    let mut file = if let Some(rel) = raw_path.strip_prefix("/scratch/") {
        scratch_ws
            .open_read(Path::new(rel))
            .map_err(|e| failed(format!("cannot open file '{rel}' in /scratch: {e}")))?
    } else if let Some(rel) = raw_path.strip_prefix("scratch/") {
        scratch_ws
            .open_read(Path::new(rel))
            .map_err(|e| failed(format!("cannot open file '{rel}' in /scratch: {e}")))?
    } else if let Some(rel) = raw_path.strip_prefix("/workspace/") {
        if let Some(ws) = workspace {
            ws.open_read(Path::new(rel))
                .map_err(|e| failed(format!("cannot open file '{rel}' in workspace: {e}")))?
        } else {
            return Err(failed(
                "workspace is not available for export in this environment",
            ));
        }
    } else if let Some(rel) = raw_path.strip_prefix("workspace/") {
        if let Some(ws) = workspace {
            ws.open_read(Path::new(rel))
                .map_err(|e| failed(format!("cannot open file '{rel}' in workspace: {e}")))?
        } else {
            return Err(failed(
                "workspace is not available for export in this environment",
            ));
        }
    } else {
        let rel_str = raw_path.strip_prefix('/').unwrap_or(raw_path);
        let rel = Path::new(rel_str);

        match scratch_ws.open_read(rel) {
            Ok(f) => f,
            Err(e_scratch) => {
                if let Some(ws) = workspace {
                    match ws.open_read(rel) {
                        Ok(f) => f,
                        Err(e_ws) => {
                            return Err(failed(format!(
                                "file '{}' not found in /scratch ({e_scratch}) or workspace ({e_ws})",
                                rel.display()
                            )));
                        }
                    }
                } else {
                    return Err(failed(format!(
                        "cannot open file '{}' in /scratch: {e_scratch}",
                        rel.display()
                    )));
                }
            }
        }
    };

    use std::os::unix::fs::MetadataExt;
    let before = file.metadata().map_err(failed)?;
    let size = before.len();
    if limits.max_file_bytes != 0 && size > limits.max_file_bytes {
        return Err(failed("artifact exceeds configured file quota"));
    }

    use std::os::unix::fs::DirBuilderExt;

    for directory in [
        parent.to_owned(),
        parent.join(
            scratch
                .file_name()
                .ok_or_else(|| failed("invalid session directory"))?,
        ),
    ] {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&directory)
            .map_err(failed)?;

        if !std::fs::symlink_metadata(&directory)
            .map_err(failed)?
            .is_dir()
        {
            return Err(failed("artifact storage must be a real directory"));
        }
    }

    let destination = parent.join(
        scratch
            .file_name()
            .ok_or_else(|| failed("artifact scratch directory must have a name"))?,
    );
    // A filesystem lock serializes quota checks and publication across processes.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(destination.join(".quota-lock"))
        .map_err(failed)?;

    acquire_quota_lock(&lock, &token, std::time::Duration::from_secs(10))?;
    let (count, total) = std::fs::read_dir(&destination)
        .map_err(failed)?
        .try_fold((0usize, 0u64), |(count, total), entry| {
            let entry = entry?;
            if entry.file_name() == ".quota-lock" {
                return Ok((count, total));
            }
            let metadata = entry.metadata()?;
            Ok::<_, std::io::Error>((count + 1, total.saturating_add(metadata.len())))
        })
        .map_err(failed)?;

    if (limits.max_files != 0 && count >= limits.max_files)
        || (limits.max_session_bytes != 0 && total.saturating_add(size) > limits.max_session_bytes)
    {
        return Err(failed(
            "session artifact quota reached; existing artifacts were preserved",
        ));
    }

    let mut output = tempfile::Builder::new()
        .prefix("artifact-")
        .suffix(&format!("-{name}"))
        .tempfile_in(&destination)
        .map_err(failed)?;
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    let mut copied = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        if token.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        let n = file.read(&mut buffer).map_err(failed)?;
        if n == 0 {
            break;
        }
        copied += n as u64;
        if (limits.max_file_bytes != 0 && copied > limits.max_file_bytes)
            || (limits.max_session_bytes != 0
                && total.saturating_add(copied) > limits.max_session_bytes)
        {
            return Err(failed("artifact grew beyond configured quota during copy"));
        }

        output.write_all(&buffer[..n]).map_err(failed)?;
        hash.update(&buffer[..n]);
    }

    let after = file.metadata().map_err(failed)?;
    if (
        before.len(),
        before.mtime(),
        before.mtime_nsec(),
        before.ctime(),
        before.ctime_nsec(),
    ) != (
        after.len(),
        after.mtime(),
        after.mtime_nsec(),
        after.ctime(),
        after.ctime_nsec(),
    ) {
        return Err(failed(
            "artifact changed during copy; retry after generation finishes",
        ));
    }

    output.as_file().sync_all().map_err(failed)?;

    if token.is_cancelled() {
        return Err(ToolError::Cancelled);
    }

    let (_, path) = output.keep().map_err(failed)?;
    std::fs::File::open(destination)
        .and_then(|d| d.sync_all())
        .map_err(failed)?;

    Ok(ToolOutput::text(
        json!({ "path": path, "name": name, "bytes": copied, "sha256": hash.finalize().iter().map(|byte| format!("{byte:02x}")).collect::<String>() })
        .to_string(),
    ))
}

fn acquire_quota_lock(
    lock: &std::fs::File,
    token: &CancellationToken,
    timeout: std::time::Duration,
) -> Result<(), ToolError> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if token.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        match lock.try_lock() {
            Ok(()) => return Ok(()),
            Err(std::fs::TryLockError::WouldBlock) => {
                if std::time::Instant::now() >= deadline {
                    return Err(failed(
                        "artifact quota lock timed out; retry when the other export finishes",
                    ));
                }

                std::thread::sleep(std::time::Duration::from_millis(10));
            }

            Err(error) => return Err(failed(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_export_is_explicit_unique_and_confined_to_session() {
        let root = tempfile::tempdir().unwrap();
        let scratch = root.path().join("session");
        std::fs::create_dir(&scratch).unwrap();
        std::fs::write(scratch.join("test.xlsx"), b"PK\0\xff").unwrap();
        let parent = root.path().join("artifacts");
        let args = || Arguments {
            path: "test.xlsx".into(),
            name: None,
        };
        let first: serde_json::Value = serde_json::from_str(
            &export(&scratch, None, &parent, args(), CancellationToken::new())
                .unwrap()
                .content,
        )
        .unwrap();
        let second: serde_json::Value = serde_json::from_str(
            &export(&scratch, None, &parent, args(), CancellationToken::new())
                .unwrap()
                .content,
        )
        .unwrap();

        assert_ne!(first["path"], second["path"]);

        let path = Path::new(first["path"].as_str().unwrap());

        assert!(path.starts_with(parent.join("session")));
        assert_eq!(std::fs::read(path).unwrap(), b"PK\0\xff");

        for path in ["../outside", "/etc/passwd"] {
            assert!(
                export(
                    &scratch,
                    None,
                    &parent,
                    Arguments {
                        path: path.into(),
                        name: None
                    },
                    CancellationToken::new()
                )
                .is_err()
            );
        }

        std::os::unix::fs::symlink("test.xlsx", scratch.join("link")).unwrap();

        assert!(
            export(
                &scratch,
                None,
                &parent,
                Arguments {
                    path: "link".into(),
                    name: None
                },
                CancellationToken::new()
            )
            .is_ok()
        );
    }

    #[test]
    fn export_supports_scratch_prefix_and_workspace_files() {
        let root = tempfile::tempdir().unwrap();
        let scratch = root.path().join("session");
        std::fs::create_dir(&scratch).unwrap();
        std::fs::write(scratch.join("demo-artifact.txt"), b"scratch content").unwrap();

        let ws_dir = root.path().join("workspace");
        std::fs::create_dir_all(ws_dir.join("demo")).unwrap();
        std::fs::write(ws_dir.join("demo/demo-artifact.txt"), b"workspace content").unwrap();
        let ws = Workspace::new(&ws_dir).unwrap();

        let parent = root.path().join("artifacts");

        let res1: serde_json::Value = serde_json::from_str(
            &export(
                &scratch,
                Some(&ws),
                &parent,
                Arguments {
                    path: "/scratch/demo-artifact.txt".into(),
                    name: None,
                },
                CancellationToken::new(),
            )
            .unwrap()
            .content,
        )
        .unwrap();

        assert_eq!(res1["name"], "demo-artifact.txt");
        assert_eq!(
            std::fs::read(res1["path"].as_str().unwrap()).unwrap(),
            b"scratch content"
        );

        let res2: serde_json::Value = serde_json::from_str(
            &export(
                &scratch,
                Some(&ws),
                &parent,
                Arguments {
                    path: "scratch/demo-artifact.txt".into(),
                    name: None,
                },
                CancellationToken::new(),
            )
            .unwrap()
            .content,
        )
        .unwrap();

        assert_eq!(
            std::fs::read(res2["path"].as_str().unwrap()).unwrap(),
            b"scratch content"
        );

        let res3: serde_json::Value = serde_json::from_str(
            &export(
                &scratch,
                Some(&ws),
                &parent,
                Arguments {
                    path: "demo/demo-artifact.txt".into(),
                    name: None,
                },
                CancellationToken::new(),
            )
            .unwrap()
            .content,
        )
        .unwrap();

        assert_eq!(res3["name"], "demo-artifact.txt");
        assert_eq!(
            std::fs::read(res3["path"].as_str().unwrap()).unwrap(),
            b"workspace content"
        );

        let res4: serde_json::Value = serde_json::from_str(
            &export(
                &scratch,
                Some(&ws),
                &parent,
                Arguments {
                    path: "/workspace/demo/demo-artifact.txt".into(),
                    name: None,
                },
                CancellationToken::new(),
            )
            .unwrap()
            .content,
        )
        .unwrap();

        assert_eq!(
            std::fs::read(res4["path"].as_str().unwrap()).unwrap(),
            b"workspace content"
        );
    }

    #[test]
    fn quota_lock_wait_has_a_deadline_and_remains_cancellable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".quota-lock");
        let holder = std::fs::File::create(&path).unwrap();
        holder.lock().unwrap();
        let contender = std::fs::File::open(&path).unwrap();
        let token = CancellationToken::new();
        let error = acquire_quota_lock(&contender, &token, std::time::Duration::from_millis(20))
            .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        token.cancel();
        assert_eq!(
            acquire_quota_lock(&contender, &token, std::time::Duration::from_secs(10)),
            Err(ToolError::Cancelled)
        );
        drop(holder);
        acquire_quota_lock(
            &contender,
            &CancellationToken::new(),
            std::time::Duration::from_secs(1),
        )
        .unwrap();
    }

    #[test]
    fn streams_large_artifacts_and_cleans_up_quota_failures() {
        let root = tempfile::tempdir().unwrap();
        let scratch = root.path().join("session");
        std::fs::create_dir(&scratch).unwrap();
        let file = std::fs::File::create(scratch.join("large.bin")).unwrap();
        file.set_len(65 * 1024 * 1024).unwrap();
        let parent = root.path().join("artifacts");
        let args = || Arguments {
            path: "large.bin".into(),
            name: None,
        };
        let result = export(&scratch, None, &parent, args(), CancellationToken::new()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&result.content).unwrap();
        assert_eq!(value["bytes"], 65 * 1024 * 1024);
        let limits = ArtifactLimits {
            max_file_bytes: 70 * 1024 * 1024,
            max_session_bytes: 66 * 1024 * 1024,
            max_files: 128,
        };

        assert!(
            export_with_limits(
                &scratch,
                None,
                &parent,
                args(),
                CancellationToken::new(),
                &limits
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read_dir(parent.join("session")).unwrap().count(),
            2
        ); // artifact + quota lock
    }
}
