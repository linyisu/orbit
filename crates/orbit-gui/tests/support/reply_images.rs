//! Distinct raster, vector, embedded and HTTP resources for native reply checks.
use base64::Engine as _;
use serde_json::{Value, json};
use std::path::Path;

pub(super) fn seed(workspace: &str) -> anyhow::Result<Vec<Value>> {
    let root = Path::new(workspace);
    let raster = image::RgbaImage::from_fn(180, 54, |x, _| {
        image::Rgba(if x < 90 {
            [32, 105, 220, 255]
        } else {
            [235, 113, 45, 255]
        })
    });
    raster.save(root.join("reply image.png"))?;
    std::fs::write(
        root.join("reply.svg"),
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="180" height="54"><rect width="180" height="54" fill="#217e50"/><circle cx="90" cy="27" r="20" fill="#f4da70"/></svg>"##,
    )?;
    let absolute = url::Url::from_file_path(root.join("reply image.png")).unwrap();
    let vector = url::Url::from_file_path(root.join("reply.svg")).unwrap();
    let embedded = base64::engine::general_purpose::STANDARD
        .encode(std::fs::read(root.join("reply image.png"))?);
    let mut reply = format!(
        "本地相对路径：![relative](reply%20image.png)\n\n绝对路径：![absolute]({})\n\nSVG：![vector]({vector})\n\n内嵌图片：![embedded](data:image/png;base64,{embedded})",
        absolute.path()
    );
    if let Ok(url) = std::fs::read_to_string(root.join(".fixture-image-http")) {
        reply.push_str(&format!("\n\n网络图片：![http]({})", url.trim()));
    }
    reply.push_str("\n\n失效图片：![missing](missing.png)\n\n图片后的正文仍然可见。");
    Ok(vec![
        json!({"Started":{"prompt":"展示合成的几何图案"}}),
        json!({"Agent":{"Model":{"TextDelta":reply}}}),
        json!({"Finished":{"status":"completed"}}),
    ])
}

pub(super) fn replace(workspace: &str) -> anyhow::Result<Value> {
    image::RgbaImage::from_pixel(180, 54, image::Rgba([174, 65, 171, 255]))
        .save(Path::new(workspace).join("reply image.png"))?;
    Ok(
        json!({"Agent":{"Model":{"TextDelta":"重新生成同名图片：![new](reply%20image.png)\n\n![standalone](reply%20image.png)\n\n上面的旧回答保留原图。"}}}),
    )
}
