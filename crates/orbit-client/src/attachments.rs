use std::path::{Path, PathBuf};

use crate::{
    protocol::{Request, Response},
    transport::Client,
};
use anyhow::{Context, Result, bail};
use orbit_model::types::Attachment;
use orbit_tools::image_assets::ImageAssets;

/// Recognize the image formats accepted by the attachment importer.
pub fn is_image_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp"
            )
        })
}

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

/// Read a bounded regular file before transporting it to the session host.
pub fn file_upload(path: &Path) -> Result<orbit_protocol::FileUpload> {
    use base64::Engine;
    use std::io::Read;
    let metadata = std::fs::metadata(path)?;
    anyhow::ensure!(metadata.is_file(), "附件必须是普通文件：{}", path.display());
    let file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    anyhow::ensure!(
        metadata.is_file() && metadata.len() <= Attachment::MAX_FILE_BYTES as u64,
        "附件必须是 20 MiB 以内的普通文件：{}",
        path.display()
    );
    let mut bytes = Vec::new();
    file.take(Attachment::MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() <= Attachment::MAX_FILE_BYTES,
        "附件超过 20 MiB：{}",
        path.display()
    );
    Ok(orbit_protocol::FileUpload {
        name: path
            .file_name()
            .context("附件缺少文件名")?
            .to_string_lossy()
            .into_owned(),
        data: base64::engine::general_purpose::STANDARD.encode(bytes),
    })
}

/// Recognize only a complete list of existing local files, leaving prose untouched.
pub fn pasted_files(text: &str, directory: &Path) -> Option<Vec<PathBuf>> {
    let text = text
        .strip_prefix("copy\n")
        .or_else(|| text.strip_prefix("cut\n"))
        .filter(|text| {
            text.lines()
                .all(|line| line.starts_with("file://"))
        })
        .unwrap_or(text);
    parse_paths(text, directory, false)
}

/// Recognize a clipboard containing only existing local image paths. A whole
/// path is tried first so filenames with spaces need not be quoted; shell-style
/// quoting supports multiple paths on Unix. Prose or one missing file leaves
/// the complete clipboard as text instead of consuming only part of it.
pub fn pasted_paths(text: &str, directory: &Path) -> Option<Vec<PathBuf>> {
    parse_paths(text, directory, true)
}

fn parse_paths(text: &str, directory: &Path, images_only: bool) -> Option<Vec<PathBuf>> {
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
    if let Some(path) = pasted_path(whole, directory, images_only) {
        return Some(vec![path]);
    }

    #[cfg(unix)]
    {
        let paths = shlex::split(text)?
            .into_iter()
            .map(|part| pasted_path(&part, directory, images_only))
            .collect::<Option<Vec<_>>>()?;
        (!paths.is_empty()).then_some(paths)
    }

    #[cfg(not(unix))]
    None
}

fn pasted_path(value: &str, directory: &Path, images_only: bool) -> Option<PathBuf> {
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
    (!images_only || is_image_path(&path))
        .then_some(path)
        .filter(|path| path.is_file())
}

/// Upload local assets and submit one message without retrying side effects.
pub async fn send(
    client: &mut Client,
    session: String,
    id: String,
    prompt: String,
    paths: Vec<(PathBuf, String)>,
    steer: bool,
) -> Result<()> {
    let (images, files) = tokio::task::spawn_blocking(move || -> Result<_> {
        let (images, files): (Vec<_>, Vec<_>) = paths
            .into_iter()
            .partition(|(path, _)| is_image_path(path));
        let mut uploads = crate::attachments::uploads(&crate::attachments::paths(
            images
                .iter()
                .map(|(path, _)| path.clone())
                .collect(),
        )?)?;
        for (upload, (_, name)) in uploads.iter_mut().zip(images) {
            upload.name = name;
        }
        let files = files
            .iter()
            .map(|(path, name)| {
                let mut upload = crate::attachments::file_upload(path)?;
                upload.name = name.clone();
                Ok(upload)
            })
            .collect::<Result<Vec<_>>>()?;
        Ok((uploads, files))
    })
    .await
    .context("读取附件失败")??;
    let request = if !files.is_empty() {
        Request::SubmitAttachments {
            session,
            request_id: id,
            prompt,
            images,
            files,
            steer,
        }
    } else if steer {
        if images.is_empty() {
            Request::Steer {
                session,
                message: prompt,
            }
        } else {
            Request::SteerImages {
                session,
                message: prompt,
                images,
            }
        }
    } else {
        Request::Start {
            session,
            request_id: id,
            prompt,
            images,
            compact: false,
            model: None,
        }
    };
    match client.request(request).await? {
        Response::Ok | Response::InputQueued { .. } => Ok(()),
        _ => bail!("服务未确认发送，请检查会话状态。"),
    }
}

/// Resolve the client attachment cache beside isolated preferences or in the state directory.
pub fn cache_database(preferences: Option<&std::path::Path>) -> Result<PathBuf> {
    if let Some(path) = preferences {
        return Ok(path.with_extension("queue-cache"));
    }
    Ok(std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .context("找不到附件缓存目录")?
        .join("orbit/gui-queue-cache"))
}
/// Image resource requests use the same proxy, DNS, redirect and size policy as workspace tools.
pub use orbit_tools::web_fetch::{WebClient as ImageClient, WebConfig as ImageNetwork};

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
    #[test]
    fn clipboard_files_are_atomic_and_upload_actual_bytes_with_the_original_name() {
        use base64::Engine;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("sample notes.txt");
        let content = b"  synthetic source\nlast line  ";
        std::fs::write(&path, content).unwrap();
        assert_eq!(
            pasted_files("'sample notes.txt'", root.path()),
            Some(vec![path.clone()])
        );
        let uri = url::Url::from_file_path(&path).unwrap();
        assert_eq!(
            pasted_files(&format!("copy\n{uri}"), root.path()),
            Some(vec![path.clone()])
        );
        assert!(pasted_files("'sample notes.txt' missing.txt", root.path()).is_none());
        assert!(pasted_files("please inspect 'sample notes.txt'", root.path()).is_none());
        let upload = file_upload(&path).unwrap();
        assert_eq!(upload.name, "sample notes.txt");
        assert_eq!(
            base64::engine::general_purpose::STANDARD
                .decode(upload.data)
                .unwrap(),
            content
        );
        assert!(file_upload(root.path()).is_err());
    }
}
