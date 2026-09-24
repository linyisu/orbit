use orbit_agent::tool::{Tool, ToolError, ToolFuture};
use orbit_model::types::{ToolCall, ToolDefinition};
use orbit_store::Store;
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    output_id: String,
    stream: String,
    #[serde(default)]
    offset: usize,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    4096
}

pub struct ReadCommandOutput {
    store: Store,
    session_id: String,
}

impl ReadCommandOutput {
    pub fn new(store: Store, session_id: String) -> Self {
        Self { store, session_id }
    }
}

impl Tool for ReadCommandOutput {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "read_command_output",
            "Read a byte-range page of retained stdout or stderr from a command in this session. The output_id is returned by run_command. Results remain available after resuming the session, subject to the 128 MiB per-session retention limit. Each stream retains its first 16 MiB; total_bytes reports the original stream size.",
            json!({
                "type": "object",
                "properties": {
                    "output_id": { "type": "string", "description": "ID returned by run_command" },
                    "stream": { "type": "string", "enum": ["stdout", "stderr"] },
                    "offset": { "type": "integer", "minimum": 0, "default": 0 },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 8192, "default": 4096 }
                },
                "required": ["output_id", "stream"],
                "additionalProperties": false
            }),
        )
    }

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            let args: Arguments =
                serde_json::from_str(&call.function.arguments).map_err(|error| {
                    ToolError::InvalidArguments {
                        tool: "read_command_output".to_owned(),
                        message: error.to_string(),
                    }
                })?;
            if cancellation.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            let page = self
                .store
                .read_command_output(
                    &self.session_id,
                    &args.output_id,
                    &args.stream,
                    args.offset,
                    args.limit,
                )
                .await
                .map_err(|error| ToolError::ExecutionFailed(error.to_string()))?;
            let next_offset = page.offset + page.content.len();
            let content = String::from_utf8_lossy(&page.content);
            Ok(json!({
                "output_id": args.output_id,
                "stream": args.stream,
                "offset": page.offset,
                "next_offset": next_offset,
                "retained_bytes": page.retained_bytes,
                "total_bytes": page.total_bytes,
                "has_more": next_offset < page.retained_bytes,
                "retention_truncated": page.retained_bytes < page.total_bytes,
                "content": content
            })
            .to_string())
        })
    }
}
