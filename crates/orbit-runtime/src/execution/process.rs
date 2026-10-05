use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use crate::error::{RuntimeError, failed};
use crate::verification::{VerificationEvidence, WorkspaceFingerprint};

pub use super::output::{
    MAX_OUTPUT, MAX_RETAINED_OUTPUT_PER_SESSION, MAX_RETAINED_OUTPUT_PER_STREAM, OutputLog,
};
pub use super::resources::{
    DEFAULT_MAX_FILE_SIZE_BYTES, DEFAULT_MAX_MEMORY_BYTES, DEFAULT_MAX_OPEN_FILES,
    DEFAULT_MAX_PROCESSES, MIB, ProcessLimits, TMP_SIZE_BYTES,
};

/// Find a system program without consulting the inherited PATH. Command policy
/// uses the same trusted locations as the sandbox launcher.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MountAccess {
    ReadOnly,
    ReadWrite,
}

impl MountAccess {
    pub fn is_read_only(&self) -> bool {
        matches!(self, Self::ReadOnly)
    }

    pub fn is_read_write(&self) -> bool {
        matches!(self, Self::ReadWrite)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MountSpec {
    pub source: PathBuf,
    pub target: PathBuf,
    pub access: MountAccess,
}

impl MountSpec {
    pub fn new(
        source: impl Into<PathBuf>,
        target: impl Into<PathBuf>,
        access: MountAccess,
    ) -> Self {
        Self {
            source: source.into(),
            target: target.into(),
            access,
        }
    }

    pub fn read_only(source: impl Into<PathBuf>, target: impl Into<PathBuf>) -> Self {
        Self::new(source, target, MountAccess::ReadOnly)
    }

    pub fn read_write(source: impl Into<PathBuf>, target: impl Into<PathBuf>) -> Self {
        Self::new(source, target, MountAccess::ReadWrite)
    }

    pub fn normalized_target(&self) -> Result<PathBuf, RuntimeError> {
        normalize_mount_target(&self.target)
    }

    pub fn validate(&self) -> Result<(), RuntimeError> {
        let normalized = self.normalized_target()?;

        if is_reserved_mount_target(&normalized) {
            return Err(failed(format!(
                "mount target '{}' is a reserved runtime path",
                self.target.display()
            )));
        }

        let canonical_source = self.source.canonicalize().map_err(|e| {
            failed(format!(
                "configured mount source '{}' must exist: {e}",
                self.source.display()
            ))
        })?;

        if !canonical_source.exists() {
            return Err(failed(format!(
                "configured mount source '{}' must exist",
                self.source.display()
            )));
        }

        Ok(())
    }
}

pub fn normalize_mount_target(target: &Path) -> Result<PathBuf, RuntimeError> {
    if !target.is_absolute() {
        return Err(failed(format!(
            "mount target '{}' must be an absolute path",
            target.display()
        )));
    }

    use std::os::unix::ffi::OsStrExt;

    for seg in target.as_os_str().as_bytes().split(|&b| b == b'/') {
        if seg == b"." || seg == b".." {
            return Err(failed(format!(
                "mount target '{}' must not contain '.' or '..' components",
                target.display()
            )));
        }
    }

    let mut normalized = PathBuf::from("/");
    let mut has_normal = false;

    for component in target.components() {
        match component {
            std::path::Component::RootDir => {}
            std::path::Component::Normal(c) => {
                has_normal = true;
                normalized.push(c);
            }

            std::path::Component::CurDir | std::path::Component::ParentDir => {
                return Err(failed(format!(
                    "mount target '{}' must not contain '.' or '..' components",
                    target.display()
                )));
            }

            std::path::Component::Prefix(_) => {
                return Err(failed(format!(
                    "mount target '{}' contains unsupported prefix component",
                    target.display()
                )));
            }
        }
    }

    if !has_normal {
        return Err(failed("mount target cannot be root '/'"));
    }

    Ok(normalized)
}

/// Sandbox targets reserve all of /tmp because a user mount could replace the
/// isolated temporary filesystem. This is intentionally stricter than host
/// capability paths, where an explicitly approved /tmp/file can be accessed.
pub fn is_reserved_mount_target(normalized_target: &Path) -> bool {
    const EXACT_RESERVED: &[&str] = &["/", "/opt", "/opt/orbit-cache"];
    const PREFIX_RESERVED: &[&str] = &[
        "/workspace",
        "/tmp",
        "/scratch",
        "/opt/orbit-rust",
        "/dev",
        "/proc",
        "/sys",
        "/etc",
        "/usr",
        "/bin",
        "/sbin",
        "/lib",
        "/lib64",
        "/orbit-command.sh",
        "/orbit-source",
    ];

    for exact in EXACT_RESERVED {
        if normalized_target == Path::new(exact) {
            return true;
        }
    }

    for prefix in PREFIX_RESERVED {
        if normalized_target.starts_with(Path::new(prefix)) {
            return true;
        }
    }

    false
}

/// Reserved host paths for external capabilities and mount sources. Unlike a
/// sandbox mount target, a descendant of host /tmp may be granted explicitly.
pub const RESERVED_CAPABILITY_PATHS: &[&str] =
    &["/", "/tmp", "/orbit-command.sh", "/opt", "/opt/orbit-cache"];

/// These host directories and all their descendants are reserved. Tool
/// descriptions use the same lists as policy enforcement.
pub const RESERVED_CAPABILITY_TREES: &[&str] = &[
    "/proc",
    "/dev",
    "/sys",
    "/workspace",
    "/scratch",
    "/orbit-source",
    "/opt/orbit-rust",
    "/tmp/home",
];

pub fn is_reserved_system_path(path: &Path) -> bool {
    RESERVED_CAPABILITY_PATHS
        .iter()
        .any(|exact| path == Path::new(exact))
        || RESERVED_CAPABILITY_TREES
            .iter()
            .any(|prefix| path.starts_with(Path::new(prefix)))
}

#[derive(Clone, Debug)]
pub struct ProcessSpec {
    pub command: String,
    pub cwd: Option<PathBuf>,
    pub env: std::collections::BTreeMap<String, String>,
    pub timeout: Duration,
    pub scratch: Option<PathBuf>,
    pub limits: ProcessLimits,
    pub read_only_workspace: bool,
    pub rust_toolchain: Option<PathBuf>,
    pub mounts: Vec<MountSpec>,
    pub workspace_files: usize,
    pub skipped_entries: usize,
    pub fingerprint: Option<WorkspaceFingerprint>,
    pub protected_paths: Vec<PathBuf>,
    pub opaque_runtime_roots: Vec<PathBuf>,
    pub unmask_git: bool,
    pub allow_git_config_write: bool,
}

impl ProcessSpec {
    pub fn new(command: impl Into<String>, timeout: Duration) -> Self {
        Self {
            command: command.into(),
            cwd: None,
            env: std::collections::BTreeMap::new(),
            timeout,
            scratch: None,
            limits: ProcessLimits::default(),
            read_only_workspace: true,
            rust_toolchain: None,
            mounts: Vec::new(),
            workspace_files: 0,
            skipped_entries: 0,
            fingerprint: None,
            protected_paths: Vec::new(),
            opaque_runtime_roots: Vec::new(),
            unmask_git: false,
            allow_git_config_write: false,
        }
    }

    pub fn with_cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn with_env(mut self, env: std::collections::BTreeMap<String, String>) -> Self {
        self.env = env;
        self
    }

    pub fn with_read_write_workspace(mut self, rw: bool) -> Self {
        self.read_only_workspace = !rw;
        self
    }
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
    pub sandbox: &'static str,
    pub scratch: Option<&'static str>,
    pub workspace_files: usize,
    pub skipped_entries: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_error: Option<String>,
    #[serde(skip)]
    pub verification: Option<VerificationEvidence>,
}
