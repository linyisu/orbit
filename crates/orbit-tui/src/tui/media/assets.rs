use std::{
    io::Cursor,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

use anyhow::{Result, bail};
use image::{DynamicImage, ImageReader};
use resvg::usvg;

use super::decode::{MAX_DECODE_EDGE_PIXELS, MAX_SOURCE_PIXELS};

#[derive(Default)]
pub(super) struct Assets {
    local: Option<Local>,
    raster: Option<(Arc<Vec<u8>>, Arc<DynamicImage>)>,
    svg: Option<(Arc<Vec<u8>>, Arc<usvg::Tree>)>,
}

struct Local {
    path: PathBuf,
    length: u64,
    modified: SystemTime,
    bytes: Arc<Vec<u8>>,
}

impl Assets {
    pub fn bytes(&mut self, path: &Path) -> Result<Arc<Vec<u8>>> {
        let metadata = std::fs::metadata(path)?;
        let modified = metadata.modified().ok();
        if let Some(local) = &self.local
            && local.path == path
            && local.length == metadata.len()
            && Some(local.modified) == modified
        {
            return Ok(local.bytes.clone());
        }
        let bytes = Arc::new(orbit_model::types::Attachment::read_image_bytes(path)?);
        self.local = modified.map(|modified| Local {
            path: path.to_owned(),
            length: metadata.len(),
            modified,
            bytes: bytes.clone(),
        });
        Ok(bytes)
    }

    pub fn raster(&mut self, bytes: &Arc<Vec<u8>>) -> Result<Arc<DynamicImage>> {
        if let Some((cached, image)) = &self.raster
            && Arc::ptr_eq(bytes, cached)
        {
            return Ok(image.clone());
        }
        let mut reader = ImageReader::new(Cursor::new(bytes.as_slice())).with_guessed_format()?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(MAX_DECODE_EDGE_PIXELS);
        limits.max_image_height = Some(MAX_DECODE_EDGE_PIXELS);
        limits.max_alloc = Some(MAX_SOURCE_PIXELS * 4);
        reader.limits(limits);
        let image = reader.decode()?;
        if u64::from(image.width()) * u64::from(image.height()) > MAX_SOURCE_PIXELS {
            bail!("图片像素尺寸过大");
        }
        let image = Arc::new(DynamicImage::ImageRgba8(image.into_rgba8()));
        self.raster = Some((bytes.clone(), image.clone()));
        Ok(image)
    }

    pub fn svg(
        &mut self,
        bytes: &Arc<Vec<u8>>,
        fonts: Option<Arc<usvg::fontdb::Database>>,
    ) -> Result<Arc<usvg::Tree>> {
        if let Some((cached, tree)) = &self.svg
            && Arc::ptr_eq(bytes, cached)
        {
            return Ok(tree.clone());
        }
        let tree = Arc::new(super::decode::parse_svg(bytes, fonts)?);
        self.svg = Some((bytes.clone(), tree.clone()));
        Ok(tree)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoded_sources_are_reused_and_local_edits_refresh_the_cache() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("picture.png");
        let write = |color| {
            image::RgbaImage::from_pixel(8, 8, image::Rgba(color))
                .save(&path)
                .unwrap();
        };
        write([255, 0, 0, 255]);
        let modified = std::fs::metadata(&path)
            .unwrap()
            .modified()
            .unwrap();
        let mut assets = Assets::default();
        let bytes = assets.bytes(&path).unwrap();
        let image = assets.raster(&bytes).unwrap();
        assert!(Arc::ptr_eq(&bytes, &assets.bytes(&path).unwrap()));
        assert!(Arc::ptr_eq(&image, &assets.raster(&bytes).unwrap()));

        write([0, 255, 0, 255]);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(modified + std::time::Duration::from_secs(1))
            .unwrap();
        let edited = assets.bytes(&path).unwrap();
        let replacement = assets.raster(&edited).unwrap();
        assert!(!Arc::ptr_eq(&bytes, &edited));
        assert!(!Arc::ptr_eq(&image, &replacement));
        assert_eq!(replacement.to_rgba8().get_pixel(0, 0).0, [0, 255, 0, 255]);

        let svg = Arc::new(
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="20" height="10"/>"#.to_vec(),
        );
        let tree = assets.svg(&svg, None).unwrap();
        assert!(Arc::ptr_eq(&tree, &assets.svg(&svg, None).unwrap()));
        let changed = Arc::new(
            br#"<svg xmlns="http://www.w3.org/2000/svg" width="40" height="10"/>"#.to_vec(),
        );
        let replacement = assets.svg(&changed, None).unwrap();
        assert!(!Arc::ptr_eq(&tree, &replacement));
        assert_eq!(replacement.size().width(), 40.0);
    }
}
