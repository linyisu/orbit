//! Terminal presentation and interaction for Orbit application services.

pub(crate) use orbit_config::config::{self, connections};
pub(crate) use orbit_config::models::account_models;
#[cfg(test)]
pub(crate) use orbit_service::context::{instructions, project_state};
#[cfg(test)]
pub(crate) use orbit_service::extensions::{marketplaces, skills};
#[cfg(test)]
pub(crate) use orbit_service::prepare_workspace;
#[cfg(test)]
pub(crate) use orbit_service::tools::verification;
#[cfg(test)]
pub(crate) use orbit_service::{
    PreparedWorkspace, WorkspaceOptions, WorkspaceResources, execution,
    prepare_workspace_resources, session,
};

mod tui;
pub use tui::{run_workspace, select_startup_model, setup_connection};

pub struct ConnectionSettings {
    pub config: std::path::PathBuf,
    pub auth_directory: std::path::PathBuf,
}

#[cfg(not(test))]
pub(crate) use orbit_protocol::plugins as marketplaces;

pub(crate) use orbit_client as workspace;

pub(crate) use orbit_client::platform;
