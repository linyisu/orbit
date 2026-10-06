//! Terminal presentation and interaction for Orbit application services.

pub(crate) use orbit_app::config::connections;
pub(crate) use orbit_app::context::{instructions, project_state};
pub(crate) use orbit_app::extensions::{marketplaces, skills};
pub(crate) use orbit_app::models::account_models;
#[cfg(test)]
pub(crate) use orbit_app::prepare_workspace;
pub(crate) use orbit_app::tools::verification;
pub(crate) use orbit_app::{
    PreparedWorkspace, WorkspaceOptions, WorkspaceResources, config, execution, platform,
    prepare_workspace_resources, session, workspace,
};

mod tui;
pub use tui::{run, run_workspace, setup_connection};
