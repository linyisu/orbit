use std::path::Path;

use anyhow::{Context, Result, ensure};
use orbit_model::types::Attachment;
use orbit_runtime::assets::AssetDirectory;

pub const MAX_IMAGE_BYTES: usize = Attachment::MAX_IMAGE_BYTES;
const MAX_ATTACHMENT_EDGE_PIXELS: u32 = 2048;

#[derive(Clone)]
pub struct ImageAssets {
    directory: AssetDirectory,
    temporary: Option<std::sync::Arc<tempfile::TempDir>>,
}

impl ImageAssets {
    pub fn for_database(database: &Path) -> Self {
        Self {
            directory: AssetDirectory::for_database(database),
            temporary: None,
        }
    }

    /// Composer images are uploads in transit, not durable session attachments.
    pub fn temporary() -> Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let temporary = std::sync::Arc::new(
            tempfile::Builder::new()
                .prefix("orbit-composer-")
                .permissions(std::fs::Permissions::from_mode(0o700))
                .tempdir_in("/tmp")?,
        );
        Ok(Self {
            directory: AssetDirectory::for_database(&temporary.path().join("images")),
            temporary: Some(temporary),
        })
    }

    /// Read an immutable asset after the caller has checked session ownership.
    pub fn read(&self, path: &Path) -> Result<Vec<u8>> {
        self.directory.read(path, MAX_IMAGE_BYTES)
    }

    pub fn import_path(&self, source: &Path) -> Result<Attachment> {
        let bytes = Attachment::read_image_bytes(source)
            .with_context(|| format!("无法读取图片 {}", source.display()))?;
        let name = source
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();

        self.import_bytes(name, bytes)
    }

    pub fn import_bytes(&self, name: impl Into<String>, bytes: Vec<u8>) -> Result<Attachment> {
        ensure!(!bytes.is_empty(), "图片是空文件");
        ensure!(bytes.len() <= MAX_IMAGE_BYTES, "图片不能超过 20 MiB");

        let (bytes, mime_type, extension) = normalize_image(bytes)?;
        let (destination, lease) = self.directory.import(&bytes, extension)?;

        if let Some(temporary) = &self.temporary {
            return Ok(Attachment::temporary_image(
                name.into(),
                mime_type.into(),
                destination,
                temporary.clone(),
            ));
        }

        Ok(Attachment::Image {
            name: name.into(),
            mime_type: mime_type.into(),
            path: destination,
            lease: Some(lease),
        })
    }

    pub fn import_rgba(
        &self,
        name: impl Into<String>,
        width: usize,
        height: usize,
        bytes: Vec<u8>,
    ) -> Result<Attachment> {
        let width = u32::try_from(width).context("剪贴板图片宽度过大")?;
        let height = u32::try_from(height).context("剪贴板图片高度过大")?;
        let image = image::RgbaImage::from_raw(width, height, bytes)
            .context("剪贴板返回了不完整的 RGBA 图片")?;
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image)
            .write_to(&mut png, image::ImageFormat::Png)
            .context("无法编码剪贴板图片")?;

        self.import_bytes(name, png.into_inner())
    }
}

fn image_type(bytes: &[u8]) -> Option<(&'static str, &'static str)> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(("image/png", "png"))
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        Some(("image/jpeg", "jpg"))
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(("image/gif", "gif"))
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Some(("image/webp", "webp"))
    } else {
        None
    }
}

fn normalize_image(bytes: Vec<u8>) -> Result<(Vec<u8>, &'static str, &'static str)> {
    use image::{GenericImageView, ImageFormat, ImageReader};

    let (mime_type, extension) =
        image_type(&bytes).context("只支持 PNG、JPEG、GIF 和 WebP 图片；扩展名不能代替真实格式")?;
    let format = match mime_type {
        "image/png" => ImageFormat::Png,
        "image/jpeg" => ImageFormat::Jpeg,
        "image/gif" => ImageFormat::Gif,
        "image/webp" => ImageFormat::WebP,
        _ => unreachable!(),
    };

    let image = ImageReader::with_format(std::io::Cursor::new(&bytes), format)
        .decode()
        .context("图片内容损坏或无法解码")?;
    let (width, height) = image.dimensions();
    ensure!(width > 0 && height > 0, "图片尺寸无效");
    if width <= MAX_ATTACHMENT_EDGE_PIXELS && height <= MAX_ATTACHMENT_EDGE_PIXELS {
        return Ok((bytes, mime_type, extension));
    }

    let resized = image.resize(
        MAX_ATTACHMENT_EDGE_PIXELS,
        MAX_ATTACHMENT_EDGE_PIXELS,
        image::imageops::FilterType::Lanczos3,
    );
    let mut output = std::io::Cursor::new(Vec::new());
    resized
        .write_to(&mut output, ImageFormat::Png)
        .context("无法编码缩小后的图片")?;

    Ok((output.into_inner(), "image/png", "png"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_reads_check_location_and_content() {
        let root = tempfile::tempdir().unwrap();
        let assets = ImageAssets::for_database(&root.path().join("sessions.db"));
        let attachment = assets
            .import_rgba("clipboard.png", 1, 1, vec![255; 4])
            .unwrap();
        let path = attachment.path().to_owned();
        let bytes = assets.read(&path).unwrap();
        let external = root.path().join(path.file_name().unwrap());
        std::fs::write(&external, &bytes).unwrap();
        assert!(assets.read(&external).is_err());
        std::fs::write(&path, b"modified").unwrap();
        assert!(
            assets
                .read(&path)
                .unwrap_err()
                .to_string()
                .contains("integrity")
        );
    }
}
