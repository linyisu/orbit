//! Application services for configuration, sessions, tools and workspace execution.

pub mod chat;
pub mod config;
pub mod context;
pub mod execution;
pub mod extensions;
pub mod models;
pub mod platform;
pub mod session;
pub mod side_questions;
pub mod subagents;
pub mod tools;
pub mod workspace;

pub(crate) use context::{history, instructions, project_state};
pub(crate) use extensions::{marketplaces, skills};
pub(crate) use models::{account_models, model_metadata};
pub(crate) use tools as runtime;
pub(crate) use tools::verification;

pub use workspace::setup::{
    PreparedWorkspace, WorkspaceOptions, WorkspaceResources, prepare_workspace,
    prepare_workspace_resources,
};

#[cfg(test)]
mod config_tests;
