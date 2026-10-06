use super::*;

pub(super) fn open_subagent_at(state: &mut UiState, point: (u16, u16)) -> bool {
    let offset = if state.follow_bottom {
        state.max_scroll
    } else {
        state.max_scroll.saturating_sub(state.scroll)
    };
    let target = state
        .transcript_layout
        .link_at(state.selection.viewport, offset, point)
        .and_then(subagent_link_id)
        .map(str::to_owned);

    if let Some(id) = target {
        subagents::open(state, id);
        true
    } else {
        false
    }
}

pub(super) fn jump_footnote_at(state: &mut UiState, point: (u16, u16)) -> bool {
    let offset = if state.follow_bottom {
        state.max_scroll
    } else {
        state.max_scroll.saturating_sub(state.scroll)
    };

    if !state
        .transcript_layout
        .link_at(state.selection.viewport, offset, point)
        .is_some_and(markdown::footnotes::is_target)
    {
        return false;
    }

    if let Some(destination) =
        state
            .transcript_layout
            .footnote_at(state.selection.viewport, offset, point)
    {
        state.footnote_focus =
            state
                .transcript_layout
                .footnote_focus_at(state.selection.viewport, offset, point);
        state.scroll = state.max_scroll.saturating_sub(destination);
        state.follow_bottom = false;
    }

    state.selection.clear();
    state.link_hovered = None;
    true
}

pub(super) fn subagent_link_id(value: &str) -> Option<&str> {
    let id = value.strip_prefix("orbit://session/")?;
    (id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit())).then_some(id)
}

pub(super) use crate::platform::{browser_url, open_link_in_browser};
