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
const COLUMN_GAP: u16 = 1;
const ROW_GAP: u16 = 1;

pub(in crate::tui) struct Strip {
    pub rows: Vec<LinkedLine>,
    pub areas: Vec<Rect>,
}

impl Strip {
    pub fn new(
        context: &Context,
        images: impl IntoIterator<Item = (Source, String)>,
        width: u16,
        size: Size,
    ) -> Self {
        Self::layout(context, images, width, size, COLUMN_GAP)
    }

    pub fn composer(
        context: &Context,
        images: impl IntoIterator<Item = (Source, String)>,
        width: u16,
    ) -> Self {
        Self::layout(context, images, width, COMPOSER_SIZE, COLUMN_GAP)
    }

    fn layout(
        context: &Context,
        images: impl IntoIterator<Item = (Source, String)>,
        width: u16,
        size: Size,
        column_gap: u16,
    ) -> Self {
        let mut strip = Self {
            rows: Vec::new(),
            areas: Vec::new(),
        };
        if width == 0 {
            return strip;
        }
        let bounds = Size::new(size.width.min(width), size.height);
        let mut next_x: u16 = 0;
        let mut next_y = 0;
        for (source, label) in images {
            let picture = context.cached_picture(&source, bounds.width, bounds.height);
            let item_width = picture.as_ref().map_or(bounds.width, |p| p.size.width);
            if next_x > 0 && next_x.saturating_add(item_width) > width {
                next_x = 0;
                next_y += usize::from(size.height + ROW_GAP);
            }
            let (x, y) = (next_x, next_y);
            next_x = next_x.saturating_add(item_width + column_gap);
            if y + usize::from(size.height) > usize::from(u16::MAX) {
                break;
            }
            strip.rows.resize_with(y + usize::from(size.height), || {
                LinkedLine::chrome(Line::from(" ".repeat(usize::from(width))))
            });
            let area = Rect::new(x, y as u16, item_width, size.height);
            strip.areas.push(area);
            if context.enabled() {
                // Every slice can independently become visible after scrolling.
                for row in &mut strip.rows[y..y + usize::from(area.height)] {
                    row.media_requests.push(Request {
                        source: source.clone(),
                        width: bounds.width,
                        height: bounds.height,
                    });
                }
            }
            if let Some(picture) = picture {
                let left = x;
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
