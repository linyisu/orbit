use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use crate::error::{RuntimeError, failed};
use crate::verification::{VerificationEvidence, WorkspaceFingerprint};
use serde::{Deserialize, Serialize};

pub const MAX_OUTPUT: usize = 32 * 1024;
pub const MIB: usize = 1024 * 1024;
pub const MAX_RETAINED_OUTPUT_PER_STREAM: u64 = 16 * 1024 * 1024;
pub const MAX_RETAINED_OUTPUT_PER_SESSION: u64 = 128 * 1024 * 1024;
pub const TMP_SIZE_BYTES: u64 = 64 * 1024 * 1024;
pub const DEFAULT_MAX_MEMORY_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const DEFAULT_MAX_FILE_SIZE_BYTES: u64 = 64 * 1024 * 1024;
pub const DEFAULT_MAX_PROCESSES: u64 = 1024;
pub const DEFAULT_MAX_OPEN_FILES: u64 = 1024;

static OUTPUT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MountAccess {
    ReadOnly,
    ReadWrite,
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

pub fn is_reserved_system_path(path: &Path) -> bool {
    const EXACT_RESERVED: &[&str] = &["/", "/tmp", "/orbit-command.sh", "/opt", "/opt/orbit-cache"];
    const PREFIX_RESERVED: &[&str] = &[
        "/proc",
        "/dev",
        "/sys",
        "/etc",
        "/usr",
        "/bin",
        "/sbin",
        "/lib",
        "/lib64",
        "/workspace",
        "/scratch",
        "/orbit-source",
        "/opt/orbit-rust",
        "/tmp/home",
    ];

    for exact in EXACT_RESERVED {
        if path == Path::new(exact) {
            return true;
        }
    }

    for prefix in PREFIX_RESERVED {
        if path.starts_with(Path::new(prefix)) {
            return true;
        }
    }

    false
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct ProcessLimits {
    pub max_memory_bytes: u64,
    pub max_file_size_bytes: u64,
    pub max_processes: u64,
    pub max_open_files: u64,
}

impl ProcessLimits {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.max_memory_bytes < 16 * 1024 * 1024 {
            return Err("process max_memory_bytes must be at least 16 MiB");
        }
        if self.max_file_size_bytes < 1024 {
            return Err("process max_file_size_bytes must be at least 1 KiB");
        }
        if self.max_processes < 1 {
            return Err("process max_processes must be at least 1");
        }
        if self.max_open_files < 16 {
            return Err("process max_open_files must be at least 16");
        }
        Ok(())
    }
}

impl Default for ProcessLimits {
    fn default() -> Self {
        Self {
            max_memory_bytes: DEFAULT_MAX_MEMORY_BYTES,
            max_file_size_bytes: DEFAULT_MAX_FILE_SIZE_BYTES,
            max_processes: DEFAULT_MAX_PROCESSES,
            max_open_files: DEFAULT_MAX_OPEN_FILES,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProcessSpec {
    pub command: String,
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
}

impl ProcessSpec {
    pub fn new(command: impl Into<String>, timeout: Duration) -> Self {
        Self {
            command: command.into(),
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
        }
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

pub struct OutputLog {
    id: String,
    stdout: tokio::fs::File,
    stderr: tokio::fs::File,
}

impl OutputLog {
    pub async fn create(directory: &Path) -> Result<Self, RuntimeError> {
        use std::os::unix::fs::DirBuilderExt;

        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(directory)
            .map_err(failed)?;
        let metadata = std::fs::symlink_metadata(directory).map_err(failed)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(failed("command output directory must be a real directory"));
        }
        prune_output_directory(directory)?;
        let id = format!(
            "cmd-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(failed)?
                .as_nanos(),
            OUTPUT_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        );
        let stdout = create_output_file(&directory.join(format!("{id}.stdout")))?;
        let stderr = match create_output_file(&directory.join(format!("{id}.stderr"))) {
            Ok(file) => file,
            Err(error) => {
                let _ = std::fs::remove_file(directory.join(format!("{id}.stdout")));
                return Err(error);
            }
        };
        Ok(Self {
            id,
            stdout: tokio::fs::File::from_std(stdout),
            stderr: tokio::fs::File::from_std(stderr),
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn split(self) -> (String, tokio::fs::File, tokio::fs::File) {
        (self.id, self.stdout, self.stderr)
    }
}

fn create_output_file(path: &Path) -> Result<std::fs::File, RuntimeError> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(failed)
}

fn prune_output_directory(directory: &Path) -> Result<(), RuntimeError> {
    let mut entries = std::fs::read_dir(directory)
        .map_err(failed)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let metadata = std::fs::symlink_metadata(entry.path()).ok()?;
            (metadata.is_file() && entry.file_name().to_string_lossy().starts_with("cmd-"))
                .then_some((
                    entry.path(),
                    metadata.len(),
                    metadata
                        .modified()
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                ))
        })
        .collect::<Vec<_>>();
    let mut total = entries.iter().map(|(_, size, _)| *size).sum::<u64>();
    entries.sort_by_key(|(_, _, modified)| *modified);
    for (path, size, _) in entries {
        if total
            <= MAX_RETAINED_OUTPUT_PER_SESSION.saturating_sub(2 * MAX_RETAINED_OUTPUT_PER_STREAM)
        {
            break;
        }
        std::fs::remove_file(path).map_err(failed)?;
        total = total.saturating_sub(size);
    }
    Ok(())
}
