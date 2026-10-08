pub mod app;
mod approval;
mod attachments;
mod btw;
mod code;
pub mod commands;
mod completion;
mod components;
mod connect;
mod connect_custom;
mod connect_key;
mod connection_spec;
mod cursor;
mod dialogs;
mod directory;
mod frames;
mod icons;
pub mod input;
mod input_events;
mod keyboard;
mod languages;
mod links;
mod markdown;
mod media;
mod navigation;
mod overlay;
mod permissions;
mod pointer;
mod project_label;
pub mod render;
mod restore;
mod search;
mod selection;
mod subagents;
mod tabs;
pub mod terminal;
pub mod theme;
mod tool_display;
mod tool_groups;
mod transcript;
mod transcript_dialog;
mod wheel;
mod workspace_client;

use self::app::UiState;
use anyhow::Result;
use attachments::{pasted_image_paths, remove_focused_attachment};
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use futures_util::StreamExt;
use keyboard::*;
use links::*;
use orbit_config::models::ModelOption;
use pointer::*;
use std::path::PathBuf;

pub use workspace_client::run as run_workspace;

pub async fn setup_connection(options: &crate::ConnectionSettings) -> Result<bool> {
    let (_guard, mut terminal) = terminal::TerminalGuard::enter()?;
    let mut events = EventStream::new();
    Ok(connect::run(&mut terminal, &mut events, options)
        .await?
        .is_some())
}

/// Ask for an explicit model before creating the first workspace session.
pub async fn select_startup_model(models: Vec<ModelOption>) -> Result<Option<String>> {
    let (_guard, mut terminal) = terminal::TerminalGuard::enter()?;
    let mut events = EventStream::new();
    let mut state = app::UiState::new("");
    state.model_picker = Some(dialogs::ModelPicker::new(models, String::new(), None));
    while state.model_picker.is_some() {
        terminal::draw_synchronized(&mut terminal, |frame| {
            state.ime_cursor = None;
            render::draw(frame, &mut state);
            if let Some(cursor) = state.ime_cursor {
                frame.set_cursor_position(cursor);
            }
        })?;
        let Some(event) = events.next().await else {
            return Ok(None);
        };
        let event = event?;
        pointer::handle_list_hover(&mut state, &event);
        dialogs::handle(&mut state, event);
    }
    Ok(state.chosen_model)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod subagent_tests;

mod planning;
mod user_input;
