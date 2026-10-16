use super::*;

#[derive(Debug, PartialEq)]
pub(super) enum Action {
    None,
    Quit,
    Cancel,
    Steer(String),
    Redraw,
    AttachPaths(Vec<PathBuf>),
    PasteClipboard,
    CopyInput(String),
    Submit(String),
}

pub(super) fn keyboard_action(state: &mut UiState, event: Event, busy: bool) -> Action {
    if let Event::Mouse(mouse) = event {
        match mouse.kind {
            MouseEventKind::ScrollUp => state.scroll_up(wheel::LINES),
            MouseEventKind::ScrollDown => state.scroll_down(wheel::LINES),
            _ => {}
        }

        return Action::None;
    }

    if let Event::Paste(text) = event {
        state.attachment_cursor = None;

        if text.is_empty() {
            return Action::PasteClipboard;
        }
        if let Some(paths) = pasted_image_paths(&text, std::path::Path::new(&state.workspace)) {
            return Action::AttachPaths(paths);
        }

        state.input.insert(&text);
        state.menu_dismissed = false;
        return Action::None;
    }

    let Event::Key(key) = event else {
        return Action::None;
    };
    if key.kind == KeyEventKind::Release {
        return Action::None;
    }
    if key
        .modifiers
        .contains(KeyModifiers::ALT)
    {
        match key.code {
            KeyCode::Char('p') => {
                state.reveal_activity();
                return Action::None;
            }
            KeyCode::Char('o') => {
                if let Some(row) = state
                    .transcript
                    .iter()
                    .rev()
                    .find_map(|item| match item {
                        app::TranscriptItem::Tool(row) => Some(row),
                        _ => None,
                    })
                {
                    state.info_dialog = Some(dialogs::InfoDialog::tool(&row.record()));
                }
                return Action::None;
            }
            _ => {}
        }
    }

    let was_history = state.input.browsing_history();
    let control = key
        .modifiers
        .contains(KeyModifiers::CONTROL);
    let selecting = key
        .modifiers
        .contains(KeyModifiers::SHIFT);
    if control
        && key.code == KeyCode::Char('c')
        && let Some(text) = state.input.selected_text()
    {
        return Action::CopyInput(text.to_owned());
    }

    let motion = match (control, key.code) {
        (true, KeyCode::Left) => Some(input::Motion::WordLeft),
        (true, KeyCode::Right) => Some(input::Motion::WordRight),
        (true, KeyCode::Home) => Some(input::Motion::Start),
        (true, KeyCode::End) => Some(input::Motion::Finish),
        (false, KeyCode::Left) => Some(input::Motion::Left),
        (false, KeyCode::Right) => Some(input::Motion::Right),
        (false, KeyCode::Home) => Some(input::Motion::Home),
        (false, KeyCode::End) => Some(input::Motion::End),
        (false, KeyCode::Up) => Some(input::Motion::Up),
        (false, KeyCode::Down) => Some(input::Motion::Down),
        _ => None,
    };

    if (selecting || control)
        && let Some(motion) = motion
    {
        state.attachment_cursor = None;
        state
            .input
            .navigate(motion, state.input_width, selecting);
        return Action::None;
    }

    if control {
        match key.code {
            KeyCode::Char('c') if busy => return Action::Cancel,
            KeyCode::Char('c') if !state.input.text.is_empty() || !state.attachments.is_empty() => {
                state.input.clear();
                state.attachments.clear();
                state.attachment_cursor = None;
            }

            KeyCode::Char('c') => return Action::Quit,
            KeyCode::Char('d') if state.input.text.is_empty() && state.attachments.is_empty() => {
                return Action::Quit;
            }
            KeyCode::Char('d') => {
                if !remove_focused_attachment(state, false) {
                    state.input.delete();
                }
            }

            KeyCode::Char('j') => {
                if state.attachment_cursor.take().is_some() {
                    state.input.set_cursor(0);
                }

                state.input.insert("\n");
            }

            KeyCode::Char('a') => {
                state.attachment_cursor = None;
                state.input.select_all();
            }

            KeyCode::Up => {
                state.attachment_cursor = None;
                state.input.history_up();
                state.menu_dismissed = true;
                return Action::None;
            }

            KeyCode::Down => {
                state.attachment_cursor = None;
                state.input.history_down();
                state.menu_dismissed = true;
                return Action::None;
            }

            KeyCode::Char('e') => {
                state.attachment_cursor = None;
                state
                    .input
                    .navigate(input::Motion::End, state.input_width, false);
            }

            KeyCode::Char('u') => state.input.kill_to_home(),
            KeyCode::Char('k') => state.input.kill_to_end(),
            KeyCode::Char('l') => return Action::Redraw,
            KeyCode::Char('v') => return Action::PasteClipboard,

            KeyCode::Char('o') => {
                let offset = state
                    .max_scroll
                    .saturating_sub(state.scroll);

                if let Some(index) = state
                    .expanded
                    .iter()
                    .rev()
                    .copied()
                    .find(|index| {
                        state
                            .transcript_layout
                            .tool_offset(*index)
                            .is_some()
                    })
                {
                    toggle_transcript_item(state, index);
                } else if let Some(index) = state
                    .expanded_tool_groups
                    .iter()
                    .rev()
                    .copied()
                    .find(|index| {
                        state
                            .transcript_layout
                            .group_range(*index)
                            .is_some_and(|range| range.start == *index)
                    })
                {
                    toggle_transcript_group(state, index);
                } else {
                    let group = state
                        .transcript_layout
                        .visible_group(offset, state.selection.viewport.height as usize);
                    let item = state
                        .transcript_layout
                        .visible_collapsible(
                            &state.transcript,
                            offset,
                            state.selection.viewport.height as usize,
                        );

                    match (group, item) {
                        (Some(group), item) if item.is_none_or(|item| group >= item) => {
                            toggle_transcript_group(state, group)
                        }
                        (_, Some(item)) => toggle_transcript_item(state, item),
                        _ => {}
                    }
                }
            }

            _ => {}
        }

        if was_history && !state.input.browsing_history() {
            state.menu_dismissed = false;
            state.menu_index = 0;
            state.menu_offset = 0;
            state.menu_manual_scroll = false;
        }

        return Action::None;
    }

    if key.code == KeyCode::Esc {
        state.attachment_cursor = None;
        state.input.clear_selection();
        state.show_help = false;
        state.menu_dismissed = true;
        return Action::None;
    }

    if key.code == KeyCode::Char('v')
        && key
            .modifiers
            .contains(KeyModifiers::ALT)
    {
        return Action::PasteClipboard;
    }

    state.menu_manual_scroll = false;
    if key.code == KeyCode::Tab && state.menu_dismissed {
        state.menu_dismissed = false;
        state.menu_index = 0;
        state.menu_offset = 0;
        return Action::None;
    }

    let menu = state.menu();

    if !menu.is_empty() {
        state.menu_index = state.menu_index.min(menu.len() - 1);

        match key.code {
            KeyCode::Up => {
                state.menu_index = (state.menu_index + menu.len() - 1) % menu.len();
                return Action::None;
            }

            KeyCode::Down => {
                state.menu_index = (state.menu_index + 1) % menu.len();
                return Action::None;
            }

            KeyCode::Tab => {
                let candidate = &menu[state.menu_index];
                let suffix = if (candidate.continue_input
                    && !candidate.name.starts_with("/cd ")
                    && !candidate.name.ends_with("::"))
                    || matches!(
                        candidate.name.as_str(),
                        "/cd"
                            | "/ssh"
                            | "/wsl"
                            | "/mode"
                            | "/model"
                            | "/reasoning"
                            | "/skill"
                            | "/tasks"
                            | "/memory"
                            | "/diff"
                            | "/undo"
                    ) {
                    " "
                } else {
                    ""
                };

                state
                    .input
                    .set(format!("{}{suffix}", candidate.name));
                state.menu_index = 0;
                return Action::None;
            }

            KeyCode::Enter if key.modifiers.is_empty() => {
                let candidate = &menu[state.menu_index];
                if candidate.continue_input && candidate.name.ends_with("::") {
                    state.input.set(candidate.name.clone());
                    state.menu_index = 0;
                    return Action::None;
                }
                if candidate.continue_input
                    && !candidate.name.starts_with("/cd ")
                    && !candidate.name.ends_with("::")
                {
                    state
                        .input
                        .set(format!("{} ", candidate.name));
                    state.menu_index = 0;
                    return Action::None;
                }
                // An exact directory can be submitted even while its children are suggested.
                let exact_directory = state
                    .input
                    .text
                    .strip_prefix("/cd ")
                    .is_some_and(|argument| {
                        if state.connection_label.is_some() {
                            argument
                                .trim_end_matches(['\'', '"'])
                                .ends_with('/')
                                || menu.iter().any(|entry| {
                                    entry.name.trim_end_matches('/')
                                        == state.input.text.trim_end_matches('/')
                                })
                        } else {
                            directory::resolve(
                                argument,
                                std::path::Path::new(&state.workspace),
                                state.completion_home.as_deref(),
                            )
                            .is_ok()
                        }
                    });
                if !exact_directory || state.menu_index != 0 {
                    state.input.set(candidate.name.clone());
                }
            }

            _ => {}
        }
    }

    match key.code {
        KeyCode::Enter
            if key
                .modifiers
                .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
        {
            state.input.insert("\n")
        }

        KeyCode::Enter
            if state.input.cursor == state.input.text.len()
                && state.input.selection().is_none()
                && state.input.text.ends_with('\\') =>
        {
            state.input.backspace();
            state.input.insert("\n");
        }

        KeyCode::Enter if busy => {
            if let Some(guidance) = state.input.take() {
                return if commands::is_command_input(&guidance) {
                    Action::Submit(guidance)
                } else {
                    Action::Steer(guidance)
                };
            }
        }

        KeyCode::Enter => {
            state.menu_index = 0;
            state.menu_dismissed = false;
            state.attachment_cursor = None;
            return if let Some(prompt) = state.input.take() {
                Action::Submit(prompt)
            } else if !state.attachments.is_empty() {
                Action::Submit(String::new())
            } else {
                Action::None
            };
        }

        KeyCode::Char('?') if state.input.text.is_empty() && state.attachments.is_empty() => {
            state.selection = Default::default();
            state.show_help = true;
            state.help_scroll = 0;
        }

        KeyCode::Char(c)
            if !key
                .modifiers
                .contains(KeyModifiers::ALT) =>
        {
            if state.attachment_cursor.take().is_some() {
                state.input.set_cursor(0);
            }

            state.input.insert(&c.to_string());
            state.menu_index = 0;
            state.menu_dismissed = false;
        }

        KeyCode::Backspace => {
            if !remove_focused_attachment(state, true) {
                if state.input.cursor == 0
                    && state.input.selection().is_none()
                    && !state.attachments.is_empty()
                {
                    state.attachments.pop();
                } else {
                    state.input.backspace();
                }
            }

            state.menu_index = 0;
            state.menu_dismissed = false;
        }

        KeyCode::Delete => {
            if !remove_focused_attachment(state, false) {
                state.input.delete();
            }
        }

        KeyCode::Left => {
            if let Some(index) = &mut state.attachment_cursor {
                *index = index.saturating_sub(1);
            } else if state.input.cursor == 0
                && state.input.selection().is_none()
                && !state.attachments.is_empty()
            {
                state.attachment_cursor = Some(state.attachments.len() - 1);
            } else {
                state
                    .input
                    .navigate(input::Motion::Left, state.input_width, false);
            }
        }

        KeyCode::Right => {
            if let Some(index) = state.attachment_cursor {
                if index + 1 < state.attachments.len() {
                    state.attachment_cursor = Some(index + 1);
                } else {
                    state.attachment_cursor = None;
                    state.input.set_cursor(0);
                }
            } else {
                state
                    .input
                    .navigate(input::Motion::Right, state.input_width, false);
            }
        }

        KeyCode::Home => {
            state.attachment_cursor = None;
            state
                .input
                .navigate(input::Motion::Home, state.input_width, false);
        }

        KeyCode::End => {
            state.attachment_cursor = None;
            state
                .input
                .navigate(input::Motion::End, state.input_width, false);
        }

        KeyCode::Up => {
            if state.attachment_cursor.is_some()
                || (!state.attachments.is_empty()
                    && state.input.selection().is_none()
                    && state
                        .input
                        .visual_lines(state.input_width)
                        .1
                        .row
                        == 0)
            {
                attachments::move_attachment_vertical(state, false);
            } else {
                state
                    .input
                    .navigate(input::Motion::Up, state.input_width, false);
            }
        }

        KeyCode::Down => {
            if state.attachment_cursor.is_some() {
                attachments::move_attachment_vertical(state, true);
            } else {
                state
                    .input
                    .navigate(input::Motion::Down, state.input_width, false);
            }
        }

        KeyCode::PageUp => state.scroll_up(8),
        KeyCode::PageDown => state.scroll_down(8),
        _ => {}
    }

    if matches!(key.code, KeyCode::Up | KeyCode::Down)
        && (was_history || state.input.browsing_history())
    {
        state.menu_dismissed = true;
    } else if was_history && !state.input.browsing_history() {
        state.menu_dismissed = false;
        state.menu_index = 0;
    }

    Action::None
}
