pub mod workers;

pub use orbit_auth::directory;

/// Resolve native credential storage for service entry points.
pub fn auth_directory(explicit: Option<std::path::PathBuf>) -> anyhow::Result<std::path::PathBuf> {
    directory(
        explicit,
        orbit_config::directories::config(),
        orbit_config::directories::home(),
    )
}
