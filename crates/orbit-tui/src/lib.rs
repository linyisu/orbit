//! Terminal presentation and interaction for Orbit application services.

pub(crate) use orbit_service::config::connections;
pub(crate) use orbit_service::context::{instructions, project_state};
pub(crate) use orbit_service::extensions::{marketplaces, skills};
pub(crate) use orbit_service::models::account_models;
#[cfg(test)]
pub(crate) use orbit_service::prepare_workspace;
pub(crate) use orbit_service::tools::verification;
pub(crate) use orbit_service::{
    PreparedWorkspace, WorkspaceOptions, WorkspaceResources, config, execution, platform,
    prepare_workspace_resources, session,
};

mod tui;
pub use tui::{run, run_workspace, setup_connection};

pub(crate) use orbit_client as workspace;
