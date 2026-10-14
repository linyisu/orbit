//! SQLite persistence and durable task, conversation and attachment ownership.

pub mod compactions;
pub mod context_archive;
mod database;
pub mod diagnostics;
mod entities;
mod event_log;
mod execution;
pub mod input_queue;
mod migrations;
pub mod project;
pub mod projects;
pub mod session_log;
mod storage_usage;
pub mod store;
pub mod transcript;
pub mod tui_sessions;
pub mod types;
mod usage;
mod workspace_history;

pub mod tui_windows;

pub mod subagents;

pub mod live_context;
pub mod workspace_requests;

pub mod plans;
pub mod user_input;

mod memories;
mod plugins;
pub mod tasks;

mod attachments;
#[cfg(test)]
mod tests;
