use std::path::PathBuf;

use anyhow::Result;
use orbit_model::types::Attachment;
use orbit_tools::image_assets::ImageAssets;

fn temporary_assets() -> Result<ImageAssets> {
    ImageAssets::temporary()
}

pub fn paths(paths: Vec<PathBuf>) -> Result<Vec<Attachment>> {
    let assets = temporary_assets()?;
    paths.iter().map(|path| assets.import_path(path)).collect()
}

pub fn rgba(width: usize, height: usize, bytes: Vec<u8>) -> Result<Vec<Attachment>> {
    Ok(vec![temporary_assets()?.import_rgba(
        "clipboard.png",
        width,
        height,
        bytes,
    )?])
}

pub fn uploads(attachments: &[Attachment]) -> Result<Vec<orbit_protocol::ImageUpload>> {
    attachments
        .iter()
        .map(|image| {
            Ok(orbit_protocol::ImageUpload {
                name: image.name().into(),
                data: image.image()?.1,
            })
        })
        .collect()
}
