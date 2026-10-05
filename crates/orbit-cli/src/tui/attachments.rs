//! Local attachment paths, clipboard acquisition and composer attachment state.
use std::path::PathBuf;

use anyhow::Result;

use super::app::{TranscriptItem, UiState};
use crate::session::SessionState;

const VISION_DISABLED_MESSAGE: &str = "当前模型未启用图片输入，未添加图片附件。可切换支持图片的模型，或在 /connect 中调整自定义模型的图片输入设置。";

pub(super) fn attachment_path(argument: &str) -> std::path::PathBuf {
    let trimmed = argument.trim();
    let trimmed = if trimmed.len() >= 2
        && ((trimmed.starts_with('"') && trimmed.ends_with('"'))
            || (trimmed.starts_with('\'') && trimmed.ends_with('\'')))
    {
        &trimmed[1..trimmed.len() - 1]
    } else {
        trimmed
    };

    if let Ok(url) = url::Url::parse(trimmed)
        && url.scheme() == "file"
        && let Ok(path) = url.to_file_path()
    {
        return path;
    }

    #[cfg(unix)]
    let trimmed = {
        let mut value = String::with_capacity(trimmed.len());
        let mut chars = trimmed.chars();

        while let Some(character) = chars.next() {
            if character == '\\' {
                value.push(chars.next().unwrap_or(character));
            } else {
                value.push(character);
            }
        }

        value
    };
    #[cfg(not(unix))]
    let trimmed = trimmed.to_owned();

    trimmed.into()
}

pub(super) fn resolved_image_path(value: &str, directory: &std::path::Path) -> Option<PathBuf> {
    let path = attachment_path(value);
    let path = if path.is_absolute() {
        path
    } else {
        directory.join(path)
    };

    let extension = path.extension()?.to_str()?.to_ascii_lowercase();

    matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp")
        .then_some(path)
        .filter(|path| path.is_file())
}

pub(super) fn pasted_image_paths(text: &str, directory: &std::path::Path) -> Option<Vec<PathBuf>> {
    if let Some(path) = resolved_image_path(text, directory) {
        return Some(vec![path]);
    }

    #[cfg(unix)]
    {
        let paths = shlex::split(text)?
            .into_iter()
            .map(|part| resolved_image_path(&part, directory))
            .collect::<Option<Vec<_>>>()?;
        (!paths.is_empty()).then_some(paths)
    }

    #[cfg(not(unix))]
    None
}

pub(super) fn queue_attachment(
    state: &mut UiState,
    attachment: orbit_model::types::Attachment,
) -> bool {
    if !state.supports_vision {
        return false;
    }
    if state
        .attachments
        .iter()
        .any(|existing| existing == &attachment)
    {
        return false;
    }

    state.attachments.push(attachment);
    state.attachment_cursor = None;

    true
}

pub(super) fn focus_attachment_at(state: &mut UiState, x: u16, y: u16) -> bool {
    let Some(index) = state
        .attachment_areas
        .iter()
        .position(|area| area.contains((x, y).into()))
    else {
        return false;
    };

    state.attachment_cursor = Some(index);

    true
}

pub(super) fn remove_focused_attachment(state: &mut UiState, backwards: bool) -> bool {
    let Some(index) = state.attachment_cursor.take() else {
        return false;
    };
    if index >= state.attachments.len() {
        return false;
    }

    state.attachments.remove(index);

    if backwards && index > 0 {
        state.attachment_cursor = Some(index - 1);
    } else if !backwards && index < state.attachments.len() {
        state.attachment_cursor = Some(index);
    }

    true
}

pub(super) fn attach_paths(
    state: &mut UiState,
    session: &SessionState,
    paths: Vec<PathBuf>,
) -> Result<()> {
    if !state.supports_vision {
        state.report_local(TranscriptItem::CommandError(VISION_DISABLED_MESSAGE.into()));
        return Ok(());
    }

    for path in paths {
        let attachment = session.import_image(&path)?;
        queue_attachment(state, attachment);
    }

    Ok(())
}

enum ClipboardPayload {
    Image {
        width: usize,
        height: usize,
        bytes: Vec<u8>,
    },
    Files(Vec<PathBuf>),
    Text(String),
}

fn read_clipboard() -> Result<ClipboardPayload> {
    let mut clipboard = arboard::Clipboard::new()?;

    if let Ok(image) = clipboard.get_image() {
        return Ok(ClipboardPayload::Image {
            width: image.width,
            height: image.height,
            bytes: image.bytes.into_owned(),
        });
    }

    if let Ok(files) = clipboard.get().file_list()
        && !files.is_empty()
    {
        return Ok(ClipboardPayload::Files(files));
    }

    if let Ok(text) = clipboard.get_text() {
        return Ok(ClipboardPayload::Text(text));
    }

    anyhow::bail!("剪贴板中没有可用的图片、文件或文本")
}

pub(super) async fn paste_clipboard(state: &mut UiState, session: &SessionState) -> Result<()> {
    let payload = tokio::task::spawn_blocking(read_clipboard).await??;

    match payload {
        ClipboardPayload::Image {
            width,
            height,
            bytes,
        } => {
            if !state.supports_vision {
                state.report_local(TranscriptItem::CommandError(VISION_DISABLED_MESSAGE.into()));
                state.menu_dismissed = false;
                return Ok(());
            }

            let attachment = session.import_clipboard_image(width, height, bytes)?;
            queue_attachment(state, attachment);
        }

        ClipboardPayload::Files(files) => {
            let images = files
                .iter()
                .filter_map(|path| {
                    resolved_image_path(
                        &path.to_string_lossy(),
                        std::path::Path::new(&state.workspace),
                    )
                })
                .collect::<Vec<_>>();

            if images.len() == files.len() {
                attach_paths(state, session, images)?;
            } else {
                state.input.insert(
                    &files
                        .iter()
                        .map(|path| path.to_string_lossy())
                        .collect::<Vec<_>>()
                        .join(" "),
                );
            }
        }

        ClipboardPayload::Text(text) => {
            if let Some(paths) = pasted_image_paths(&text, std::path::Path::new(&state.workspace)) {
                attach_paths(state, session, paths)?;
            } else {
                state.input.insert(&text);
            }
        }
    }

    state.menu_dismissed = false;

    Ok(())
}
