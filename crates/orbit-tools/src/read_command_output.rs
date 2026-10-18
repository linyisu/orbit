use std::{io::Read, path::PathBuf};

use orbit_agent::tool::{Tool, ToolError, ToolOutput};
use orbit_runtime::execution::{
    MAX_RETAINED_OUTPUT_PER_STREAM,
    output::{CommandOutputs, open_retained_file},
};
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::limits::MAX_TEXT_OUTPUT_BYTES;

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct Arguments {
    /// Opaque output_id from Exec.
    #[schemars(required)]
    output_id: String,
    stream: Stream,
    /// Zero-based byte offset.
    #[schemars(range(max = MAX_RETAINED_OUTPUT_PER_STREAM))]
    offset: u64,
    #[schemars(range(min = 1, max = MAX_TEXT_OUTPUT_BYTES))]
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

#[derive(Clone, Copy, serde::Serialize, Deserialize, schemars::JsonSchema)]
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

/// Read a bounded page from the owning session's retained command logs.
pub struct ReadCommandOutput {
    outputs: CommandOutputs,
}

impl ReadCommandOutput {
    pub fn new(outputs: CommandOutputs) -> Self {
        Self { outputs }
    }
}

impl Tool for ReadCommandOutput {
    fn capabilities(&self) -> orbit_agent::tool::ToolCapabilities {
        orbit_agent::tool::ToolCapabilities::READ
    }

    /// Read retained stdout or stderr using output_id from Exec. Logs belong to the
    /// current session and survive task completion and service restart. Use next_offset for
    /// UTF-8-safe continuation; has_more refers only to bytes already retained. capture_complete
    /// distinguishes complete capture from missing output; null means no final capture receipt
    /// is available yet. Storage is capped at 16 MiB per stream and 128 MiB per session; older
    /// logs may be evicted. Missing logs must not trigger automatic command re-execution.
    #[orbit_agent::tool(name = "CommandOutput")]
    async fn execute(
        &self,
        arguments: Arguments,
        cancellation: CancellationToken,
    ) -> Result<ToolOutput, ToolError> {
        validate(&arguments)?;

        if cancellation.is_cancelled() {
            return Err(ToolError::Cancelled);
        }

        let lease = self
            .outputs
            .lease()
            .ok_or_else(|| failed("command output is unavailable or expired in this session"))?;
        let token = cancellation.clone();
        let task =
            tokio::task::spawn_blocking(move || read(lease.path().to_owned(), arguments, token));

        tokio::select! {
            _ = cancellation.cancelled() => Err(ToolError::Cancelled),
            result = task => {
                result.map_err(|error| ToolError::ExecutionFailed(error.to_string()))?
            }
        }
        .map(ToolOutput::text)
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
            "output_id must be an Orbit command id; offset must be 0..16777216 and max_bytes \
                1..32768",
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
            "command output was not found in this session; it may have expired, been evicted \
                or was not retained. Do not automatically rerun the command"
                .into(),
        )
    })?;

    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(ToolError::ExecutionFailed(
            "command output file is invalid".into(),
        ));
    }

    let mut file = open_retained_file(&path).map_err(failed)?;
    let metadata = file.metadata().map_err(failed)?;
    if !metadata.is_file() {
        return Err(failed("command output file is invalid"));
    }
    let total_bytes = metadata.len();

    if arguments.offset > total_bytes {
        return Err(invalid(format!(
            "offset exceeds retained {} length ({total_bytes} bytes)",
            arguments.stream.suffix()
        )));
    }

    use std::io::Seek;

    file.seek(std::io::SeekFrom::Start(arguments.offset))
        .map_err(failed)?;
    let remaining = total_bytes.saturating_sub(arguments.offset);
    let mut bytes = vec![0; remaining.min(arguments.max_bytes.saturating_add(3) as u64) as usize];
    file.read_exact(&mut bytes)
        .map_err(failed)?;

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
    let receipt = capture_receipt(&directory.join(format!("{}.json", arguments.output_id)));

    Ok(json!({
        "output_id": arguments.output_id,
        "stream": arguments.stream.suffix(),
        "offset": arguments.offset,
        "next_offset": next_offset,
        "total_retained_bytes": total_bytes,
        "total_captured_bytes": receipt.as_ref().map(|receipt| match arguments.stream {
            Stream::Stdout => receipt.stdout_bytes,
            Stream::Stderr => receipt.stderr_bytes,
        }),
        "truncated": next_offset < total_bytes,
        "has_more": next_offset < total_bytes,
        "capture_complete": receipt.as_ref().map(|receipt| {
            !receipt.truncated && receipt.error.is_none()
        }),
        "capture_error": receipt.as_ref().and_then(|receipt| receipt.error.as_deref()),
        "content": String::from_utf8_lossy(&bytes)
    })
    .to_string())
}

fn capture_receipt(
    path: &std::path::Path,
) -> Option<orbit_runtime::execution::output::CaptureReceipt> {
    const MAX_RECEIPT_BYTES: u64 = 16 * 1_024;
    let file = open_retained_file(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(MAX_RECEIPT_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_RECEIPT_BYTES {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidArguments {
        tool: "CommandOutput".into(),
        message: message.into(),
    }
}

fn failed(error: impl std::fmt::Display) -> ToolError {
    ToolError::ExecutionFailed(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_model::types::ToolCall;

    #[tokio::test]
    async fn completed_commands_remain_readable_after_reopening_the_session_store() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("logs").join("session");
        let outputs = CommandOutputs::persistent(path.clone());
        let run = outputs.begin_run().unwrap();
        let (sender, mut approvals) = crate::approval::channel();
        let approver = tokio::spawn(async move {
            approvals
                .recv()
                .await
                .unwrap()
                .decision
                .send(true)
                .unwrap();
        });
        let command = crate::run_command::RunCommand::new(
            orbit_runtime::filesystem::Workspace::new(root.path()).unwrap(),
            sender,
        )
        .with_output_store(outputs);
        let output = command
            .execute(
                ToolCall::function(
                    "capture",
                    "Exec",
                    json!({
                        "command": "printf 'sample output'"
                    }),
                ),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        approver.await.unwrap();
        let result: serde_json::Value = serde_json::from_str(&output.content).unwrap();
        drop(command);
        drop(run);
        let reader = ReadCommandOutput::new(CommandOutputs::persistent(path));
        let page = reader
            .execute(
                ToolCall::function(
                    "reopen",
                    "CommandOutput",
                    json!({
                        "output_id": result["output_id"], "max_bytes": 6
                    }),
                ),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let page: serde_json::Value = serde_json::from_str(&page.content).unwrap();
        assert_eq!(page["content"], "sample");
        assert_eq!(page["has_more"], true);
        assert_eq!(page["capture_complete"], true);
        assert_eq!(page["total_captured_bytes"], 13);
    }

    #[tokio::test]
    async fn temporary_output_api_is_task_scoped_and_does_not_reopen_expired_ids() {
        let outputs = CommandOutputs::default();
        let tool = ReadCommandOutput::new(outputs.clone());
        let task = outputs.begin_run().unwrap();
        let log = outputs.create_log().await.unwrap();
        let id = log.id().to_owned();
        let path = outputs
            .lease()
            .unwrap()
            .path()
            .to_owned();
        std::fs::write(path.join(format!("{id}.stdout")), "甲乙丙").unwrap();
        drop(log);
        let call = |offset| {
            ToolCall::function(
                "page",
                "CommandOutput",
                json!({
                    "output_id": id,
                    "offset": offset,
                    "max_bytes": 4
                }),
            )
        };
        let first = tool
            .execute(call(0), CancellationToken::new())
            .await
            .unwrap();
        let first: serde_json::Value = serde_json::from_str(&first).unwrap();
        assert_eq!(first["content"], "甲");
        let second = tool
            .execute(
                call(first["next_offset"].as_u64().unwrap()),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let second: serde_json::Value = serde_json::from_str(&second).unwrap();
        assert_eq!(second["content"], "乙");
        assert_eq!(second["next_offset"], 6);
        let last = tool
            .execute(call(6), CancellationToken::new())
            .await
            .unwrap();
        let last: serde_json::Value = serde_json::from_str(&last).unwrap();
        assert_eq!(last["content"], "丙");
        assert_eq!(last["has_more"], false);
        let other = ReadCommandOutput::new(CommandOutputs::default());
        assert!(
            other
                .execute(call(0), CancellationToken::new())
                .await
                .is_err()
        );

        drop(task);
        assert!(!path.exists());
        let error = tool
            .execute(call(0), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("expired"));
        let _next = outputs.begin_run().unwrap();
        let _log = outputs.create_log().await.unwrap();
        assert!(
            tool.execute(call(0), CancellationToken::new())
                .await
                .is_err()
        );
    }

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
}
