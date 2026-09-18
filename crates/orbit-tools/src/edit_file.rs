use orbit_agent::tool::{Tool, ToolError, ToolFuture};
use orbit_model::types::{ToolCall, ToolDefinition};
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::{approval::ApprovalSender, workspace::Workspace, write_file::WriteFile};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    path: String,
    old_text: String,
    new_text: String,
    #[serde(default)]
    replace_all: bool,
}

pub struct EditFile {
    writer: WriteFile,
}

impl EditFile {
    pub fn new(workspace: Workspace, approvals: ApprovalSender) -> Self {
        Self {
            writer: WriteFile::new(workspace, approvals),
        }
    }
}

impl Tool for EditFile {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "edit_file",
            "Edit an existing UTF-8 workspace file by exact text replacement, after approval. Read the file first. old_text must be nonempty and match exactly once unless replace_all is explicitly true. Include surrounding context to disambiguate. new_text may be empty to delete; include the existing anchor in new_text to insert. Whitespace and line endings must match exactly; no regex or fuzzy matching. Old and new files are limited to 1 MiB. Uses the same protected-path and conflict checks as write_file. Never retry a denied edit without a new user request.",
            json!({"type":"object","properties":{"path":{"type":"string"},"old_text":{"type":"string","minLength":1},"new_text":{"type":"string"},"replace_all":{"type":"boolean","default":false}},"required":["path","old_text","new_text"],"additionalProperties":false}),
        )
    }

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            let args: Arguments = serde_json::from_str(&call.function.arguments).map_err(|e| {
                ToolError::InvalidArguments {
                    tool: "edit_file".into(),
                    message: e.to_string(),
                }
            })?;
            if args.path.trim().is_empty()
                || args.old_text.is_empty()
                || args.old_text.len() > 1024 * 1024
                || args.new_text.len() > 1024 * 1024
                || args.old_text.contains('\0')
                || args.new_text.contains('\0')
            {
                return Err(ToolError::InvalidArguments { tool: "edit_file".into(), message: "path and old_text must be nonempty; old_text and new_text must be text without NUL, at most 1 MiB each".into() });
            }
            self.writer.edit(args.path, move |before| {
                // Count overlapping candidates too: `aa` in `aaa` is ambiguous.
                let count = before.char_indices().filter(|(index, _)| before[*index..].starts_with(&args.old_text)).count();
                if count == 0 {
                    return Err(ToolError::ExecutionFailed("old_text was not found; read the current file and use exact text".into()));
                }
                if count != 1 && !args.replace_all {
                    return Err(ToolError::ExecutionFailed(format!("old_text matches {count} locations; include more context or explicitly set replace_all")));
                }
                // replace_all uses left-to-right, non-overlapping replacement.
                let count = before.matches(&args.old_text).count();
                let new_len = before.len() - count * args.old_text.len();
                if args.new_text.len().checked_mul(count).and_then(|n| new_len.checked_add(n)).is_none_or(|n| n > 1024 * 1024) {
                    return Err(ToolError::ExecutionFailed("edited file would exceed 1 MiB".into()));
                }
                Ok(before.replace(&args.old_text, &args.new_text))
            }, cancellation).await
        })
    }
}
