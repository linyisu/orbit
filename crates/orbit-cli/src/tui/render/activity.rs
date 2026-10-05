use std::ops::Range;

use ratatui::{Frame, style::Color};

use crate::tui::{
    app::{ToolStatus, TranscriptItem, UiState},
    theme::{self, THEME},
};

pub(super) fn draw(frame: &mut Frame<'_>, state: &UiState, offset: usize) {
    if !state.running() || state.approval.is_some() {
        return;
    }
    let layout = &state.transcript_layout;
    for index in layout.visible_items(offset, usize::from(state.selection.viewport.height)) {
        match &state.transcript[index] {
            TranscriptItem::Tool(row) => {
                if row.status == ToolStatus::Running
                    && let Some(header) = layout.tool_header_range(index, false)
                {
                    paint(frame, state, offset, header, THEME.tool_running);
                }
                if let Some(header) = layout.tool_header_range(index, true)
                    && let Some(group) = layout.group_range(index)
                    && state.transcript[group].iter().any(|item| matches!(item, TranscriptItem::Tool(row) if row.status == ToolStatus::Running))
                {
                    paint(frame, state, offset, header, THEME.tool_running);
                }
            }
            TranscriptItem::Thinking(row) if !row.finished => {
                if let Some(start) = layout.item_offset(index) {
                    paint(
                        frame,
                        state,
                        offset,
                        start..start + 1,
                        THEME.thinking_header_default,
                    );
                }
            }
            _ => {}
        }
    }
}

fn paint(frame: &mut Frame<'_>, state: &UiState, offset: usize, rows: Range<usize>, base: Color) {
    let area = state.selection.viewport;
    let elapsed = state.active_elapsed();
    for row in rows.start.max(offset)..rows.end.min(offset + usize::from(area.height)) {
        let y = area.y + (row - offset) as u16;
        let columns: Vec<_> = (area.x..area.right())
            .filter(|&x| {
                let cell = &frame.buffer_mut()[(x, y)];
                cell.fg == base && !cell.symbol().trim().is_empty()
            })
            .collect();
        for (index, &x) in columns.iter().enumerate() {
            frame.buffer_mut()[(x, y)].set_fg(theme::activity_color(
                base,
                elapsed,
                index,
                columns.len(),
            ));
        }
    }
}
