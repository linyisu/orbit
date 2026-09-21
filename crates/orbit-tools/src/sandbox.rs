use std::{
    collections::VecDeque,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use orbit_agent::tool::ToolError;
use orbit_agent::verification::{FileFingerprint, VerificationEvidence, WorkspaceFingerprint};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
};
use tokio_util::sync::CancellationToken;

use crate::{
    capability::NetworkPolicy,
    path_security::ResolvedPath,
    policy::{BasePolicy, EffectivePolicy},
    workspace::{Workspace, WorkspaceError},
};

const MAX_OUTPUT: usize = 32 * 1024;
const MIB: usize = 1024 * 1024;
pub const MAX_RETAINED_OUTPUT_PER_STREAM: u64 = 16 * 1024 * 1024;
pub const MAX_RETAINED_OUTPUT_PER_SESSION: u64 = 128 * 1024 * 1024;
const TMP_SIZE_BYTES: u64 = 64 * 1024 * 1024;
const DEFAULT_MAX_MEMORY_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const DEFAULT_MAX_FILE_SIZE_BYTES: u64 = 64 * 1024 * 1024;
const DEFAULT_MAX_PROCESSES: u64 = 1024;
const DEFAULT_MAX_OPEN_FILES: u64 = 1024;
static OUTPUT_SEQUENCE: AtomicU64 = AtomicU64::new(0);

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
    #[serde(skip)]
    pub verification: Option<VerificationEvidence>,
}

/// Private, host-side files used for command output. They are deliberately not
/// mounted in Bubblewrap and are only addressed through an opaque identifier.
pub struct OutputLog {
    id: String,
    stdout: tokio::fs::File,
    stderr: tokio::fs::File,
}

impl OutputLog {
    pub async fn create(directory: &Path) -> Result<Self, ToolError> {
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

    fn split(self) -> (String, tokio::fs::File, tokio::fs::File) {
        (self.id, self.stdout, self.stderr)
    }
}

fn create_output_file(path: &Path) -> Result<std::fs::File, ToolError> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(failed)
}

fn prune_output_directory(directory: &Path) -> Result<(), ToolError> {
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

struct Snapshot {
    directory: Option<TempDir>,
    fingerprint: WorkspaceFingerprint,
    limits: crate::run_command::CommandLimits,
    files: usize,
    skipped: usize,
    bytes: usize,
    visited: usize,
}

impl Snapshot {
    #[cfg(test)]
    fn create(workspace: Workspace, cancellation: CancellationToken) -> Result<Self, ToolError> {
        Self::scan(workspace, cancellation, true)
    }
    fn scan(
        workspace: Workspace,
        cancellation: CancellationToken,
        copy: bool,
    ) -> Result<Self, ToolError> {
        Self::scan_configured(
            workspace,
            cancellation,
            copy,
            crate::run_command::CommandLimits::default(),
        )
    }
    fn scan_configured(
        workspace: Workspace,
        cancellation: CancellationToken,
        copy: bool,
        limits: crate::run_command::CommandLimits,
    ) -> Result<Self, ToolError> {
        limits.validate().map_err(failed)?;
        let directory = if copy {
            Some(
                tempfile::Builder::new()
                    .prefix("orbit-sandbox-")
                    .tempdir()
                    .map_err(failed)?,
            )
        } else {
            None
        };
        let mut snapshot = Self {
            fingerprint: WorkspaceFingerprint::default(),
            limits,
            directory,
            files: 0,
            skipped: 0,
            bytes: 0,
            visited: 0,
        };
        snapshot.copy_directory(&workspace, Path::new(""), &cancellation, 0)?;
        snapshot.fingerprint.directories.sort();
        snapshot.fingerprint.skipped_entries = snapshot.skipped;
        snapshot.fingerprint.digest = content_hash(
            &serde_json::to_vec(&(
                &snapshot.fingerprint.files,
                &snapshot.fingerprint.directories,
            ))
            .map_err(failed)?,
        );
        Ok(snapshot)
    }

    fn copy_directory(
        &mut self,
        workspace: &Workspace,
        relative: &Path,
        token: &CancellationToken,
        depth: usize,
    ) -> Result<(), ToolError> {
        if token.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        if depth > 64 {
            return Err(failed("workspace snapshot exceeds 64 directory levels"));
        }
        for name in workspace.directory_entries(relative).map_err(failed)? {
            self.visited += 1;
            if self.visited > self.limits.snapshot_entries {
                return Err(failed(format!(
                    "workspace snapshot exceeds {} entries",
                    self.limits.snapshot_entries
                )));
            }
            if token.is_cancelled() {
                return Err(ToolError::Cancelled);
            }
            if ["target", "node_modules", ".venv", "__pycache__"]
                .iter()
                .any(|excluded| name == *excluded)
            {
                self.skipped += 1;
                continue;
            }
            let path = relative.join(name);
            let destination = self.directory.as_ref().map(|d| d.path().join(&path));
            // All opens use the workspace's directory descriptors and protection
            // policy. No source symlinks or protected hard-link aliases are copied.
            match workspace.open_read(&path) {
                Ok(file) => {
                    use std::os::unix::fs::PermissionsExt;
                    let executable =
                        file.metadata().map_err(failed)?.permissions().mode() & 0o111 != 0;
                    let available = (self.limits.snapshot_mib * MIB).saturating_sub(self.bytes);
                    let mut content = Vec::new();
                    file.take(available as u64 + 1)
                        .read_to_end(&mut content)
                        .map_err(failed)?;
                    if content.len() > available {
                        return Err(failed(format!(
                            "workspace snapshot exceeds {} MiB; no command was started",
                            self.limits.snapshot_mib
                        )));
                    }
                    self.bytes += content.len();
                    self.fingerprint.files.insert(
                        path.to_str().ok_or_else(||failed("workspace snapshot paths must be valid UTF-8; no command was started"))?.to_owned(),
                        FileFingerprint {
                            sha256: content_hash(&content),
                            executable,
                        },
                    );
                    if let Some(destination) = destination {
                        std::fs::write(&destination, content).map_err(failed)?;
                        std::fs::set_permissions(
                            destination,
                            std::fs::Permissions::from_mode(if executable { 0o700 } else { 0o600 }),
                        )
                        .map_err(failed)?;
                    }
                    self.files += 1;
                }
                Err(WorkspaceError::NotFile) => {
                    // Only recurse when a no-follow directory open succeeds; FIFOs,
                    // sockets and devices are deliberately absent from the snapshot.
                    if workspace.directory_entries(&path).is_ok() {
                        self.fingerprint
                            .directories
                            .push(path.to_str().ok_or_else(||failed("workspace snapshot paths must be valid UTF-8; no command was started"))?.to_owned());
                        if let Some(destination) = destination {
                            std::fs::create_dir(destination).map_err(failed)?;
                        }
                        self.copy_directory(workspace, &path, token, depth + 1)?;
                    } else {
                        self.skipped += 1;
                    }
                }
                Err(WorkspaceError::ProtectedPath | WorkspaceError::Io(_)) => self.skipped += 1,
                Err(error) => return Err(failed(error)),
            }
        }
        Ok(())
    }
}

pub fn content_hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

pub async fn fingerprint(
    workspace: Workspace,
    cancellation: CancellationToken,
) -> Result<WorkspaceFingerprint, ToolError> {
    tokio::task::spawn_blocking(move || {
        Snapshot::scan(workspace, cancellation, false).map(|s| s.fingerprint)
    })
    .await
    .map_err(failed)?
}

pub async fn fingerprint_configured(
    workspace: Workspace,
    cancellation: CancellationToken,
    limits: crate::run_command::CommandLimits,
) -> Result<WorkspaceFingerprint, ToolError> {
    tokio::task::spawn_blocking(move || {
        Snapshot::scan_configured(workspace, cancellation, false, limits).map(|s| s.fingerprint)
    })
    .await
    .map_err(failed)?
}

#[derive(Clone, Debug)]
pub struct ProcessLimits {
    pub max_memory_bytes: u64,
    pub max_file_size_bytes: u64,
    pub max_processes: u64,
    pub max_open_files: u64,
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
}

impl ProcessSpec {
    pub fn new(command: impl Into<String>, timeout: Duration) -> Self {
        Self {
            command: command.into(),
            timeout,
            scratch: None,
            limits: ProcessLimits::default(),
            read_only_workspace: false,
        }
    }
}

/// Abstract execution backend for running sandboxed commands.
pub trait SandboxBackend: Send + Sync {
    fn run<'a>(
        &'a self,
        spec: ProcessSpec,
        policy: &'a EffectivePolicy,
        output_log: Option<OutputLog>,
        cancellation: CancellationToken,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<CommandResult, ToolError>> + Send + 'a>,
    >;
}

/// Linux Bubblewrap sandbox backend implementing capability-driven isolation.
pub struct BwrapSandboxBackend {
    bwrap_path: PathBuf,
    prlimit_path: PathBuf,
}

impl BwrapSandboxBackend {
    pub fn new() -> Result<Self, ToolError> {
        let bwrap_path = find_executable("bwrap").ok_or_else(|| {
            failed("sandbox unavailable: install Bubblewrap at /usr/bin/bwrap; refusing unsandboxed execution")
        })?;

        let prlimit_path = find_executable("prlimit").ok_or_else(|| {
            failed("sandbox unavailable: /usr/bin/prlimit is required for resource limits")
        })?;

        use std::os::unix::fs::MetadataExt;
        let metadata = bwrap_path.metadata().map_err(failed)?;
        if !metadata.is_file() || metadata.mode() & 0o6000 != 0 {
            return Err(failed(
                "sandbox unavailable: setuid/setgid Bubblewrap is not supported",
            ));
        }

        Ok(Self {
            bwrap_path,
            prlimit_path,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn build_command(
        &self,
        policy: &EffectivePolicy,
        script_path: &Path,
        spec: &ProcessSpec,
        custom_workspace: Option<&Path>,
        writable_snapshot_source: Option<&Path>,
        rust_toolchain: Option<&Path>,
        dependency_caches: &[crate::run_command::DependencyCache],
    ) -> Result<Command, ToolError> {
        let mut command = Command::new(&self.bwrap_path);

        command
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        configure_isolation(&mut command, policy.base.network);
        mount_system_paths(&mut command, policy.base.network);

        command
            .arg("--ro-bind")
            .arg(script_path)
            .arg("/orbit-command.sh");

        if let Some(source) = writable_snapshot_source {
            command.arg("--ro-bind").arg(source).arg("/orbit-source");
            command.args([
                "--size",
                &spec.limits.max_file_size_bytes.to_string(),
                "--tmpfs",
                "/workspace",
            ]);
        } else {
            let ws_target = custom_workspace.unwrap_or_else(|| policy.base.workspace.canonical());
            if spec.read_only_workspace {
                command.arg("--ro-bind").arg(ws_target).arg("/workspace");
            } else {
                command.arg("--bind").arg(ws_target).arg("/workspace");
            }
        }

        mount_scratch_and_tmp(&mut command, spec.scratch.as_deref())?;
        mount_toolchains_and_caches(&mut command, rust_toolchain, dependency_caches)?;
        mount_capabilities(&mut command, policy)?;

        configure_limits_and_entrypoint(
            &mut command,
            &self.prlimit_path,
            &spec.limits,
            rust_toolchain.is_some(),
            writable_snapshot_source.is_some(),
        );

        Ok(command)
    }
}

impl SandboxBackend for BwrapSandboxBackend {
    fn run<'a>(
        &'a self,
        spec: ProcessSpec,
        policy: &'a EffectivePolicy,
        output_log: Option<OutputLog>,
        cancellation: CancellationToken,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<CommandResult, ToolError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let mut script_file = tempfile::NamedTempFile::new().map_err(failed)?;
            script_file
                .write_all(spec.command.as_bytes())
                .map_err(failed)?;
            script_file.flush().map_err(failed)?;

            let child = self
                .build_command(policy, script_file.path(), &spec, None, None, None, &[])?
                .spawn()
                .map_err(failed)?;

            run_child_process(
                child,
                spec.timeout,
                cancellation,
                output_log,
                "bubblewrap; capability policy sandbox",
                spec.scratch.is_some(),
                0,
                0,
                None,
            )
            .await
        })
    }
}

fn configure_isolation(command: &mut Command, network: NetworkPolicy) {
    command.args([
        "--unshare-user",
        "--disable-userns",
        "--die-with-parent",
        "--new-session",
        "--cap-drop",
        "ALL",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--unshare-cgroup-try",
    ]);

    if network == NetworkPolicy::Deny {
        command.arg("--unshare-net");
    }
}

fn mount_system_paths(command: &mut Command, network: NetworkPolicy) {
    for path in ["/usr", "/bin", "/sbin", "/lib", "/lib64"] {
        if Path::new(path).exists() {
            command.args(["--ro-bind", path, path]);
        }
    }

    if Path::new("/etc/ld.so.cache").exists() {
        command.args(["--ro-bind", "/etc/ld.so.cache", "/etc/ld.so.cache"]);
    }

    if network == NetworkPolicy::Allow {
        for path in [
            "/etc/resolv.conf",
            "/etc/hosts",
            "/etc/ssl",
            "/etc/pki",
            "/etc/ca-certificates",
        ] {
            if Path::new(path).exists() {
                command.args(["--ro-bind", path, path]);
            }
        }
    }
}

fn mount_capabilities(command: &mut Command, policy: &EffectivePolicy) -> Result<(), ToolError> {
    for path in &policy.persistent_read_write {
        let canonical = path.canonical();
        if canonical.exists() {
            command.arg("--bind").arg(canonical).arg(canonical);
        }
    }

    for path in &policy.persistent_read {
        let canonical = path.canonical();
        if canonical.exists() {
            command.arg("--ro-bind").arg(canonical).arg(canonical);
        }
    }

    if let Ok(store) = policy.session_grants.read() {
        for grant in store.grants() {
            if grant.environment.is_local() {
                let canonical = grant.path.canonical();
                if canonical.exists() {
                    if grant.write {
                        command.arg("--bind").arg(canonical).arg(canonical);
                    } else if grant.read {
                        command.arg("--ro-bind").arg(canonical).arg(canonical);
                    }
                }
            }
        }
    }

    Ok(())
}

fn mount_toolchains_and_caches(
    command: &mut Command,
    rust_toolchain: Option<&Path>,
    dependency_caches: &[crate::run_command::DependencyCache],
) -> Result<(), ToolError> {
    if let Some(path) = rust_toolchain {
        let path = path.canonicalize().map_err(failed)?;
        if !path.join("bin/rustc").is_file()
            || !path.join("bin/cargo").is_file()
            || !path.join("lib/rustlib").is_dir()
        {
            return Err(failed(
                "configured Rust toolchain must contain bin/rustc, bin/cargo and lib/rustlib",
            ));
        }

        command.arg("--ro-bind").arg(path).arg("/opt/orbit-rust");
    }

    for cache in dependency_caches {
        let path = cache.path.canonicalize().map_err(failed)?;
        if !path.is_dir() {
            return Err(failed("configured dependency cache must be a directory"));
        }

        command
            .arg("--ro-bind")
            .arg(path)
            .arg(format!("/opt/orbit-cache/{}", cache.name));
    }

    Ok(())
}

fn mount_scratch_and_tmp(command: &mut Command, scratch: Option<&Path>) -> Result<(), ToolError> {
    if let Some(path) = scratch {
        use std::os::unix::fs::DirBuilderExt;

        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)
            .map_err(failed)?;

        if std::fs::symlink_metadata(path)
            .map_err(failed)?
            .file_type()
            .is_symlink()
        {
            return Err(failed("scratch directory must not be a symlink"));
        }

        command.arg("--bind").arg(path).arg("/scratch");
    }

    command.args([
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--size",
        &TMP_SIZE_BYTES.to_string(),
        "--tmpfs",
        "/tmp",
        "--dir",
        "/tmp/home",
        "--chdir",
        "/workspace",
    ]);

    Ok(())
}

fn configure_limits_and_entrypoint(
    command: &mut Command,
    prlimit: &Path,
    limits: &ProcessLimits,
    has_rust: bool,
    is_writable_tmpfs: bool,
) {
    command.args([
        "--clearenv",
        "--setenv",
        "PATH",
        if has_rust {
            "/opt/orbit-rust/bin:/usr/bin:/bin"
        } else {
            "/usr/bin:/bin"
        },
        "--setenv",
        "HOME",
        "/tmp/home",
        "--setenv",
        "TMPDIR",
        "/tmp",
        "--setenv",
        "LANG",
        "C.UTF-8",
        "--",
        prlimit.to_str().unwrap_or("/usr/bin/prlimit"),
        &format!(
            "--as={}:{}",
            limits.max_memory_bytes, limits.max_memory_bytes
        ),
        &format!(
            "--fsize={}:{}",
            limits.max_file_size_bytes, limits.max_file_size_bytes
        ),
        &format!(
            "--nofile={}:{}",
            limits.max_open_files, limits.max_open_files
        ),
        &format!("--nproc={}:{}", limits.max_processes, limits.max_processes),
        "--",
        "/bin/sh",
    ]);

    if is_writable_tmpfs {
        command.args([
            "-c",
            "cp -R /orbit-source/. /workspace/ && exec /bin/sh /orbit-command.sh",
        ]);
    } else {
        command.arg("/orbit-command.sh");
    }
}

fn find_executable(name: &str) -> Option<PathBuf> {
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path_var) {
            let candidate = dir.join(name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    for fallback in [
        format!("/usr/bin/{name}"),
        format!("/bin/{name}"),
        format!("/usr/local/bin/{name}"),
    ] {
        let path = PathBuf::from(fallback);
        if path.is_file() {
            return Some(path);
        }
    }

    None
}

struct Captured {
    bytes: Vec<u8>,
    total: u64,
    retained_truncated: bool,
}

async fn capture(
    mut pipe: impl AsyncRead + Unpin,
    mut retained: Option<tokio::fs::File>,
) -> std::io::Result<Captured> {
    let retaining = retained.is_some();
    let mut tail = VecDeque::with_capacity(MAX_OUTPUT);
    let mut total = 0u64;
    let mut buffer = [0; 4096];
    loop {
        let count = pipe.read(&mut buffer).await?;
        if count == 0 {
            let mut bytes: Vec<u8> = tail.into();
            if total > MAX_OUTPUT as u64 {
                // Do not expose a UTF-8 continuation byte as a spurious replacement
                // character when a valid wide character crosses the retained boundary.
                let partial = bytes.iter().take_while(|b| **b & 0xc0 == 0x80).count();
                bytes.drain(..partial);
            }
            if let Some(file) = retained.as_mut() {
                file.flush().await?;
            }
            return Ok(Captured {
                bytes,
                total,
                retained_truncated: retaining && total > MAX_RETAINED_OUTPUT_PER_STREAM,
            });
        }
        total = total.saturating_add(count as u64);
        if let Some(file) = retained.as_mut() {
            let prior = total.saturating_sub(count as u64);
            if prior < MAX_RETAINED_OUTPUT_PER_STREAM {
                let allowed = (MAX_RETAINED_OUTPUT_PER_STREAM - prior).min(count as u64) as usize;
                file.write_all(&buffer[..allowed]).await?;
            }
        }
        let overflow = (tail.len() + count).saturating_sub(MAX_OUTPUT);
        tail.drain(..overflow);
        tail.extend(&buffer[..count]);
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_child_process(
    mut child: tokio::process::Child,
    timeout: Duration,
    token: CancellationToken,
    output_log: Option<OutputLog>,
    sandbox_label: &'static str,
    persistent_scratch: bool,
    workspace_files: usize,
    skipped_entries: usize,
    verification: Option<VerificationEvidence>,
) -> Result<CommandResult, ToolError> {
    let started = std::time::Instant::now();
    let (output_id, stdout_log, stderr_log) = output_log
        .map(OutputLog::split)
        .map_or((None, None, None), |(id, stdout, stderr)| {
            (Some(id), Some(stdout), Some(stderr))
        });

    let stdout = capture(child.stdout.take().expect("piped stdout"), stdout_log);
    let stderr = capture(child.stderr.take().expect("piped stderr"), stderr_log);

    let completion = async {
        let mut timed_out = false;
        let mut cancelled = false;
        let status = tokio::select! {
            result = child.wait() => result,
            _ = token.cancelled() => {
                cancelled = true;
                child.start_kill()?;
                child.wait().await
            },
            _ = tokio::time::sleep(timeout) => {
                timed_out = true;
                child.start_kill()?;
                child.wait().await
            },
        }?;
        Ok::<_, std::io::Error>((status, timed_out, cancelled))
    };

    let (result, stdout, stderr) = tokio::join!(completion, stdout, stderr);
    let (status, timed_out, cancelled) = result.map_err(failed)?;
    let stdout = stdout.map_err(failed)?;
    let stderr = stderr.map_err(failed)?;

    let stdout_total_bytes = stdout.total;
    let stderr_total_bytes = stderr.total;
    let output_truncated = stdout.total > MAX_OUTPUT as u64 || stderr.total > MAX_OUTPUT as u64;
    let retained_output_truncated = stdout.retained_truncated || stderr.retained_truncated;
    let stdout = String::from_utf8_lossy(&stdout.bytes).into_owned();
    let stderr = String::from_utf8_lossy(&stderr.bytes).into_owned();

    Ok(CommandResult {
        exit_code: status.code(),
        elapsed_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        stdout,
        stderr,
        timed_out,
        cancelled,
        output_truncated,
        stdout_total_bytes,
        stderr_total_bytes,
        output_id,
        retained_output_truncated,
        sandbox: sandbox_label,
        scratch: if persistent_scratch {
            Some("/scratch (persistent session files; no persistent processes)")
        } else {
            None
        },
        workspace_files,
        skipped_entries,
        verification,
    })
}

pub async fn run(
    workspace: Workspace,
    script: String,
    timeout: Duration,
    token: CancellationToken,
) -> Result<CommandResult, ToolError> {
    run_with_options(workspace, script, timeout, token, false).await
}

pub async fn run_with_options(
    workspace: Workspace,
    script: String,
    timeout: Duration,
    token: CancellationToken,
    writable_snapshot: bool,
) -> Result<CommandResult, ToolError> {
    run_with_toolchain(workspace, script, timeout, token, writable_snapshot, None).await
}

pub async fn run_with_toolchain(
    workspace: Workspace,
    script: String,
    timeout: Duration,
    token: CancellationToken,
    writable_snapshot: bool,
    rust_toolchain: Option<&Path>,
) -> Result<CommandResult, ToolError> {
    run_with_resources(
        workspace,
        script,
        timeout,
        token,
        writable_snapshot,
        rust_toolchain,
        None,
    )
    .await
}

pub async fn run_with_resources(
    workspace: Workspace,
    script: String,
    timeout: Duration,
    token: CancellationToken,
    writable_snapshot: bool,
    rust_toolchain: Option<&Path>,
    scratch: Option<&Path>,
) -> Result<CommandResult, ToolError> {
    run_with_resources_and_output(
        workspace,
        script,
        timeout,
        token,
        writable_snapshot,
        rust_toolchain,
        scratch,
        None,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn run_with_resources_and_output(
    workspace: Workspace,
    script: String,
    timeout: Duration,
    token: CancellationToken,
    writable_snapshot: bool,
    rust_toolchain: Option<&Path>,
    scratch: Option<&Path>,
    output_log: Option<OutputLog>,
) -> Result<CommandResult, ToolError> {
    let limits = crate::run_command::CommandLimits {
        rust_toolchain: rust_toolchain.map(Path::to_path_buf),
        ..Default::default()
    };
    run_configured(
        workspace,
        script,
        timeout,
        token,
        writable_snapshot,
        &limits,
        scratch,
        output_log,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn run_configured(
    workspace: Workspace,
    script: String,
    timeout: Duration,
    token: CancellationToken,
    writable_snapshot: bool,
    limits: &crate::run_command::CommandLimits,
    scratch: Option<&Path>,
    output_log: Option<OutputLog>,
) -> Result<CommandResult, ToolError> {
    limits.validate().map_err(failed)?;
    let rust_toolchain = limits.rust_toolchain.as_deref();
    if token.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    let copy_token = token.clone();
    let snapshot_limits = limits.clone();
    let snapshot = tokio::task::spawn_blocking(move || {
        Snapshot::scan_configured(workspace, copy_token, true, snapshot_limits)
    })
    .await
    .map_err(failed)??;
    if token.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    // Keep the script outside the project and out of argv: large here-documents
    // otherwise hit execve's per-argument limit even after raising tool limits.
    let mut script_file = tempfile::NamedTempFile::new().map_err(failed)?;
    script_file.write_all(script.as_bytes()).map_err(failed)?;
    script_file.flush().map_err(failed)?;
    let backend = BwrapSandboxBackend::new()?;
    let snapshot_path = snapshot.directory.as_ref().expect("copied snapshot").path();

    let base = BasePolicy {
        workspace: ResolvedPath::resolve(snapshot_path, None).map_err(failed)?,
        network: NetworkPolicy::Deny,
    };
    let policy = EffectivePolicy::new(base);

    let mut spec = ProcessSpec::new(&script, timeout);
    spec.scratch = scratch.map(Path::to_path_buf);
    spec.limits.max_file_size_bytes = (limits.writable_snapshot_mib * MIB) as u64;
    spec.read_only_workspace = !writable_snapshot;

    let writable_source = if writable_snapshot {
        Some(snapshot_path)
    } else {
        None
    };

    let child = backend
        .build_command(
            &policy,
            script_file.path(),
            &spec,
            Some(snapshot_path),
            writable_source,
            rust_toolchain,
            &limits.dependency_caches,
        )?
        .spawn()
        .map_err(failed)?;

    let sandbox_label = if writable_snapshot {
        "bubblewrap; no network; disposable writable workspace snapshot; changes discarded"
    } else {
        "bubblewrap; no network; read-only filtered workspace snapshot"
    };

    let verification = Some(VerificationEvidence {
        input: snapshot.fingerprint,
        platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        isolation: "bubblewrap; no network; filtered source snapshot".into(),
        rust_toolchain: rust_toolchain.map(|p| p.display().to_string()),
        writable_snapshot,
        persistent_scratch: scratch.is_some(),
        dependency_caches: limits
            .dependency_caches
            .iter()
            .map(|c| format!("{}: {}", c.name, c.path.display()))
            .collect(),
    });

    run_child_process(
        child,
        timeout,
        token,
        output_log,
        sandbox_label,
        scratch.is_some(),
        snapshot.files,
        snapshot.skipped,
        verification,
    )
    .await
}

fn failed(error: impl std::fmt::Display) -> ToolError {
    ToolError::ExecutionFailed(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn capture_keeps_bounded_unicode_tail_and_counts_all_output() {
        let data = format!("{}FINAL", "中文".repeat(20_000));
        let result = capture(data.as_bytes(), None).await.unwrap();
        assert_eq!(result.total, data.len() as u64);
        assert!(result.bytes.len() <= MAX_OUTPUT);
        let tail = String::from_utf8(result.bytes).unwrap();
        assert!(tail.ends_with("FINAL"));
        assert!(data.ends_with(&tail));
        let short = capture(b"small".as_slice(), None).await.unwrap();
        assert_eq!(short.bytes, b"small");
        assert_eq!(short.total, 5);
    }

    #[tokio::test]
    async fn capture_retains_a_paginated_log_with_a_hard_limit() {
        let directory = tempfile::tempdir().unwrap();
        let log = OutputLog::create(directory.path()).await.unwrap();
        let id = log.id().to_owned();
        let (_, stdout, _) = log.split();
        let result = capture(b"first\nsecond\n".as_slice(), Some(stdout))
            .await
            .unwrap();
        assert!(!result.retained_truncated);
        assert_eq!(result.total, 13);
        assert_eq!(
            std::fs::read_to_string(directory.path().join(format!("{id}.stdout"))).unwrap(),
            "first\nsecond\n"
        );
    }
}

#[cfg(test)]
mod fingerprint_tests {
    use super::*;
    #[test]
    fn evidence_matches_copied_bytes_and_detects_content_names_modes_and_additions() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        std::fs::write(root.join("run.sh"), "#!/bin/sh\necho hi\n").unwrap();
        std::fs::set_permissions(root.join("run.sh"), std::fs::Permissions::from_mode(0o755))
            .unwrap();
        std::fs::write(root.join(".env"), "PRIVATE").unwrap();
        symlink(root.join(".env"), root.join("alias")).unwrap();
        let workspace = Workspace::new(root).unwrap();
        let copied = Snapshot::create(workspace.clone(), CancellationToken::new()).unwrap();
        let observed = Snapshot::scan(workspace.clone(), CancellationToken::new(), false).unwrap();
        assert_eq!(observed.fingerprint, copied.fingerprint);
        assert_eq!(copied.fingerprint.files.len(), 1);
        let copied_path = copied.directory.as_ref().unwrap().path().join("run.sh");
        assert_ne!(
            std::fs::metadata(&copied_path)
                .unwrap()
                .permissions()
                .mode()
                & 0o111,
            0
        );
        assert_eq!(
            content_hash(&std::fs::read(copied_path).unwrap()),
            copied.fingerprint.files["run.sh"].sha256
        );
        let original = observed.fingerprint.digest;
        std::fs::set_permissions(root.join("run.sh"), std::fs::Permissions::from_mode(0o600))
            .unwrap();
        assert_ne!(
            Snapshot::scan(workspace.clone(), CancellationToken::new(), false)
                .unwrap()
                .fingerprint
                .digest,
            original
        );
        std::fs::rename(root.join("run.sh"), root.join("other.sh")).unwrap();
        assert_ne!(
            Snapshot::scan(workspace.clone(), CancellationToken::new(), false)
                .unwrap()
                .fingerprint
                .digest,
            original
        );
        std::fs::write(root.join("new.rs"), "fn main(){}\n").unwrap();
        let new = Snapshot::scan(workspace, CancellationToken::new(), false).unwrap();
        assert_eq!(new.fingerprint.files.len(), 2);
        assert_ne!(new.fingerprint.digest, original);
    }
}
