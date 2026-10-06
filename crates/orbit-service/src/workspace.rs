mod actions;
mod admin;
mod changes;
mod completion;
pub mod configuration;
mod plugins;
mod profile;
pub mod project;
pub mod protocol;
mod questions;
mod run;
mod server;
mod session;
pub mod setup;

pub use orbit_protocol::request_id;
pub use server::{runtime_directory, serve};
#[derive(Debug)]
pub struct ServiceOptions {
    pub workspace: String,
    pub daemon: bool,
    pub allow_mcp_start: bool,
}
