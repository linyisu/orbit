//! Attachment thumbnails share geometry, fallback and image metadata in both
//! the composer and saved messages. Decode only when their rows are visible.
use ratatui::{
    layout::{Rect, Size},
    text::{Line, Span},
};
use std::sync::Arc;

use super::{Context, CopyText, Fragment, Request, Source, Target};
use crate::tui::{icons, markdown::LinkedLine};

pub(in crate::tui) const COMPOSER_SIZE: Size = Size::new(16, 4);
pub(in crate::tui) const MESSAGE_SIZE: Size = Size::new(16, 3);
const COLUMN_GAP: u16 = 2;
const ROW_GAP: u16 = 1;

pub(in crate::tui) struct Strip {
    pub rows: Vec<LinkedLine>,
    pub areas: Vec<Rect>,
}

fn columns(width: u16, size: Size) -> usize {
    usize::from(width.saturating_add(COLUMN_GAP) / (size.width + COLUMN_GAP)).max(1)
}

pub(in crate::tui) fn height(count: usize, width: u16, size: Size) -> u16 {
    if count == 0 || width == 0 {
        return 0;
    }
    count
        .div_ceil(columns(width, size))
        .saturating_mul(usize::from(size.height + ROW_GAP))
        .saturating_sub(usize::from(ROW_GAP))
        .min(usize::from(u16::MAX)) as u16
}

impl Strip {
    pub fn new(
        context: &Context,
        images: impl IntoIterator<Item = (Source, String)>,
        width: u16,
        size: Size,
    ) -> Self {
        let images = images.into_iter().collect::<Vec<_>>();
        let height = height(images.len(), width, size);
        let mut strip = Self {
            rows: (0..height)
                .map(|_| LinkedLine::chrome(Line::from(" ".repeat(usize::from(width)))))
                .collect(),
            areas: Vec::new(),
        };
        if width == 0 {
            return strip;
        }
        let columns = columns(width, size);
        for (index, (source, label)) in images.into_iter().enumerate() {
            let x = (index % columns) as u16 * (size.width + COLUMN_GAP);
            let y = (index / columns).saturating_mul(usize::from(size.height + ROW_GAP));
            if y + usize::from(size.height) > strip.rows.len() {
                break;
            }
            let area = Rect::new(x, y as u16, size.width.min(width), size.height);
            strip.areas.push(area);
            let picture = context.cached_picture(&source, area.width, area.height);
            if context.enabled() {
                // Every slice can independently become visible after scrolling.
                for row in &mut strip.rows[y..y + usize::from(area.height)] {
                    row.media_requests.push(Request {
                        source: source.clone(),
                        width: area.width,
                        height: area.height,
                    });
                }
            }
            if let Some(picture) = picture {
                let left = x + area.width.saturating_sub(picture.size.width) / 2;
                let top = y + usize::from(area.height.saturating_sub(picture.size.height) / 2);
                let text: Arc<str> = label.into();
                for row in 0..picture.size.height {
                    let line = &mut strip.rows[top + usize::from(row)];
                    line.graphics.push(Fragment {
                        picture: picture.clone(),
                        x: left,
                        row,
                    });
                    line.copy.push(CopyText {
                        columns: usize::from(left)..usize::from(left + picture.size.width),
                        row,
                        height: picture.size.height,
                        baseline: picture.baseline,
                        text: text.clone(),
                    });
                }
            } else {
                let row = y + usize::from(area.height / 2);
                let left = x + area.width.saturating_sub(1) / 2;
                let line = &mut strip.rows[row];
                let before = usize::from(left);
                // Preserve earlier fallback icons on the same row.
                let mut content = line.line.to_string();
                let start = content
                    .char_indices()
                    .nth(before)
                    .map_or(content.len(), |(i, _)| i);
                let end = content[start..]
                    .char_indices()
                    .nth(1)
                    .map_or(content.len(), |(i, _)| start + i);
                content.replace_range(start..end, icons::IMAGE_OUTLINE);
                line.line = Line::from(Span::raw(content));
                // The icon is one terminal cell, but its UTF-8 byte range is larger.
                let text = line.text();
                line.media.push(Target {
                    range: start..start + icons::IMAGE_OUTLINE.len(),
                    source,
                });
                line.copy.push(CopyText {
                    columns: usize::from(left)..usize::from(left + 1),
                    row: 0,
                    height: 1,
                    baseline: 0,
                    text: label.into(),
                });
                line.decorations = std::iter::once(0..text.len()).collect();
            }
        }
        strip
    }
}
