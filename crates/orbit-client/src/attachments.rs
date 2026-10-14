use std::path::{Path, PathBuf};

use anyhow::Result;
use orbit_model::types::Attachment;
use orbit_tools::image_assets::ImageAssets;

fn temporary_assets() -> Result<ImageAssets> {
    ImageAssets::temporary()
}

pub fn paths(paths: Vec<PathBuf>) -> Result<Vec<Attachment>> {
    let assets = temporary_assets()?;
    paths
        .iter()
        .map(|path| assets.import_path(path))
        .collect()
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

/// Recognize a clipboard containing only existing local image paths. A whole
/// path is tried first so filenames with spaces need not be quoted; shell-style
/// quoting supports multiple paths on Unix. Prose or one missing file leaves
/// the complete clipboard as text instead of consuming only part of it.
pub fn pasted_paths(text: &str, directory: &Path) -> Option<Vec<PathBuf>> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let whole = if trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')))
    {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };
    if let Some(path) = pasted_path(whole, directory) {
        return Some(vec![path]);
    }

    #[cfg(unix)]
    {
        let paths = shlex::split(text)?
            .into_iter()
            .map(|part| pasted_path(&part, directory))
            .collect::<Option<Vec<_>>>()?;
        (!paths.is_empty()).then_some(paths)
    }

    #[cfg(not(unix))]
    None
}

fn pasted_path(value: &str, directory: &Path) -> Option<PathBuf> {
    let path = if let Ok(url) = url::Url::parse(value)
        && url.scheme() == "file"
    {
        url.to_file_path().ok()?
    } else if let Some(rest) = value.strip_prefix("~/") {
        PathBuf::from(std::env::var_os("HOME")?).join(rest)
    } else {
        PathBuf::from(value)
    };
    let path = if path.is_absolute() {
        path
    } else {
        directory.join(path)
    };
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp"
            )
        })
        .then_some(path)
        .filter(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pasted_paths_accept_existing_absolute_relative_and_file_url_images() {
        let directory = tempfile::tempdir().unwrap();
        let image = directory
            .path()
            .join("中文 screen shot.PNG");
        std::fs::write(&image, b"validated when imported").unwrap();
        for input in [
            image.to_string_lossy().into_owned(),
            "中文 screen shot.PNG".into(),
            "'中文 screen shot.PNG'".into(),
            "\"中文 screen shot.PNG\"".into(),
            url::Url::from_file_path(&image)
                .unwrap()
                .to_string(),
        ] {
            assert_eq!(
                pasted_paths(&input, directory.path()),
                Some(vec![image.clone()]),
                "{input}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn pasted_paths_accept_quoted_multiple_and_escaped_local_images() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("screen shot.png");
        let second = directory.path().join("另一个图.webp");
        let escaped = directory.path().join("back\\slash.gif");
        for image in [&first, &second, &escaped] {
            std::fs::write(image, b"validated when imported").unwrap();
        }
        assert_eq!(
            pasted_paths("'screen shot.png' \"另一个图.webp\"", directory.path()),
            Some(vec![first.clone(), second])
        );
        assert_eq!(
            pasted_paths("screen\\ shot.png", directory.path()),
            Some(vec![first])
        );
        assert_eq!(
            pasted_paths("\"back\\\\slash.gif\"", directory.path()),
            Some(vec![escaped])
        );
    }

    #[test]
    fn pasted_paths_leave_commands_prose_and_incomplete_lists_as_text() {
        let directory = tempfile::tempdir().unwrap();
        let image = directory.path().join("image.png");
        std::fs::write(&image, b"validated when imported").unwrap();
        std::fs::write(directory.path().join("notes.md"), b"text").unwrap();
        for input in [
            "",
            " ",
            "/",
            "/plan",
            "/image image.png",
            "look at image.png",
            "image.png please inspect",
            "image.png notes.md",
            "image.png missing.png",
            "missing.png",
            "https://example.test/image.png",
            "'image.png",
        ] {
            assert!(pasted_paths(input, directory.path()).is_none(), "{input}");
        }
        let absolute_prose = format!("{} please inspect", image.display());
        assert!(pasted_paths(&absolute_prose, directory.path()).is_none());
        assert_eq!(
            pasted_paths(image.to_str().unwrap(), directory.path()),
            Some(vec![image])
        );
    }
}
