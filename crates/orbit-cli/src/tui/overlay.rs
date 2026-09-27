//! Clear an overlay without leaving half a background grapheme across its edges.
use ratatui::{
    Frame,
    buffer::{CellDiffOption, CellWidth},
    layout::Rect,
    widgets::Clear,
};

pub(super) fn clear(frame: &mut Frame<'_>, area: Rect) {
    let buffer = frame.buffer_mut();
    let bounds = *buffer.area();
    let area = area.intersection(bounds);

    if area.is_empty() {
        return;
    }

    for y in area.top()..area.bottom() {
        // Follow actual cell widths, including forced widths. Merely resetting
        // the cell at area.x leaves a preceding wide glyph alive, so the diff
        // skips the first overlay cell as that glyph's continuation.
        let mut x = bounds.left();

        while x < area.right() {
            let end = x
                .saturating_add(buffer[(x, y)].cell_width().max(1))
                .min(bounds.right());

            if (x < area.left() && end > area.left()) || (x < area.right() && end > area.right()) {
                for column in x..end {
                    buffer[(column, y)].reset();
                }
            }

            x = end;
        }
    }

    frame.render_widget(Clear, area);

    // A modal owns the covered terminal cells, including blank ones. Explicitly
    // repaint them instead of trusting the previous buffer: a terminal-side
    // artifact is otherwise invisible to the diff and can survive indefinitely.
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            frame.buffer_mut()[(x, y)].set_diff_option(CellDiffOption::AlwaysUpdate);
        }
    }
}
