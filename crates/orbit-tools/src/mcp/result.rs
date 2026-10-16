//! Project MCP image blocks into native model attachments without sending base64 as text.

use base64::Engine;
use orbit_agent::tool::{ToolError, ToolOutput};
use serde_json::Value;

use crate::image_assets::ImageAssets;

pub(super) fn images(
    mut output: ToolOutput,
    supports_vision: bool,
) -> Result<ToolOutput, ToolError> {
    let mut result: Value = serde_json::from_str(&output.content)
        .map_err(|error| ToolError::ExecutionFailed(error.to_string()))?;
    let Some(blocks) = result
        .get_mut("server_result")
        .and_then(Value::as_array_mut)
    else {
        return Ok(output);
    };
    let mut assets = None;
    for (index, block) in blocks.iter_mut().enumerate() {
        if block
            .get("type")
            .and_then(Value::as_str)
            != Some("image")
        {
            continue;
        }
        let Some(fields) = block.as_object_mut() else {
            continue;
        };
        let data = fields.remove("data");
        let image = (|| -> anyhow::Result<_> {
            if !supports_vision {
                anyhow::bail!("current model does not support image input");
            }
            let data = data
                .as_ref()
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow::anyhow!("MCP image has no base64 data"))?;
            let bytes = base64::engine::general_purpose::STANDARD.decode(data)?;
            if assets.is_none() {
                assets = Some(ImageAssets::temporary()?);
            }
            assets
                .as_ref()
                .unwrap()
                .import_bytes(format!("mcp-image-{}.png", index + 1), bytes)
        })();
        match image {
            Ok(image) => {
                fields.insert("attachment".into(), Value::String(image.name().to_owned()));
                output.attachments.push(image);
            }
            Err(error) => {
                fields.insert("unavailable".into(), Value::String(error.to_string()));
            }
        }
    }
    output.content = result.to_string();
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn images_use_native_attachments_and_preserve_structured_results() {
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(2, 2)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let data = base64::engine::general_purpose::STANDARD.encode(bytes.into_inner());
        let output = ToolOutput::text(
            json!({
                "server_result": [
                    { "type": "text", "text": "A generated image" },
                    { "type": "image", "mimeType": "image/png", "data": data }
                ],
                "structured_content": { "width": 2 }
            })
            .to_string(),
        );
        let projected = images(output.clone(), true).unwrap();
        assert_eq!(projected.attachments.len(), 1);
        assert!(projected.attachments[0].path().exists());
        let content: Value = serde_json::from_str(&projected.content).unwrap();
        assert!(
            content["server_result"][1]
                .get("data")
                .is_none()
        );
        assert_eq!(content["structured_content"]["width"], 2);
        let text_only = images(output, false).unwrap();
        assert!(text_only.attachments.is_empty());
        assert!(!text_only.content.contains(&data));
        assert!(
            text_only
                .content
                .contains("does not support image input")
        );
    }
}
