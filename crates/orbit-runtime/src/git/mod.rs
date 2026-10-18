pub mod discovery;
pub mod policy;
pub mod process;

pub use discovery::{GitRepoKind, GitRepositoryInfo};
pub use policy::{GitCommandAnalysis, GitOperationKind};

/// Native null device used to disable ambient Git configuration and hooks.
#[cfg(windows)]
pub const NULL_DEVICE: &str = "NUL";
#[cfg(unix)]
pub const NULL_DEVICE: &str = "/dev/null";

/// Minimal executable environment for extension acquisition after `env_clear`.
/// Unix retains its fixed system search path; Windows carries only native loader/temp paths
/// and the installed tool search path, never inherited Git configuration variables.
pub fn acquisition_environment() -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    #[cfg(unix)]
    {
        vec![("PATH".into(), "/usr/bin:/bin".into())]
    }
    #[cfg(windows)]
    {
        ["PATH", "SystemRoot", "WINDIR", "TEMP", "TMP"]
            .into_iter()
            .filter_map(|name| std::env::var_os(name).map(|value| (name.into(), value)))
            .collect()
    }
}
