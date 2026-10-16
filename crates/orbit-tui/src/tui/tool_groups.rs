//! Compact tool disclosures over the shared conversation ordering.
use std::{borrow::Borrow, ops::Range};

use ratatui::{style::Style, text::Line};

use super::{
    app::{ToolRow, ToolStatus, TranscriptItem},
    markdown::LinkedLine,
    theme::THEME,
};

pub(super) fn source_start(items: &[TranscriptItem], index: usize) -> usize {
    start(items, index)
}

pub(super) fn start<T: Borrow<TranscriptItem>>(items: &[T], index: usize) -> usize {
    range(items, index).start
}

pub(super) fn range<T: Borrow<TranscriptItem>>(items: &[T], index: usize) -> Range<usize> {
    orbit_client::conversation::layout::tool_range(items.len(), index, |i| {
        matches!(items[i].borrow(), TranscriptItem::Tool(_))
    })
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
