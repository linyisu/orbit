use orbit_agent::tool::{Tool, ToolError, ToolFuture};
use orbit_model::types::{ToolCall, ToolDefinition};
use orbit_store::Store;
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArguments {
    sha256: String,
    #[serde(default)]
    offset: usize,
    #[serde(default = "default_read_limit")]
    limit: usize,
}

fn default_read_limit() -> usize {
    4096
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchArguments {
    query: String,
    #[serde(default = "default_search_limit")]
    limit: usize,
}

fn default_search_limit() -> usize {
    10
}

pub struct ReadHistory {
    store: Store,
    session_id: String,
}

impl ReadHistory {
    pub fn new(store: Store, session_id: String) -> Self {
        Self { store, session_id }
    }
}

impl Tool for ReadHistory {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "read_history",
            "Read a page of an original message from this session by sha256 digest. Use references in compacted context or results from search_history. The canonical message remains unchanged after compaction.",
            json!({
                "type": "object",
                "properties": {
                    "sha256": { "type": "string", "description": "SHA-256 message digest from context or search_history" },
                    "offset": { "type": "integer", "minimum": 0, "default": 0 },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 8192, "default": 4096 }
                },
                "required": ["sha256"],
                "additionalProperties": false
            }),
        )
    }

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            let args: ReadArguments =
                serde_json::from_str(&call.function.arguments).map_err(|error| {
                    ToolError::InvalidArguments {
                        tool: "read_history".to_owned(),
                        message: error.to_string(),
                    }
                })?;
            if cancellation.is_cancelled() {
                return Err(ToolError::Cancelled);
            }

            let page = self
                .store
                .read_history(&self.session_id, &args.sha256, args.offset, args.limit)
                .await
                .map_err(|error| ToolError::ExecutionFailed(error.to_string()))?;
            let next_offset = page.offset + page.content.len();
            Ok(json!({
                "message_id": page.message_id,
                "sha256": page.sha256,
                "offset": page.offset,
                "next_offset": next_offset,
                "total_bytes": page.total_bytes,
                "has_more": next_offset < page.total_bytes,
                "content": page.content
            })
            .to_string())
        })
    }
}

pub struct SearchHistory {
    store: Store,
    session_id: String,
}

impl SearchHistory {
    pub fn new(store: Store, session_id: String) -> Self {
        Self { store, session_id }
    }
}

impl Tool for SearchHistory {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "search_history",
            "Search original messages in this session for historical decisions, user requirements, tool outputs, or evidence. Returns sha256 digests for read_history. Search results are session-scoped.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Literal text to search in this session" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 20, "default": 10 }
                },
                "required": ["query"],
                "additionalProperties": false
            }),
        )
    }

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            let args: SearchArguments =
                serde_json::from_str(&call.function.arguments).map_err(|error| {
                    ToolError::InvalidArguments {
                        tool: "search_history".to_owned(),
                        message: error.to_string(),
                    }
                })?;
            if cancellation.is_cancelled() {
                return Err(ToolError::Cancelled);
            }

            let matches = self
                .store
                .search_history(&self.session_id, &args.query, args.limit)
                .await
                .map_err(|error| ToolError::ExecutionFailed(error.to_string()))?;
            let matches = matches
                .into_iter()
                .map(|hit| {
                    json!({
                        "message_id": hit.message_id,
                        "sha256": hit.sha256,
                        "excerpt": hit.excerpt
                    })
                })
                .collect::<Vec<_>>();
            Ok(json!({
                "query": args.query,
                "matches": matches
            })
            .to_string())
        })
    }
}
