//! Local attachment paths, clipboard acquisition and composer attachment state.
use std::path::PathBuf;

use anyhow::Result;

use super::app::UiState;

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

    if let Some(rest) = trimmed.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    trimmed.into()
}

pub(super) fn is_image_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "webp"
            )
        })
}

pub(super) fn resolved_image_path(value: &str, directory: &std::path::Path) -> Option<PathBuf> {
    let path = attachment_path(value);
    let path = if path.is_absolute() {
        path
    } else {
        directory.join(path)
    };

    is_image_path(&path)
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
    state.input.clear_selection();
    state.input_dragging = false;
    state.selection.clear();

    true
}

pub(super) fn thumbnails(state: &UiState, width: u16) -> super::media::thumbnails::Strip {
    let mut media = state.media.clone();
    media.files = None;
    super::media::thumbnails::Strip::composer(
        &media,
        state
            .attachments
            .iter()
            .map(|attachment| {
                let path = attachment
                    .path()
                    .to_string_lossy()
                    .into_owned();
                (super::media::Source::Image(path), String::new())
            }),
        width,
    )
}

pub(super) fn move_attachment_vertical(state: &mut UiState, down: bool) {
    let strip = thumbnails(state, (state.input_width as u16).saturating_add(2));
    let current = state
        .attachment_cursor
        .and_then(|index| strip.areas.get(index));
    let row = match current {
        Some(current) => strip
            .areas
            .iter()
            .filter(|area| {
                if down {
                    area.y > current.y
                } else {
                    area.y < current.y
                }
            })
            .min_by_key(|area| area.y.abs_diff(current.y))
            .map(|area| area.y),
        None => strip.areas.last().map(|area| area.y),
    };
    let column = current.map_or_else(
        || {
            state
                .input
                .visual_lines(state.input_width)
                .1
                .column as u16
                + 2
        },
        |area| area.x + area.width / 2,
    );
    if let Some(row) = row {
        state.attachment_cursor = strip
            .areas
            .iter()
            .enumerate()
            .filter(|(_, area)| area.y == row)
            .min_by_key(|(_, area)| (area.x + area.width / 2).abs_diff(column))
            .map(|(index, _)| index);
        state.input.clear_selection();
    } else if down {
        state.attachment_cursor = None;
    }
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

pub(super) enum ClipboardPayload {
    Image {
        width: usize,
        height: usize,
        bytes: Vec<u8>,
    },
    Files(Vec<PathBuf>),
    Text(String),
}

pub(super) fn read_clipboard() -> Result<ClipboardPayload> {
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
