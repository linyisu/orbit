//! Explicit export from a session's sandbox scratch to private host artifacts.
use crate::run_command::ScratchDirectory;
use orbit_agent::tool::{Tool, ToolError, ToolFuture, ToolOutput};
use orbit_model::types::{ToolCall, ToolDefinition};
use orbit_runtime::filesystem::Workspace;
use serde::Deserialize;
use serde_json::json;
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

pub struct ExportArtifact {
    scratch: ScratchDirectory,
    directory: PathBuf,
}

impl ExportArtifact {
    pub fn new(scratch: ScratchDirectory, directory: PathBuf) -> Self {
        Self { scratch, directory }
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
            "Export a regular file from the current session's /scratch to Orbit's private host artifact directory for the user to open. Supports binary files. path is relative to /scratch; maximum 64 MiB per file, 128 files / 256 MiB per session. Does not overwrite project files or existing artifacts; symlinks and traversal are rejected. Save build output to the configured persistent /scratch directory, then export explicitly after the command finishes. Returns the actual host path, size and SHA-256.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "name": { "type": "string", "description": "Optional filename, without directories" }
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
            let directory = self.directory.clone();

            tokio::task::spawn_blocking(move || export(&scratch, &directory, args, token))
                .await
                .map_err(failed)?
        })
    }
}

fn failed(error: impl std::fmt::Display) -> ToolError {
    ToolError::ExecutionFailed(error.to_string())
}

fn export(
    scratch: &Path,
    parent: &Path,
    args: Arguments,
    token: CancellationToken,
) -> Result<ToolOutput, ToolError> {
    if token.is_cancelled() {
        return Err(ToolError::Cancelled);
    }

    let name = args
        .name
        .as_deref()
        .or_else(|| Path::new(&args.path).file_name().and_then(|s| s.to_str()))
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

    let source = Workspace::new(scratch).map_err(failed)?;
    let file = source.open_read(Path::new(&args.path)).map_err(failed)?;
    let mut bytes = Vec::new();
    file.take(64 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(failed)?;

    if bytes.len() > 64 * 1024 * 1024 {
        return Err(failed("artifact exceeds 64 MiB"));
    }
    if token.is_cancelled() {
        return Err(ToolError::Cancelled);
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

    let destination = parent.join(scratch.file_name().unwrap());
    let (count, total) = std::fs::read_dir(&destination)
        .map_err(failed)?
        .try_fold((0usize, 0u64), |(count, total), entry| {
            let metadata = entry?.metadata()?;
            Ok::<_, std::io::Error>((count + 1, total.saturating_add(metadata.len())))
        })
        .map_err(failed)?;

    if count >= 128 || total.saturating_add(bytes.len() as u64) > 256 * 1024 * 1024 {
        return Err(failed(
            "session artifact quota reached: 128 files / 256 MiB; existing artifacts were preserved",
        ));
    }

    let mut output = tempfile::Builder::new()
        .prefix("artifact-")
        .suffix(&format!("-{name}"))
        .tempfile_in(&destination)
        .map_err(failed)?;
    output.write_all(&bytes).map_err(failed)?;
    output.as_file().sync_all().map_err(failed)?;

    if token.is_cancelled() {
        return Err(ToolError::Cancelled);
    }

    let (_, path) = output.keep().map_err(failed)?;
    std::fs::File::open(destination)
        .and_then(|d| d.sync_all())
        .map_err(failed)?;

    Ok(ToolOutput::text(
        json!({ "path": path, "name": name, "bytes": bytes.len(), "sha256": orbit_runtime::execution::content_hash(&bytes) })
        .to_string(),
    ))
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
            &export(&scratch, &parent, args(), CancellationToken::new())
                .unwrap()
                .content,
        )
        .unwrap();
        let second: serde_json::Value = serde_json::from_str(
            &export(&scratch, &parent, args(), CancellationToken::new())
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
                &parent,
                Arguments {
                    path: "link".into(),
                    name: None
                },
                CancellationToken::new()
            )
            .is_err()
        );
    }
}
