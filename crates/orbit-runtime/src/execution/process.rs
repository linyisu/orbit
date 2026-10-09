pub use super::output::{
    MAX_OUTPUT, MAX_RETAINED_OUTPUT_PER_RUN, MAX_RETAINED_OUTPUT_PER_STREAM, OutputLog,
};
pub use super::resources::{
    DEFAULT_MAX_FILE_SIZE_BYTES, DEFAULT_MAX_MEMORY_BYTES, DEFAULT_MAX_OPEN_FILES, MIB,
    ProcessLimits,
};
use crate::verification::VerificationEvidence;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Find a system program in trusted locations without consulting inherited PATH.
pub fn system_executable(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt;

    ["/usr/bin", "/bin"]
        .into_iter()
        .map(|directory| Path::new(directory).join(name))
        .filter_map(|path| path.canonicalize().ok())
        .find(|path| {
            path.starts_with("/usr/bin")
                && path.metadata().is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o022 == 0
                })
        })
}

pub const RESERVED_CAPABILITY_PATHS: &[&str] = &["/", "/tmp", "/opt"];

/// Host pseudo-filesystems and their descendants are not grantable to file tools.
pub const RESERVED_CAPABILITY_TREES: &[&str] = &["/proc", "/dev", "/sys"];

pub fn is_reserved_system_path(path: &Path) -> bool {
    RESERVED_CAPABILITY_PATHS
        .iter()
        .any(|exact| path == Path::new(exact))
        || RESERVED_CAPABILITY_TREES
            .iter()
            .any(|prefix| path.starts_with(Path::new(prefix)))
}

#[derive(Debug, Serialize)]
pub struct CommandResult {
    pub exit_code: Option<i32>,
    pub elapsed_ms: u64,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub cancelled: bool,
    pub output_truncated: bool,
    pub stdout_total_bytes: u64,
    pub stderr_total_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_id: Option<String>,
    pub retained_output_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retained_output_error: Option<String>,
    pub sandbox: &'static str,
    pub workspace_files: usize,
    pub skipped_entries: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_error: Option<String>,
    #[serde(skip)]
    pub verification: Option<VerificationEvidence>,
}
