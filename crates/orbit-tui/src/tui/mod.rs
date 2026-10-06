pub mod app;
mod approval;
mod approval_diff;
mod attachments;
mod background;
mod btw;
mod changes;
mod code;
mod command_actions;
pub mod commands;
mod completion;
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
mod local;
mod markdown;
mod media;
mod model_actions;
mod model_catalog;
mod navigation;
mod overlay;
mod permissions;
mod pointer;
mod project_label;
pub mod render;
mod replay;
mod restore;
mod run_view;
mod search;
mod selection;
mod session_actions;
mod session_sync;
mod subagents;
mod tabs;
pub mod terminal;
pub mod theme;
mod tool_display;
mod tool_groups;
mod transcript;
mod transcript_dialog;
mod wheel;
mod workspace;
mod workspace_client;

use std::{path::PathBuf, time::Duration};

use anyhow::{Context, Result};
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use orbit_agent::{agent::AgentConfig, events::AgentEvent, tool::ToolRegistry};
use orbit_app::models::ModelOption;
use orbit_model::{
    client::ModelClient,
    types::{Attachment, ChatMessage},
};
use tokio::{select, sync::mpsc, task::JoinHandle, time};
use tokio_util::sync::CancellationToken;

use self::{
    app::{TranscriptItem, UiState},
    terminal::TerminalGuard,
};
use crate::{
    execution::{RunTask, StartedRun},
    platform::editor,
    session::SessionState,
    workspace::setup::RequestBudget,
};
use command_actions::*;
use keyboard::*;
use links::*;
use model_actions::*;
use pointer::*;
use run_view::*;
use session_actions::*;

#[cfg(test)]
use attachments::queue_attachment;
use attachments::{
    attach_paths, focus_attachment_at, paste_clipboard, pasted_image_paths,
    remove_focused_attachment,
};

pub use local::run;
pub use workspace_client::run as run_workspace;

type LocalTask = JoinHandle<Result<String, orbit_agent::tool::ToolError>>;

type MarketplaceTask = JoinHandle<Result<crate::marketplaces::Marketplace>>;

type ResumeTask = JoinHandle<Result<(crate::session::PreparedResume, Vec<ChatMessage>)>>;

struct CommandContext<'a> {
    tools: &'a ToolRegistry,
    instructions: &'a crate::instructions::Instructions,
    skills: &'a crate::skills::Skills,
    mcp_statuses: &'a [orbit_tools::mcp::McpServerStatus],
    model_options: &'a [ModelOption],
}

type ModelRefreshOutput = (Vec<ModelOption>, Vec<String>);

type ModelRefreshTask = tokio_util::task::AbortOnDropHandle<Result<ModelRefreshOutput>>;

pub async fn setup_connection(options: &crate::WorkspaceOptions) -> Result<bool> {
    let (_guard, mut terminal) = terminal::TerminalGuard::enter()?;
    let mut events = EventStream::new();
    Ok(connect::run(&mut terminal, &mut events, options)
        .await?
        .is_some())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod subagent_tests;
