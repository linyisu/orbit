//! Synthetic message attachments for native ordering and read-only preview checks.
use serde_json::{Value, json};
use std::path::Path;

pub(super) fn seed(workspace: &str) -> anyhow::Result<Vec<Value>> {
    let root = Path::new(workspace);
    let mut images = Vec::new();
    for (name, color) in [
        ("blue.png", [30, 100, 210, 255]),
        ("green.png", [45, 160, 90, 255]),
    ] {
        let path = root.join(name);
        image::RgbaImage::from_pixel(180, 90, image::Rgba(color)).save(&path)?;
        images.push(json!({ "name": name, "path": path }));
    }
    let text = (1..=140)
        .map(|index| format!("Generated row {index:03}: sample value = {}\n", index * 7))
        .collect::<String>();
    let mut files = Vec::new();
    for (name, bytes) in [
        ("sample.txt", text.into_bytes()),
        ("empty.txt", vec![]),
        ("binary.bin", vec![0, 255, 1, 254]),
        (
            "large.txt",
            "合成内容\n"
                .repeat(100_000)
                .into_bytes(),
        ),
    ] {
        let path = root.join(name);
        std::fs::write(&path, bytes)?;
        files.push(json!({ "name": name, "path": path }));
    }
    // The fixture service deliberately returns an error for this synthetic missing resource.
    files.push(json!({ "name": "unavailable.txt", "path": "missing.png" }));
    Ok(vec![
        json!({
            "Started": {
                "prompt": "检查合成附件的排列与只读预览。",
                "presentation": {
                    "image_assets": images,
                    "file_assets": files
                }
            }
        }),
        json!({
            "Finished": { "status": "completed" }
        }),
    ])
}
