use std::{io::Read, path::Path};

use orbit_agent::tool::{Tool, ToolError, ToolFuture, ToolOutput};
use orbit_model::types::{ToolCall, ToolDefinition};
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use orbit_runtime::filesystem::Workspace;

const MAX_FILE_BYTES: u64 = 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 32 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArguments {
    path: String,
    #[serde(default = "first_line")]
    start_line: usize,
    #[serde(default)]
    start_char: usize,
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
            "Read a UTF-8 file inside the workspace (or outside path granted via request_capability). Use a relative path or granted outside path, and optional 1-based start_line/max_lines. If a long line is truncated, resume with the returned next_start_line and next_start_char (Unicode character offset within that line). Returns JSON with numbered lines. Symlinks and protected configuration files are rejected. Maximum file size: 1 MiB; maximum content field: 32 KiB.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path relative to the workspace root, or an outside path granted via request_capability" },
                    "start_line": { "type": "integer", "minimum": 1, "default": 1 },
                    "start_char": { "type": "integer", "minimum": 0, "default": 0 },
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
    let file = workspace
        .authorize_read(Path::new(&arguments.path))
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
    if arguments.start_char
        > lines
            .get(arguments.start_line - 1)
            .map_or(0, |line| line.chars().count())
    {
        return Err(invalid("start_char exceeds the selected line length"));
    }

    let mut content = String::new();
    let mut end_line = arguments.start_line.saturating_sub(1);
    let mut continuation = None;

    for (index, line) in lines
        .iter()
        .enumerate()
        .skip(arguments.start_line - 1)
        .take(arguments.max_lines)
    {
        let skipped = if index + 1 == arguments.start_line {
            arguments.start_char
        } else {
            0
        };

        let line = line.chars().skip(skipped).collect::<String>();
        let prefix = format!("{}: ", index + 1);
        let available = MAX_OUTPUT_BYTES.saturating_sub(content.len());

        if prefix.len() + line.len() + 1 > available {
            if available <= prefix.len() {
                continuation = Some((index + 1, skipped));
                break;
            }

            let mut end = 0;
            let mut characters = 0;

            for character in line.chars() {
                if end + character.len_utf8() > available - prefix.len() {
                    break;
                }

                end += character.len_utf8();
                characters += 1;
            }

            content.push_str(&prefix);
            content.push_str(&line[..end]);
            end_line = index + 1;
            continuation = Some((index + 1, skipped + characters));
            break;
        }

        content.push_str(&prefix);
        content.push_str(&line);
        content.push('\n');
        end_line = index + 1;
    }

    let continuation =
        continuation.or_else(|| (end_line < lines.len()).then_some((end_line + 1, 0)));

    Ok(json!({
        "path": arguments.path,
        "start_line": arguments.start_line,
        "start_char": arguments.start_char,
        "end_line": end_line,
        "total_lines": lines.len(),
        "truncated": continuation.is_some(),
        "next_start_line": continuation.map(|(line, _)| line),
        "next_start_char": continuation.map(|(_, character)| character),
        "content": content
    })
    .to_string())
}
