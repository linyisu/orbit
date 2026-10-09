use std::{
    io::Read,
    path::PathBuf,
    sync::{Arc, RwLock},
};

use orbit_agent::tool::{Tool, ToolError, ToolFuture, ToolOutput};
use orbit_model::types::{ToolCall, ToolDefinition};
use orbit_runtime::execution::MAX_RETAINED_OUTPUT_PER_STREAM;
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

pub type CommandOutputDirectory = Arc<RwLock<PathBuf>>;

use crate::limits::MAX_TEXT_OUTPUT_BYTES;

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Arguments {
    output_id: String,
    stream: Stream,
    offset: u64,
    max_bytes: usize,
}

impl Default for Arguments {
    fn default() -> Self {
        Self {
            output_id: String::new(),
            stream: Stream::Stdout,
            offset: 0,
            max_bytes: MAX_TEXT_OUTPUT_BYTES,
        }
    }
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Stream {
    Stdout,
    Stderr,
}

impl Stream {
    fn suffix(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

/// Read a bounded page of a prior run_command invocation's host-side log.
/// The directory is session-scoped and changes with /new and /resume.
pub struct ReadCommandOutput {
    directory: CommandOutputDirectory,
}

impl ReadCommandOutput {
    pub fn new(directory: CommandOutputDirectory) -> Self {
        Self { directory }
    }
}

impl Tool for ReadCommandOutput {
    fn capabilities(&self) -> orbit_agent::tool::ToolCapabilities {
        orbit_agent::tool::ToolCapabilities::READ
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "read_command_output",
            "Read one page of a previous run_command stdout or stderr log. run_command returns an output_id when retained output is available. offset and max_bytes are byte counts; use the returned next_offset to continue across UTF-8 boundaries. Use them to inspect the beginning, middle, or later portions without placing an entire build log in context. Logs are private to the current Orbit session, retained outside the project, capped at 16 MiB per stream and 128 MiB per session, and may be evicted when newer command logs need space.",
            json!({
                "type": "object",
                "properties": {
                    "output_id": { "type": "string", "description": "Opaque output_id from run_command" },
                    "stream": { "type": "string", "enum": ["stdout", "stderr"], "default": "stdout" },
                    "offset": {
                        "type": "integer",
                        "minimum": 0,
                        "maximum": MAX_RETAINED_OUTPUT_PER_STREAM,
                        "default": 0,
                        "description": "Zero-based byte offset"
                    },
                    "max_bytes": { "type": "integer", "minimum": 1, "maximum": MAX_TEXT_OUTPUT_BYTES, "default": MAX_TEXT_OUTPUT_BYTES }
                },
                "required": ["output_id"],
                "additionalProperties": false
            }),
        )
    }

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            let arguments: Arguments = serde_json::from_str(&call.function.arguments)
                .map_err(|error| invalid(error.to_string()))?;
            validate(&arguments)?;

            if cancellation.is_cancelled() {
                return Err(ToolError::Cancelled);
            }

            let directory = self
                .directory
                .read()
                .expect("command output lock poisoned")
                .clone();
            let token = cancellation.clone();
            let task = tokio::task::spawn_blocking(move || read(directory, arguments, token));

            tokio::select! {
                _ = cancellation.cancelled() => Err(ToolError::Cancelled),
                result = task => result.map_err(|error| ToolError::ExecutionFailed(error.to_string()))?,
            }
            .map(ToolOutput::text)
        })
    }
}

fn validate(arguments: &Arguments) -> Result<(), ToolError> {
    if arguments.output_id.len() > 160
        || !arguments.output_id.starts_with("cmd-")
        || !arguments
            .output_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        || arguments.offset > MAX_RETAINED_OUTPUT_PER_STREAM
        || !(1..=MAX_TEXT_OUTPUT_BYTES).contains(&arguments.max_bytes)
    {
        return Err(invalid(
            "output_id must be an Orbit command id; offset must be 0..16777216 and max_bytes 1..32768",
        ));
    }

    Ok(())
}

fn read(
    directory: PathBuf,
    arguments: Arguments,
    cancellation: CancellationToken,
) -> Result<String, ToolError> {
    let metadata = std::fs::symlink_metadata(&directory).map_err(|_| {
        ToolError::ExecutionFailed("command output is unavailable for this session".into())
    })?;

    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(ToolError::ExecutionFailed(
            "command output directory is invalid".into(),
        ));
    }

    let name = format!("{}.{}", arguments.output_id, arguments.stream.suffix());
    let path = directory.join(name);
    let metadata = std::fs::symlink_metadata(&path).map_err(|_| {
        ToolError::ExecutionFailed(
            "command output was not found; it may have been evicted or was not retained".into(),
        )
    })?;

    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(ToolError::ExecutionFailed(
            "command output file is invalid".into(),
        ));
    }

    let total_bytes = metadata.len();

    if arguments.offset > total_bytes {
        return Err(invalid(format!(
            "offset exceeds retained {} length ({total_bytes} bytes)",
            arguments.stream.suffix()
        )));
    }

    let mut file = std::fs::File::open(path).map_err(failed)?;

    use std::io::Seek;

    file.seek(std::io::SeekFrom::Start(arguments.offset))
        .map_err(failed)?;
    let remaining = total_bytes.saturating_sub(arguments.offset);
    let mut bytes = vec![0; remaining.min(arguments.max_bytes.saturating_add(3) as u64) as usize];
    file.read_exact(&mut bytes).map_err(failed)?;

    if cancellation.is_cancelled() {
        return Err(ToolError::Cancelled);
    }

    let desired = bytes.len().min(arguments.max_bytes);
    let valid_end = match std::str::from_utf8(&bytes) {
        Ok(_) => bytes.len(),
        Err(error) if error.error_len().is_none() && error.valid_up_to() >= desired => {
            error.valid_up_to()
        }
        Err(_) => 0,
    };

    let page_len = match std::str::from_utf8(&bytes[..valid_end]) {
        Ok(text) if desired < valid_end => {
            let mut end = desired;

            while end > 0 && !text.is_char_boundary(end) {
                end -= 1;
            }

            if end == 0 {
                end = desired;

                while end < valid_end && !text.is_char_boundary(end) {
                    end += 1;
                }
            }

            end
        }

        _ => desired,
    };

    bytes.truncate(page_len);
    let next_offset = arguments.offset + bytes.len() as u64;

    Ok(json!({
        "output_id": arguments.output_id,
        "stream": arguments.stream.suffix(),
        "offset": arguments.offset,
        "next_offset": next_offset,
        "total_retained_bytes": total_bytes,
        "truncated": next_offset < total_bytes,
        "content": String::from_utf8_lossy(&bytes)
    })
    .to_string())
}

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidArguments {
        tool: "read_command_output".into(),
        message: message.into(),
    }
}

fn failed(error: impl std::fmt::Display) -> ToolError {
    ToolError::ExecutionFailed(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_traversal_and_accepts_opaque_ids() {
        assert!(
            validate(&Arguments {
                output_id: "cmd-123-456-0".into(),
                ..Arguments::default()
            })
            .is_ok()
        );
        assert!(
            validate(&Arguments {
                output_id: "../cmd-123".into(),
                ..Arguments::default()
            })
            .is_err()
        );
    }

    #[test]
    fn reads_a_bounded_page() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("cmd-1-2-3.stdout"), "abcdef").unwrap();
        let content = read(
            tmp.path().to_owned(),
            Arguments {
                output_id: "cmd-1-2-3".into(),
                max_bytes: 3,
                ..Arguments::default()
            },
            CancellationToken::new(),
        )
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&content).unwrap();

        assert_eq!(value["content"], "abc");
        assert_eq!(value["next_offset"], 3);
        assert_eq!(value["total_retained_bytes"], 6);
        assert_eq!(value["truncated"], true);
    }

    #[test]
    fn unicode_pages_resume_on_character_boundaries() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("cmd-1-2-3.stdout"), "甲乙丙").unwrap();
        let first: serde_json::Value = serde_json::from_str(
            &read(
                tmp.path().to_owned(),
                Arguments {
                    output_id: "cmd-1-2-3".into(),
                    max_bytes: 4,
                    ..Arguments::default()
                },
                CancellationToken::new(),
            )
            .unwrap(),
        )
        .unwrap();
        let second: serde_json::Value = serde_json::from_str(
            &read(
                tmp.path().to_owned(),
                Arguments {
                    output_id: "cmd-1-2-3".into(),
                    offset: first["next_offset"].as_u64().unwrap(),
                    max_bytes: 4,
                    ..Arguments::default()
                },
                CancellationToken::new(),
            )
            .unwrap(),
        )
        .unwrap();

        assert_eq!(first["content"], "甲");
        assert_eq!(second["content"], "乙");
        assert_eq!(second["next_offset"], 6);
    }
}
