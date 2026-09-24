use std::{io::Read, path::Path};

use orbit_agent::tool::{Tool, ToolError, ToolFuture};
use orbit_model::types::{ToolCall, ToolDefinition};
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::workspace::Workspace;

const MAX_FILE_BYTES: u64 = 32 * 1024 * 1024;
const DEFAULT_CHARS: usize = 20_000;
const MAX_CHARS: usize = 60_000;

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Arguments {
    path: String,
    start_char: usize,
    max_chars: usize,
}

impl Default for Arguments {
    fn default() -> Self {
        Self {
            path: String::new(),
            start_char: 0,
            max_chars: DEFAULT_CHARS,
        }
    }
}

pub struct ReadDocument {
    workspace: Workspace,
}

impl ReadDocument {
    pub fn new(workspace: Workspace) -> Self {
        Self { workspace }
    }
}

impl Tool for ReadDocument {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "read_document",
            "Convert a document in the workspace to Markdown and read a Unicode-character page. Supports PDF, Word, PowerPoint, Excel, OpenDocument, RTF, EPUB and CSV. Use read_file for ordinary text and source code. Continue at next_start_char when truncated; image-only PDFs need OCR.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path relative to the workspace root" },
                    "start_char": { "type": "integer", "minimum": 0, "default": 0 },
                    "max_chars": { "type": "integer", "minimum": 1, "maximum": MAX_CHARS, "default": DEFAULT_CHARS }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        )
    }

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            let arguments: Arguments = serde_json::from_str(&call.function.arguments)
                .map_err(|error| invalid(error.to_string()))?;
            if arguments.path.trim().is_empty() || !(1..=MAX_CHARS).contains(&arguments.max_chars) {
                return Err(invalid(format!(
                    "path must be nonempty and max_chars must be between 1 and {MAX_CHARS}"
                )));
            }
            if cancellation.is_cancelled() {
                return Err(ToolError::Cancelled);
            }

            let workspace = self.workspace.clone();
            let token = cancellation.clone();
            let task = tokio::task::spawn_blocking(move || convert(workspace, arguments, token));
            tokio::select! {
                _ = cancellation.cancelled() => Err(ToolError::Cancelled),
                result = task => result.map_err(|error| ToolError::ExecutionFailed(error.to_string()))?,
            }
        })
    }
}

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidArguments {
        tool: "read_document".into(),
        message: message.into(),
    }
}

fn convert(
    workspace: Workspace,
    arguments: Arguments,
    cancellation: CancellationToken,
) -> Result<String, ToolError> {
    let path = Path::new(&arguments.path);
    let file = workspace
        .open_read(path)
        .map_err(|error| ToolError::ExecutionFailed(error.to_string()))?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| ToolError::ExecutionFailed(error.to_string()))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(ToolError::ExecutionFailed(
            "document exceeds the 32 MiB read limit".into(),
        ));
    }
    if cancellation.is_cancelled() {
        return Err(ToolError::Cancelled);
    }

    let format = anydoc::Format::from_bytes(&bytes)
        .or_else(|| anydoc::Format::from_path(path))
        .ok_or_else(|| ToolError::ExecutionFailed("unsupported document format".into()))?;
    let markdown = anydoc::to_markdown_bytes(&bytes, format).map_err(|error| {
        ToolError::ExecutionFailed(format!("document conversion failed: {error}"))
    })?;
    let total_chars = markdown.chars().count();
    if arguments.start_char > total_chars {
        return Err(invalid(format!(
            "start_char exceeds document length ({total_chars} characters)"
        )));
    }

    let content: String = markdown
        .chars()
        .skip(arguments.start_char)
        .take(arguments.max_chars)
        .collect();
    let next = arguments.start_char + content.chars().count();
    Ok(json!({
        "path": arguments.path,
        "format": format!("{format:?}").to_ascii_lowercase(),
        "content": content,
        "start_char": arguments.start_char,
        "total_chars": total_chars,
        "truncated": next < total_chars,
        "next_start_char": (next < total_chars).then_some(next),
        "source_kind": "local_document_markdown"
    })
    .to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn converts_csv_and_pages_without_changing_the_workspace() {
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("data.csv"),
            "name,value\nalpha,1\nbeta,2\n",
        )
        .unwrap();
        let tool = ReadDocument::new(Workspace::new(project.path()).unwrap());

        let first = tool
            .execute(
                ToolCall::function(
                    "doc-1",
                    "read_document",
                    json!({ "path": "data.csv", "max_chars": 12 }),
                ),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let first: serde_json::Value = serde_json::from_str(&first).unwrap();

        assert_eq!(first["source_kind"], "local_document_markdown");
        assert_eq!(first["truncated"], true);
        assert!(first["next_start_char"].as_u64().unwrap() > 0);
        assert_eq!(std::fs::read_dir(project.path()).unwrap().count(), 1);
    }
}
