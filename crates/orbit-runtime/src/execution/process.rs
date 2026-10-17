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

/// Own a new process group; dropping the guard kills descendants that stay in the group.
pub struct ProcessGroup(Option<u32>);

impl ProcessGroup {
    /// Spawn a group leader with kill-on-drop enabled and return its ownership guard.
    pub fn spawn(
        command: &mut tokio::process::Command,
    ) -> std::io::Result<(tokio::process::Child, Self)> {
        let child = command
            .process_group(0)
            .kill_on_drop(true)
            .spawn()?;
        let group = Self(child.id());
        Ok((child, group))
    }

    /// Ask the owned group to terminate, allowing server cleanup handlers to run.
    pub fn request_termination(&self) {
        self.signal(libc::SIGTERM);
    }

    /// Kill the owned group once, including when its leader has already exited.
    pub fn kill(&mut self) {
        self.signal(libc::SIGKILL);
        self.0 = None;
    }

    fn signal(&self, signal: i32) {
        if let Some(id) = self.0 {
            // SAFETY: spawn created this positive PID as a private process group leader.
            unsafe {
                libc::kill(-(id as i32), signal);
            }
        }
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
}

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
    pub signal: Option<i32>,
    pub core_dumped: bool,
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
