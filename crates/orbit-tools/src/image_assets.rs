use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use orbit_model::types::Attachment;
use sha2::{Digest, Sha256};

pub const MAX_IMAGE_BYTES: usize = Attachment::MAX_IMAGE_BYTES;
const MAX_ATTACHMENT_EDGE_PIXELS: u32 = 2048;

#[derive(Clone)]
pub struct ImageAssets {
    directory: PathBuf,
}

impl ImageAssets {
    pub fn for_database(database: &Path) -> Self {
        Self {
            directory: database.with_extension("attachments"),
        }
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
        let digest = Sha256::digest(&bytes);
        let hash = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        std::fs::create_dir_all(&self.directory)?;
        ensure!(
            !self.directory.symlink_metadata()?.file_type().is_symlink(),
            "图片存储目录不能是符号链接"
        );

        let destination = self.directory.join(format!("{hash}.{extension}"));
        let valid_existing = std::fs::read(&destination)
            .ok()
            .is_some_and(|existing| Sha256::digest(existing) == digest);

        if !valid_existing {
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos();
            let temporary = self
                .directory
                .join(format!(".{hash}.{}.{nonce}.tmp", std::process::id()));
            let mut options = OpenOptions::new();
            options.create_new(true).write(true);

            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;

                options.mode(0o600);
            }
            let mut file = options.open(&temporary)?;
            file.write_all(&bytes)?;
            file.sync_all()?;

            if destination.exists() {
                std::fs::remove_file(&destination)?;
            }

            std::fs::rename(&temporary, &destination)?;
        }

        Ok(Attachment::Image {
            name: name.into(),
            mime_type: mime_type.into(),
            path: destination,
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
