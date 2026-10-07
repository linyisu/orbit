use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum TabAction {
    Select(String),
    Close(String),
    New,
}

pub(super) fn tab_target(event: &Event, state: &UiState) -> Option<TabAction> {
    if state.dialog_open() {
        return None;
    }

    match event {
        Event::Key(key)
            if key.kind != KeyEventKind::Release && key.modifiers.contains(KeyModifiers::ALT) =>
        {
            let KeyCode::Char(digit @ '1'..='9') = key.code else {
                return None;
            };

            state
                .tabs
                .get(digit.to_digit(10)? as usize - 1)
                .map(|tab| TabAction::Select(tab.id.clone()))
        }

        Event::Key(key)
            if key.kind != KeyEventKind::Release
                && key.code == KeyCode::Tab
                && key.modifiers.contains(KeyModifiers::CONTROL) =>
        {
            let index = state.tabs.iter().position(|tab| tab.active)?;
            let count = state.tabs.len();
            let next = if key.modifiers.contains(KeyModifiers::SHIFT) {
                (index + count - 1) % count
            } else {
                (index + 1) % count
            };
            Some(TabAction::Select(state.tabs[next].id.clone()))
        }

        Event::Mouse(mouse)
            if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && state
                    .new_tab_area
                    .contains((mouse.column, mouse.row).into()) =>
        {
            Some(TabAction::New)
        }

        Event::Mouse(mouse)
            if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && state
                    .tab_close_areas
                    .iter()
                    .any(|(area, _)| area.contains((mouse.column, mouse.row).into())) =>
        {
            state
                .tab_close_areas
                .iter()
                .find(|(area, _)| area.contains((mouse.column, mouse.row).into()))
                .map(|(_, id)| TabAction::Close(id.clone()))
        }

        Event::Mouse(mouse) if mouse.kind == MouseEventKind::Down(MouseButton::Left) => state
            .tab_areas
            .iter()
            .find(|(area, _)| {
                mouse.column >= area.x
                    && mouse.column < area.right()
                    && mouse.row >= area.y
                    && mouse.row < area.bottom()
            })
            .map(|(_, id)| TabAction::Select(id.clone())),

        _ => None,
    }
}

// Route list motion before the main loop discards unused pointer movement.
pub(super) fn handle_list_hover(state: &mut UiState, event: &Event) -> bool {
    let Event::Mouse(mouse) = event else {
        return false;
    };
    if mouse.kind != MouseEventKind::Moved {
        return false;
    }
    let point = (mouse.column, mouse.row).into();

    if state.picker.is_some() || state.model_picker.is_some() || state.marketplace_dialog.is_some()
    {
        if state
            .dialog_rows
            .iter()
            .any(|(area, _)| area.contains(point))
        {
            dialogs::handle(state, event.clone());
            return true;
        }
    } else if state.approval.is_some()
        && state
            .approval_options
            .iter()
            .any(|(area, _)| area.contains(point))
    {
        approval::handle(state, event.clone());
        return true;
    }

    false
}

pub(super) fn update_tab_hover(event: &Event, state: &mut UiState) -> bool {
    let next = match event {
        Event::Mouse(mouse)
            if mouse.kind == MouseEventKind::Moved && !state.selection.dragging() =>
        {
            let mut probe = *mouse;
            probe.kind = MouseEventKind::Down(MouseButton::Left);
            tab_target(&Event::Mouse(probe), state)
        }

        Event::Resize(_, _) | Event::Key(_) | Event::Mouse(_) => None,
        _ => return false,
    };

    if state.tab_hover.as_ref().map(|(action, _)| action) == next.as_ref() {
        return false;
    }

    state.tab_hover = next.map(|action| (action, std::time::Instant::now()));
    state.tab_hover_tick = 0;

    true
}

pub(super) fn advance_tab_hover(state: &mut UiState) -> bool {
    let Some((action, since)) = &state.tab_hover else {
        return false;
    };
    let elapsed = since.elapsed().as_millis();
    let tick = match action {
        TabAction::Select(_) => elapsed / 120,
        _ => elapsed.min(240) / 40,
    };
    if tick == state.tab_hover_tick {
        return false;
    }
    state.tab_hover_tick = tick;
    true
}

pub(super) fn toggle_transcript_at(state: &mut UiState, point: (u16, u16)) -> bool {
    let offset = if state.follow_bottom {
        state.max_scroll
    } else {
        state.max_scroll.saturating_sub(state.scroll)
    };

    if let Some(source) = state
        .transcript_layout
        .media_at(state.selection.viewport, offset, point)
    {
        state.selection.clear();
        let mut context = state.media.clone();
        if !state.workspace.is_empty() {
            context.workspace = state.workspace.clone().into();
        }
        state.media_preview = Some(media::Preview::new(context, source));
        return true;
    }

    let offset = state.max_scroll.saturating_sub(state.scroll);
    if let Some((index, id)) =
        state
            .transcript_layout
            .html_disclosure_at(state.selection.viewport, offset, point)
    {
        state.selection.clear();
        state.follow_bottom = false;
        state.transcript_layout.toggle_html(index, id);
        state.transcript_layout.update_expanded(
            &state.transcript,
            &state.expanded,
            &state.expanded_tool_groups,
            state.selection.viewport.width,
        );
        state.max_scroll = state
            .transcript_layout
            .height()
            .saturating_sub(usize::from(state.selection.viewport.height));
        let target = offset.min(state.max_scroll);
        state.scroll = state.max_scroll.saturating_sub(target);
        return true;
    }

    if let Some(index) = state
        .transcript_layout
        .group_at(state.selection.viewport, offset, point)
    {
        toggle_transcript_group(state, index);
        return true;
    }

    let Some(index) = state.transcript_layout.disclosure_at(
        &state.transcript,
        state.selection.viewport,
        offset,
        point,
    ) else {
        return false;
    };

    toggle_transcript_item(state, index);

    true
}

pub(super) fn toggle_transcript_item(state: &mut UiState, index: usize) {
    if let Some(super::app::TranscriptItem::Tool(row)) = state.transcript.get(index)
        && matches!(
            row.name.as_str(),
            "enter_plan" | "update_plan" | "submit_plan"
        )
    {
        state.selection.clear();
        state.planning.open();
        return;
    }
    if let Some(super::app::TranscriptItem::Tool(row)) = state.transcript.get(index)
        && row.name == "ask_user"
        && state.user_input.open_tool(&row.id, &row.output)
    {
        // The click originates in transcript selection, which freezes the frame
        // on mouse-down. Release it before the main loop's frozen-frame shortcut.
        state.selection.clear();
        return;
    }

    toggle_transcript(state, index, false);
}

pub(super) fn toggle_transcript_group(state: &mut UiState, index: usize) {
    toggle_transcript(state, index, true);
}

pub(super) fn toggle_transcript(state: &mut UiState, index: usize, group: bool) {
    let offset = state.max_scroll.saturating_sub(state.scroll);
    let anchor = state.transcript_layout.anchor_at(offset);
    let expanded = if group {
        &mut state.expanded_tool_groups
    } else {
        &mut state.expanded
    };
    let collapsing = expanded.contains(&index);
    let start = state.transcript_layout.item_offset(index).unwrap_or(offset);
    let end = state
        .transcript_layout
        .item_offset(if group {
            state
                .transcript_layout
                .group_range(index)
                .map_or(index + 1, |range| range.end)
        } else {
            index + 1
        })
        .unwrap_or_else(|| state.transcript_layout.height());
    let viewport_in_collapsing_item = collapsing && (start..end).contains(&offset);
    if collapsing {
        expanded.remove(&index);
    } else {
        expanded.insert(index);
    }

    if group
        && collapsing
        && let Some(range) = state.transcript_layout.group_range(index)
    {
        state.expanded.retain(|index| !range.contains(index));
    }

    state.follow_bottom = false;
    state.selection.clear();
    state.transcript_layout.update_expanded(
        &state.transcript,
        &state.expanded,
        &state.expanded_tool_groups,
        state.selection.viewport.width,
    );
    state.max_scroll = state
        .transcript_layout
        .height()
        .saturating_sub(usize::from(state.selection.viewport.height));
    let target = if viewport_in_collapsing_item {
        state.transcript_layout.item_offset(index).unwrap_or(offset)
    } else {
        anchor
            .and_then(|anchor| state.transcript_layout.offset_for_anchor(anchor))
            .unwrap_or(offset)
    };

    state.scroll = state.max_scroll.saturating_sub(target);
}

pub(super) fn handle_input_mouse(state: &mut UiState, event: &Event) -> bool {
    if matches!(event, Event::Resize(_, _) | Event::Key(_) | Event::Paste(_)) {
        state.input_dragging = false;
    }
    if state.dialog_open() || state.approval.is_some() {
        state.input_dragging = false;
        return false;
    }

    let Event::Mouse(mouse) = event else {
        return false;
    };
    let area = state.input_area;
    let point = (mouse.column, mouse.row).into();
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) if area.contains(point) => {
            state.selection.clear();
            state.attachment_cursor = None;
            let index = state.input.position_at(
                (mouse.column - area.x) as usize,
                (mouse.row - area.y) as usize + state.input_scroll,
                state.input_width,
            );

            if mouse.modifiers.contains(KeyModifiers::SHIFT) {
                state.input.extend_selection(index);
            } else {
                state.input.begin_selection(index);
            }

            state.input_dragging = true;
            true
        }

        MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
            if state.input_dragging && area.width > 0 && area.height > 0 =>
        {
            let column = mouse.column.clamp(area.x, area.right() - 1) - area.x;
            let row = mouse.row.clamp(area.y, area.bottom() - 1) - area.y;
            let index = state.input.position_at(
                column as usize,
                row as usize + state.input_scroll,
                state.input_width,
            );

            state.input.extend_selection(index);
            state.input_dragging = !matches!(mouse.kind, MouseEventKind::Up(_));
            true
        }

        MouseEventKind::Down(_) => {
            state.input_dragging = false;
            state.input.clear_selection();
            false
        }

        _ => false,
    }
}
