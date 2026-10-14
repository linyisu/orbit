//! Terminal presentation and interaction for Orbit application services.

pub(crate) use orbit_config::config::{self, connections};
mod tui;
pub use tui::{run_workspace, select_startup_model, setup_connection};

pub struct ConnectionSettings {
    pub config: std::path::PathBuf,
    pub auth_directory: std::path::PathBuf,
}

pub(crate) use orbit_protocol::plugins as marketplaces;

pub(crate) use orbit_client as workspace;

pub(crate) use orbit_client::platform;
