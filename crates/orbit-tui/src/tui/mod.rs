pub mod app;
mod approval;
mod approval_diff;
mod attachments;
#[cfg(test)]
#[allow(dead_code)]
mod background;
mod btw;
#[cfg(test)]
mod changes;
mod code;
#[cfg(test)]
#[allow(dead_code)]
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
mod markdown;
mod media;
#[cfg(test)]
#[allow(dead_code)]
mod model_actions;
#[cfg(test)]
#[allow(dead_code)]
mod model_catalog;
mod navigation;
mod overlay;
mod permissions;
mod pointer;
mod project_label;
pub mod render;
#[cfg(test)]
#[allow(dead_code)]
mod replay;
mod restore;
#[cfg(test)]
#[allow(dead_code)]
mod run_view;
mod search;
mod selection;
#[cfg(test)]
#[allow(dead_code)]
mod session_actions;
#[cfg(test)]
#[allow(dead_code)]
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
#[cfg(test)]
#[allow(dead_code)]
mod workspace;
mod workspace_client;

use std::path::PathBuf;
#[cfg(test)]
use std::time::Duration;

#[cfg(test)]
use anyhow::Context;
use anyhow::Result;
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
#[cfg(test)]
use orbit_agent::{agent::AgentConfig, events::AgentEvent, tool::ToolRegistry};
use orbit_config::models::ModelOption;
#[cfg(test)]
use orbit_model::{
    client::ModelClient,
    types::{Attachment, ChatMessage},
};
#[cfg(test)]
use tokio::task::JoinHandle;
#[cfg(test)]
use tokio_util::sync::CancellationToken;

#[cfg(test)]
use self::app::TranscriptItem;
use self::app::UiState;
#[cfg(test)]
use crate::{
    execution::{RunTask, StartedRun},
    platform::editor,
    session::SessionState,
};
#[cfg(test)]
use command_actions::*;
use keyboard::*;
use links::*;
#[cfg(test)]
use model_actions::*;
use pointer::*;
#[cfg(test)]
use run_view::*;
#[cfg(test)]
use session_actions::*;

#[cfg(test)]
use attachments::queue_attachment;
#[cfg(test)]
use attachments::{attach_paths, focus_attachment_at};
use attachments::{pasted_image_paths, remove_focused_attachment};

pub use workspace_client::run as run_workspace;

#[cfg(test)]
type LocalTask = JoinHandle<Result<String, orbit_agent::tool::ToolError>>;

#[cfg(test)]
type MarketplaceTask = JoinHandle<Result<crate::marketplaces::Marketplace>>;

#[cfg(test)]
struct CommandContext<'a> {
    tools: &'a ToolRegistry,
    instructions: &'a crate::instructions::Instructions,
    skills: &'a crate::skills::Skills,
    mcp_statuses: &'a [orbit_tools::mcp::McpServerStatus],
    model_options: &'a [ModelOption],
}

#[cfg(test)]
type ModelRefreshOutput = (Vec<ModelOption>, Vec<String>);

#[cfg(test)]
type ModelRefreshTask = tokio_util::task::AbortOnDropHandle<Result<ModelRefreshOutput>>;

pub async fn setup_connection(options: &crate::ConnectionSettings) -> Result<bool> {
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
