//! Compact tool disclosures over the shared conversation ordering.
use std::{borrow::Borrow, ops::Range};

use ratatui::{style::Style, text::Line};

use super::{
    app::{ToolRow, ToolStatus, TranscriptItem},
    markdown::LinkedLine,
    theme::THEME,
};

pub(super) fn source_start(items: &[TranscriptItem], index: usize) -> usize {
    let start = items[..index.min(items.len())]
        .iter()
        .rposition(|item| {
            matches!(
                item,
                TranscriptItem::User(_) | TranscriptItem::UserWithImages { .. }
            )
        })
        .map_or(0, |index| index + 1);
    items[start..]
        .iter()
        .position(|item| matches!(item, TranscriptItem::Tool(_)))
        .map_or(index, |offset| start + offset)
}

pub(super) fn start<T: Borrow<TranscriptItem>>(items: &[T], index: usize) -> usize {
    if !items
        .get(index)
        .is_some_and(|item| matches!(item.borrow(), TranscriptItem::Tool(_)))
    {
        return index;
    }
    let mut start = index;
    while start > 0 && matches!(items[start - 1].borrow(), TranscriptItem::Tool(_)) {
        start -= 1;
    }
    start
}

pub(super) fn range<T: Borrow<TranscriptItem>>(items: &[T], index: usize) -> Range<usize> {
    let mut end = index + 1;
    if items
        .get(index)
        .is_some_and(|item| matches!(item.borrow(), TranscriptItem::Tool(_)))
    {
        while end < items.len() && matches!(items[end].borrow(), TranscriptItem::Tool(_)) {
            end += 1;
        }
    }
    index..end
}

pub(super) fn header<T: Borrow<TranscriptItem>>(
    items: &[T],
    width: u16,
    expanded: bool,
) -> LinkedLine {
    let label =
        orbit_client::execution::batch_label(
            items
                .iter()
                .filter_map(|item| match item.borrow() {
                    TranscriptItem::Tool(row) => Some(row.shared_status()),
                    _ => None,
                }),
        );
    LinkedLine::plain(Line::styled(
        super::render::fit_label(
            &format!("  {} {label}", if expanded { "⌄" } else { "›" }),
            usize::from(width),
            false,
        ),
        Style::default().fg(THEME.tool_summary),
    ))
}

pub(super) fn section(row: &ToolRow, width: u16) -> Vec<LinkedLine> {
    let record = row.request_record();
    let marker = match row.status {
        ToolStatus::Running => "●",
        ToolStatus::Background => "◌",
        ToolStatus::Finished => "✓",
        ToolStatus::Failed => "×",
    };
    let text = format!("  {marker} {} · {}", row.name, record.summary());
    vec![LinkedLine::plain(Line::styled(
        super::render::fit_label(
            &super::render::safe_output(&text),
            usize::from(width),
            false,
        ),
        Style::default().fg(THEME.output_foreground),
    ))]
}

#[cfg(test)]
mod tests;
