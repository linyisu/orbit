//! Clear an overlay without leaving half a background grapheme across its edges.
use ratatui::{
    Frame,
    buffer::{CellDiffOption, CellWidth},
    layout::Rect,
    widgets::Clear,
};

use super::app::UiState;

/// Ordered like painting: input belongs to the foremost visible surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Layer {
    Info,
    Authentication,
    Connection,
    Image,
    Management,
    Plan,
    Questions,
    Restore,
    Btw,
    Subagent,
    Sessions,
    Models,
    Plugins,
    Help,
}

pub(super) fn active(state: &UiState) -> Option<Layer> {
    [
        (state.info_dialog.is_some(), Layer::Info),
        (state.ssh_prompt.is_some(), Layer::Authentication),
        (state.workspace_wizard.is_some(), Layer::Connection),
        (state.media_preview.is_some(), Layer::Image),
        (state.management.active.is_some(), Layer::Management),
        (state.planning.visible(), Layer::Plan),
        (state.user_input.visible(), Layer::Questions),
        (state.restore_picker.is_some(), Layer::Restore),
        (state.btw.visible(), Layer::Btw),
        (state.subagent_dialog.is_some(), Layer::Subagent),
        (state.picker.is_some(), Layer::Sessions),
        (state.model_picker.is_some(), Layer::Models),
        (state.marketplace_dialog.is_some(), Layer::Plugins),
        (state.show_help, Layer::Help),
    ]
    .into_iter()
    .find_map(|(visible, layer)| visible.then_some(layer))
}

/// Called before the frozen-frame shortcut as well as after event dispatch.
/// Opening through a command, transcript link or server event has one handoff.
pub(super) fn sync(state: &mut UiState) {
    state
        .management
        .approval(state.approval.is_some());
    let layer = active(state);
    if layer != state.overlay_layer {
        state.overlay_layer = layer;
        state.selection.clear();
        state.cursor_motion.clear();
        state.input_dragging = false;
        state.ime_cursor = None;
        invalidate(state);
    }
}

/// Reject queued pointer events until the visible surface has new geometry.
pub(super) fn invalidate(state: &mut UiState) {
    state.overlay_needs_layout = true;
    state.planning.invalidate_geometry();
    state.user_input.clear_geometry();
    if let Some(state) = &mut state.management.active {
        state.panel.invalidate_geometry();
    }
    state.dialog_rows.clear();
    state.pending_list_hover = None;
    if let Some(picker) = &mut state.restore_picker {
        picker.invalidate_geometry();
    }
}

pub(super) fn prepare(state: &mut UiState) {
    if active(state).is_none() && state.approval.is_none() {
        state.user_input.open_next();
    }
    sync(state);
}

pub(super) fn clear(frame: &mut Frame<'_>, area: Rect) {
    super::media::occlude(area);
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
