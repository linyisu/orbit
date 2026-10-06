use std::path::PathBuf;

use anyhow::{Context, Result};
use orbit_model::types::Attachment;
use orbit_tools::image_assets::ImageAssets;

fn cache() -> Result<ImageAssets> {
    let root = std::env::var_os("XDG_CACHE_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .context("cannot locate the composer attachment cache")?;
    Ok(ImageAssets::for_database(&root.join("orbit/composer")))
}

pub fn paths(paths: Vec<PathBuf>) -> Result<Vec<Attachment>> {
    let assets = cache()?;
    paths.iter().map(|path| assets.import_path(path)).collect()
}

pub fn rgba(width: usize, height: usize, bytes: Vec<u8>) -> Result<Vec<Attachment>> {
    Ok(vec![cache()?.import_rgba(
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
