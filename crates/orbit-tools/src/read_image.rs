use std::{io::Read, path::Path};

use orbit_agent::tool::{Tool, ToolError, ToolFuture, ToolOutput};
use orbit_model::types::{ToolCall, ToolDefinition};
use orbit_runtime::filesystem::Workspace;
use serde::Deserialize;
use serde_json::json;
use tokio_util::sync::CancellationToken;

use crate::image_assets::{ImageAssets, MAX_IMAGE_BYTES};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Arguments {
    path: String,
}

pub struct ReadImage {
    workspace: Workspace,
    assets: ImageAssets,
    enabled: bool,
}

impl ReadImage {
    pub fn new(workspace: Workspace, assets: ImageAssets) -> Self {
        Self {
            workspace,
            assets,
            enabled: true,
        }
    }

    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

impl Tool for ReadImage {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::function(
            "read_image",
            "Load a PNG, JPEG, GIF or WebP image inside the workspace (or outside path granted via request_capability) into the model's native vision input. Use this whenever the user asks about an image file; filesystem listing or shell pixel inspection does not show the image to the model. Use a relative path or granted outside path. The image is validated locally, copied to Orbit's global attachment store, and no project file is created.",
            json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "Path relative to the workspace root, or an outside path granted via request_capability" }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
        )
    }

    fn execute<'a>(&'a self, call: ToolCall, cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async move {
            if !self.enabled {
                return Err(ToolError::ExecutionFailed(
                    "current model does not support image input".into(),
                ));
            }

            let arguments: Arguments = serde_json::from_str(&call.function.arguments)
                .map_err(|error| invalid(error.to_string()))?;

            if arguments.path.trim().is_empty() {
                return Err(invalid("path must be nonempty"));
            }
            if cancellation.is_cancelled() {
                return Err(ToolError::Cancelled);
            }

            let mut file = self
                .workspace
                .authorize_read(Path::new(&arguments.path))
                .map_err(|error| ToolError::ExecutionFailed(error.to_string()))?;
            let mut bytes = Vec::new();
            file.by_ref()
                .take(MAX_IMAGE_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(|error| ToolError::ExecutionFailed(error.to_string()))?;

            if bytes.len() > MAX_IMAGE_BYTES {
                return Err(ToolError::ExecutionFailed(
                    "image exceeds the 20 MiB read limit".into(),
                ));
            }

            if cancellation.is_cancelled() {
                return Err(ToolError::Cancelled);
            }

            let name = Path::new(&arguments.path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let attachment = self
                .assets
                .import_bytes(name, bytes)
                .map_err(|error| ToolError::ExecutionFailed(error.to_string()))?;

            Ok(ToolOutput::with_attachments(
                json!({ "path": arguments.path, "loaded": true, "source_kind": "local_image" })
                    .to_string(),
                vec![attachment],
            ))
        })
    }
}

fn invalid(message: impl Into<String>) -> ToolError {
    ToolError::InvalidArguments {
        tool: "read_image".into(),
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[tokio::test]
    async fn loads_a_workspace_image_as_a_global_model_attachment() {
        let workspace = tempdir().unwrap();
        let data = tempdir().unwrap();
        let source = workspace.path().join("scene.png");
        image::RgbaImage::from_pixel(2, 2, image::Rgba([10, 20, 30, 255]))
            .save(&source)
            .unwrap();
        let tool = ReadImage::new(
            Workspace::new(workspace.path()).unwrap(),
            ImageAssets::for_database(&data.path().join("orbit.db")),
        );
        let output = tool
            .execute(
                ToolCall::function("image-1", "read_image", json!({ "path": "scene.png" })),
                CancellationToken::new(),
            )
            .await
            .unwrap();

        assert_eq!(output.attachments.len(), 1);

        let orbit_model::types::Attachment::Image { path, .. } = &output.attachments[0];

        assert!(path.starts_with(data.path().join("orbit.attachments")));
        assert_eq!(std::fs::read_dir(workspace.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn disabled_tool_fails_fast_with_clear_error() {
        let workspace = tempdir().unwrap();
        let data = tempdir().unwrap();
        let tool = ReadImage::new(
            Workspace::new(workspace.path()).unwrap(),
            ImageAssets::for_database(&data.path().join("orbit.db")),
        )
        .with_enabled(false);
        let err = tool
            .execute(
                ToolCall::function("image-1", "read_image", json!({ "path": "scene.png" })),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();

        assert!(
            matches!(err, ToolError::ExecutionFailed(msg) if msg.contains("does not support image input"))
        );
    }
}
