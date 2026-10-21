mod actions;
mod admin;
mod changes;
mod completion;
pub mod configuration;
mod files;
mod input_queue;
mod library;
mod mcp_import;
mod profile;
pub mod project;
pub mod protocol;
mod questions;
pub mod remote;
mod resources;
mod review;
mod run;
mod server;
mod session;
pub mod setup;
mod skill_import;
mod subagents;
mod tasks;
mod tool_output;
mod usage;

pub use orbit_protocol::request_id;
pub use server::{runtime_directory, serve};
#[derive(Debug)]
pub struct ServiceOptions {
    pub workspace: String,
    pub daemon: bool,
    pub allow_mcp_start: bool,
}
