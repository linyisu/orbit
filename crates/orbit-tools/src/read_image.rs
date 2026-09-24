use std::{io::Read, path::Path};

use orbit_agent::tool::{Tool, ToolError, ToolFuture, ToolOutput, ToolOutputFuture};
use orbit_model::types::{MAX_IMAGE_BYTES, ToolCall, ToolDefinition};
use orbit_store::Store;
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::workspace::Workspace;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    path: String,
}

pub struct ReadImage {
    workspace: Workspace,
    store: Store,
    session_id: String,
}

impl ReadImage {
    pub fn new(workspace: Workspace, store: Store, session_id: String) -> Self {
        Self {
            workspace,
            store,
            session_id,
        }
    }
}

impl Tool for ReadImage {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "read_image",
            "Load a PNG, JPEG, GIF, or WebP image from the workspace into the model's vision input. Use this when the user asks about a local image. The file is read through the protected workspace boundary; no project file is created.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path relative to the workspace root" }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        )
    }

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            self.execute_output(call, cancellation)
                .await
                .map(|output| output.content)
        })
    }

    fn execute_output<'a>(
        &'a self,
        call: ToolCall,
        cancellation: CancellationToken,
    ) -> ToolOutputFuture<'a> {
        Box::pin(async move {
            let arguments: Arguments =
                serde_json::from_str(&call.function.arguments).map_err(|error| {
                    ToolError::InvalidArguments {
                        tool: "read_image".into(),
                        message: error.to_string(),
                    }
                })?;
            if arguments.path.trim().is_empty() {
                return Err(ToolError::InvalidArguments {
                    tool: "read_image".into(),
                    message: "path must be nonempty".into(),
                });
            }
            if cancellation.is_cancelled() {
                return Err(ToolError::Cancelled);
            }

            let workspace = self.workspace.clone();
            let path = arguments.path.clone();
            let token = cancellation.clone();
            let bytes = tokio::task::spawn_blocking(move || {
                let file = workspace
                    .open_read(Path::new(&path))
                    .map_err(|error| ToolError::ExecutionFailed(error.to_string()))?;
                let mut bytes = Vec::new();
                file.take(MAX_IMAGE_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|error| ToolError::ExecutionFailed(error.to_string()))?;
                if bytes.len() > MAX_IMAGE_BYTES {
                    return Err(ToolError::ExecutionFailed(
                        "image exceeds the 20 MiB read limit".into(),
                    ));
                }
                if token.is_cancelled() {
                    return Err(ToolError::Cancelled);
                }
                Ok(bytes)
            })
            .await
            .map_err(|error| ToolError::ExecutionFailed(error.to_string()))??;
            if cancellation.is_cancelled() {
                return Err(ToolError::Cancelled);
            }

            let name = Path::new(&arguments.path)
                .file_name()
                .and_then(|value| value.to_str())
                .ok_or_else(|| ToolError::InvalidArguments {
                    tool: "read_image".into(),
                    message: "path has no valid filename".into(),
                })?
                .to_owned();
            let attachment = self
                .store
                .save_image_bytes(&self.session_id, name, bytes)
                .await
                .map_err(|error| ToolError::ExecutionFailed(error.to_string()))?;
            Ok(ToolOutput::with_attachments(
                json!({
                    "path": arguments.path,
                    "loaded": true,
                    "source_kind": "local_image"
                })
                .to_string(),
                vec![attachment],
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn loads_a_protected_workspace_image_for_the_model() {
        let project = tempfile::tempdir().unwrap();
        let source = project.path().join("scene.png");
        image::RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 255]))
            .save(&source)
            .unwrap();
        let database = project.path().join("sessions.sqlite");
        let store = Store::open(&database).await.unwrap();
        let writer = store.create(project.path(), "vision-model").await.unwrap();
        let mut workspace = Workspace::new(project.path()).unwrap();
        workspace
            .protect_opaque_runtime_root(database.with_extension("attachments"))
            .unwrap();
        let tool = ReadImage::new(workspace, store, writer.id().to_owned());

        let output = tool
            .execute_output(
                ToolCall::function("image-1", "read_image", json!({ "path": "scene.png" })),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert_eq!(output.attachments.len(), 1);
        let (mime_type, _) = output.attachments[0].image().unwrap();
        assert_eq!(mime_type, "image/png");
        assert!(output.content.contains("local_image"));
        assert!(source.exists());
    }
}
