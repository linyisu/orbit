use std::{
    collections::VecDeque,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use crate::error::{RuntimeError, failed};
use crate::verification::{FileFingerprint, VerificationEvidence, WorkspaceFingerprint};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
};
use tokio_util::sync::CancellationToken;

use crate::{
    capability::NetworkPolicy,
    execution::process::{
        CommandResult, MAX_OUTPUT, MAX_RETAINED_OUTPUT_PER_STREAM, MIB, MountAccess, MountSpec,
        OutputLog, ProcessLimits, ProcessSpec, TMP_SIZE_BYTES,
    },
    filesystem::workspace::{Workspace, WorkspaceError},
    policy::EffectivePolicy,
};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct WorkspaceScanLimits {
    #[serde(default = "default_max_scan_mib", alias = "snapshot_mib")]
    pub max_scan_mib: usize,
    #[serde(default = "default_max_scan_entries", alias = "snapshot_entries")]
    pub max_scan_entries: usize,
}

fn default_max_scan_mib() -> usize {
    64
}

fn default_max_scan_entries() -> usize {
    10_000
}

impl Default for WorkspaceScanLimits {
    fn default() -> Self {
        Self {
            max_scan_mib: 64,
            max_scan_entries: 10_000,
        }
    }
}

impl WorkspaceScanLimits {
    pub fn validate(&self) -> Result<(), &'static str> {
        if !(1..=1024).contains(&self.max_scan_mib)
            || !(1..=100_000).contains(&self.max_scan_entries)
        {
            return Err("max_scan_mib must be 1..1024 and max_scan_entries 1..100000");
        }
        Ok(())
    }
}

pub struct Snapshot {
    pub directory: Option<TempDir>,
    pub fingerprint: WorkspaceFingerprint,
    pub limits: WorkspaceScanLimits,
    pub files: usize,
    pub skipped: usize,
    pub bytes: usize,
    pub visited: usize,
}

impl Snapshot {
    #[cfg(test)]
    pub fn create(
        workspace: Workspace,
        cancellation: CancellationToken,
    ) -> Result<Self, RuntimeError> {
        Self::scan(workspace, cancellation, true)
    }

    pub fn scan(
        workspace: Workspace,
        cancellation: CancellationToken,
        copy: bool,
    ) -> Result<Self, RuntimeError> {
        Self::scan_configured(
            workspace,
            cancellation,
            copy,
            WorkspaceScanLimits::default(),
        )
    }

    pub fn scan_configured(
        workspace: Workspace,
        cancellation: CancellationToken,
        copy: bool,
        limits: WorkspaceScanLimits,
    ) -> Result<Self, RuntimeError> {
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
    ) -> Result<(), RuntimeError> {
        if token.is_cancelled() {
            return Err(RuntimeError::Cancelled);
        }
        if depth > 64 {
            return Err(failed("workspace snapshot exceeds 64 directory levels"));
        }
        for name in workspace.directory_entries(relative).map_err(failed)? {
            self.visited += 1;
            if self.visited > self.limits.max_scan_entries {
                return Err(failed(format!(
                    "workspace snapshot exceeds {} entries",
                    self.limits.max_scan_entries
                )));
            }
            if token.is_cancelled() {
                return Err(RuntimeError::Cancelled);
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
            match workspace.open_read(&path) {
                Ok(file) => {
                    use std::os::unix::fs::PermissionsExt;
                    let executable =
                        file.metadata().map_err(failed)?.permissions().mode() & 0o111 != 0;
                    let available = (self.limits.max_scan_mib * MIB).saturating_sub(self.bytes);
                    let mut content = Vec::new();
                    file.take(available as u64 + 1)
                        .read_to_end(&mut content)
                        .map_err(failed)?;
                    if content.len() > available {
                        return Err(failed(format!(
                            "workspace snapshot exceeds {} MiB; no command was started",
                            self.limits.max_scan_mib
                        )));
                    }
                    self.bytes += content.len();
                    self.fingerprint.files.insert(
                        path.to_str().ok_or_else(|| {
                            failed("workspace snapshot paths must be valid UTF-8; no command was started")
                        })?.to_owned(),
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
                    if workspace.directory_entries(&path).is_ok() {
                        self.fingerprint.directories.push(
                            path.to_str().ok_or_else(|| {
                                failed("workspace snapshot paths must be valid UTF-8; no command was started")
                            })?.to_owned(),
                        );
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
) -> Result<WorkspaceFingerprint, RuntimeError> {
    tokio::task::spawn_blocking(move || {
        Snapshot::scan(workspace, cancellation, false).map(|s| s.fingerprint)
    })
    .await
    .map_err(failed)?
}

pub async fn fingerprint_configured(
    workspace: Workspace,
    cancellation: CancellationToken,
    limits: WorkspaceScanLimits,
) -> Result<WorkspaceFingerprint, RuntimeError> {
    tokio::task::spawn_blocking(move || {
        Snapshot::scan_configured(workspace, cancellation, false, limits).map(|s| s.fingerprint)
    })
    .await
    .map_err(failed)?
}

pub async fn scan_workspace_summary(
    workspace: Workspace,
    cancellation: CancellationToken,
    limits: WorkspaceScanLimits,
) -> Result<(WorkspaceFingerprint, usize, usize), RuntimeError> {
    tokio::task::spawn_blocking(move || {
        Snapshot::scan_configured(workspace, cancellation, false, limits)
            .map(|s| (s.fingerprint, s.files, s.skipped))
    })
    .await
    .map_err(failed)?
}

pub trait SandboxBackend: Send + Sync {
    fn run<'a>(
        &'a self,
        spec: ProcessSpec,
        policy: &'a EffectivePolicy,
        output_log: Option<OutputLog>,
        cancellation: CancellationToken,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<CommandResult, RuntimeError>> + Send + 'a>,
    >;
}

pub struct BwrapSandboxBackend {
    bwrap_path: PathBuf,
    prlimit_path: PathBuf,
}

impl BwrapSandboxBackend {
    pub fn new() -> Result<Self, RuntimeError> {
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
        mounts: &[MountSpec],
    ) -> Result<Command, RuntimeError> {
        validate_mounts(mounts, policy)?;

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
        mount_toolchain_and_mounts(&mut command, rust_toolchain, mounts)?;
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
        Box<dyn std::future::Future<Output = Result<CommandResult, RuntimeError>> + Send + 'a>,
    > {
        Box::pin(async move {
            let mut script_file = tempfile::NamedTempFile::new().map_err(failed)?;
            script_file
                .write_all(spec.command.as_bytes())
                .map_err(failed)?;
            script_file.flush().map_err(failed)?;

            let child = self
                .build_command(
                    policy,
                    script_file.path(),
                    &spec,
                    None,
                    None,
                    spec.rust_toolchain.as_deref(),
                    &spec.mounts,
                )?
                .spawn()
                .map_err(failed)?;

            let network_desc = if policy.base.network.is_allowed() {
                "network allowed"
            } else {
                "no network"
            };

            let verification = spec.fingerprint.map(|fingerprint| VerificationEvidence {
                input: fingerprint,
                output: None,
                platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
                isolation: format!("bubblewrap; {network_desc}; capability policy sandbox"),
                rust_toolchain: spec
                    .rust_toolchain
                    .as_ref()
                    .map(|p| p.display().to_string()),
                writable_snapshot: false,
                persistent_scratch: spec.scratch.is_some(),
                mounts: spec
                    .mounts
                    .iter()
                    .map(|m| {
                        let target = m.normalized_target().unwrap_or_else(|_| m.target.clone());
                        let access = match m.access {
                            MountAccess::ReadOnly => "ro",
                            MountAccess::ReadWrite => "rw",
                        };
                        format!("{}: {access}", target.display())
                    })
                    .collect(),
            });

            run_child_process(
                child,
                spec.timeout,
                cancellation,
                output_log,
                "bubblewrap; capability policy sandbox",
                spec.scratch.is_some(),
                spec.workspace_files,
                spec.skipped_entries,
                verification,
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

fn mount_capabilities(command: &mut Command, policy: &EffectivePolicy) -> Result<(), RuntimeError> {
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

pub fn validate_mounts(mounts: &[MountSpec], policy: &EffectivePolicy) -> Result<(), RuntimeError> {
    let mut normalized_targets = Vec::with_capacity(mounts.len());
    for mount in mounts {
        mount.validate()?;
        normalized_targets.push(mount.normalized_target()?);
    }

    for i in 0..normalized_targets.len() {
        for j in (i + 1)..normalized_targets.len() {
            let ti = &normalized_targets[i];
            let tj = &normalized_targets[j];
            if ti == tj {
                return Err(failed(format!("duplicate mount target '{}'", ti.display())));
            }
            if ti.starts_with(tj) || tj.starts_with(ti) {
                return Err(failed(format!(
                    "conflicting or overlapping mount targets: '{}' and '{}'",
                    ti.display(),
                    tj.display()
                )));
            }
        }
    }

    let mut capability_paths: Vec<PathBuf> = Vec::new();
    for path in &policy.persistent_read_write {
        capability_paths.push(path.canonical().to_path_buf());
    }
    for path in &policy.persistent_read {
        capability_paths.push(path.canonical().to_path_buf());
    }
    if let Ok(store) = policy.session_grants.read() {
        for grant in store.grants() {
            if grant.environment.is_local() {
                capability_paths.push(grant.path.canonical().to_path_buf());
            }
        }
    }

    for target in &normalized_targets {
        for cap_path in &capability_paths {
            if target == cap_path || target.starts_with(cap_path) || cap_path.starts_with(target) {
                return Err(failed(format!(
                    "mount target '{}' conflicts or overlaps with active capability grant '{}'",
                    target.display(),
                    cap_path.display()
                )));
            }
        }
    }

    Ok(())
}

fn mount_toolchain_and_mounts(
    command: &mut Command,
    rust_toolchain: Option<&Path>,
    mounts: &[MountSpec],
) -> Result<(), RuntimeError> {
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

    for mount in mounts {
        let path = mount.source.canonicalize().map_err(failed)?;
        if !path.exists() {
            return Err(failed(format!(
                "configured mount source '{}' must exist",
                mount.source.display()
            )));
        }
        let target = mount.normalized_target()?;

        match mount.access {
            MountAccess::ReadOnly => {
                command.arg("--ro-bind").arg(path).arg(&target);
            }
            MountAccess::ReadWrite => {
                command.arg("--bind").arg(path).arg(&target);
            }
        }
    }

    Ok(())
}

fn mount_scratch_and_tmp(
    command: &mut Command,
    scratch: Option<&Path>,
) -> Result<(), RuntimeError> {
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
) -> Result<CommandResult, RuntimeError> {
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

#[allow(clippy::too_many_arguments)]
pub async fn run_configured(
    workspace: Workspace,
    script: String,
    timeout: Duration,
    token: CancellationToken,
    writable_snapshot: bool,
    scan_limits: &WorkspaceScanLimits,
    process_limits: &ProcessLimits,
    rust_toolchain: Option<&Path>,
    mounts: &[MountSpec],
    scratch: Option<&Path>,
    output_log: Option<OutputLog>,
) -> Result<CommandResult, RuntimeError> {
    scan_limits.validate().map_err(failed)?;
    process_limits.validate().map_err(failed)?;
    let policy_engine = workspace.policy().clone();
    let policy = policy_engine.effective_policy();
    validate_mounts(mounts, policy)?;

    if token.is_cancelled() {
        return Err(RuntimeError::Cancelled);
    }
    let copy_token = token.clone();
    let snapshot_limits = scan_limits.clone();
    let scan_workspace = workspace.clone();
    let snapshot = tokio::task::spawn_blocking(move || {
        Snapshot::scan_configured(scan_workspace, copy_token, true, snapshot_limits)
    })
    .await
    .map_err(failed)??;
    if token.is_cancelled() {
        return Err(RuntimeError::Cancelled);
    }

    let mut script_file = tempfile::NamedTempFile::new().map_err(failed)?;
    script_file.write_all(script.as_bytes()).map_err(failed)?;
    script_file.flush().map_err(failed)?;
    let backend = BwrapSandboxBackend::new()?;
    let snapshot_path = snapshot.directory.as_ref().expect("copied snapshot").path();

    let mut spec = ProcessSpec::new(&script, timeout);
    spec.scratch = scratch.map(Path::to_path_buf);
    spec.limits = process_limits.clone();
    spec.read_only_workspace = !writable_snapshot;

    let writable_source = if writable_snapshot {
        Some(snapshot_path)
    } else {
        None
    };

    let child = backend
        .build_command(
            policy,
            script_file.path(),
            &spec,
            Some(snapshot_path),
            writable_source,
            rust_toolchain,
            mounts,
        )?
        .spawn()
        .map_err(failed)?;

    let sandbox_label = if writable_snapshot {
        "bubblewrap; disposable writable workspace snapshot; changes discarded"
    } else {
        "bubblewrap; read-only filtered workspace snapshot"
    };

    let network_desc = if policy.base.network.is_allowed() {
        "network allowed"
    } else {
        "no network"
    };

    let verification = Some(VerificationEvidence {
        input: snapshot.fingerprint,
        output: None,
        platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        isolation: format!("bubblewrap; {network_desc}; filtered source snapshot"),
        rust_toolchain: rust_toolchain.map(|p| p.display().to_string()),
        writable_snapshot,
        persistent_scratch: scratch.is_some(),
        mounts: mounts
            .iter()
            .map(|m| {
                let target = m.normalized_target().unwrap_or_else(|_| m.target.clone());
                let access = match m.access {
                    MountAccess::ReadOnly => "ro",
                    MountAccess::ReadWrite => "rw",
                };
                format!("{}: {access}", target.display())
            })
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

pub async fn run(
    workspace: Workspace,
    script: String,
    timeout: Duration,
    token: CancellationToken,
) -> Result<CommandResult, RuntimeError> {
    run_with_options(workspace, script, timeout, token, false).await
}

pub async fn run_with_options(
    workspace: Workspace,
    script: String,
    timeout: Duration,
    token: CancellationToken,
    writable_snapshot: bool,
) -> Result<CommandResult, RuntimeError> {
    run_with_toolchain(workspace, script, timeout, token, writable_snapshot, None).await
}

pub async fn run_with_toolchain(
    workspace: Workspace,
    script: String,
    timeout: Duration,
    token: CancellationToken,
    writable_snapshot: bool,
    rust_toolchain: Option<&Path>,
) -> Result<CommandResult, RuntimeError> {
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
) -> Result<CommandResult, RuntimeError> {
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
) -> Result<CommandResult, RuntimeError> {
    let scan_limits = WorkspaceScanLimits::default();
    let process_limits = ProcessLimits::default();
    run_configured(
        workspace,
        script,
        timeout,
        token,
        writable_snapshot,
        &scan_limits,
        &process_limits,
        rust_toolchain,
        &[],
        scratch,
        output_log,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::process::{is_reserved_mount_target, normalize_mount_target};

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

    #[test]
    fn test_normalize_mount_target() {
        assert_eq!(
            normalize_mount_target(Path::new("/opt/orbit-cache/packages")).unwrap(),
            PathBuf::from("/opt/orbit-cache/packages")
        );
        assert_eq!(
            normalize_mount_target(Path::new("/opt/orbit-cache/packages/")).unwrap(),
            PathBuf::from("/opt/orbit-cache/packages")
        );
        assert_eq!(
            normalize_mount_target(Path::new("/opt//orbit-cache///packages")).unwrap(),
            PathBuf::from("/opt/orbit-cache/packages")
        );
        assert!(normalize_mount_target(Path::new("relative/path")).is_err());
        assert!(normalize_mount_target(Path::new("/opt/../etc")).is_err());
        assert!(normalize_mount_target(Path::new("/opt/./cache")).is_err());
        assert!(normalize_mount_target(Path::new("/")).is_err());
        assert!(normalize_mount_target(Path::new("///")).is_err());
    }

    #[test]
    fn test_is_reserved_mount_target() {
        // Exact reserved
        assert!(is_reserved_mount_target(Path::new("/")));
        assert!(is_reserved_mount_target(Path::new("/opt")));
        assert!(is_reserved_mount_target(Path::new("/opt/orbit-cache")));

        // Prefix reserved
        assert!(is_reserved_mount_target(Path::new("/workspace")));
        assert!(is_reserved_mount_target(Path::new("/workspace/src")));
        assert!(is_reserved_mount_target(Path::new("/tmp")));
        assert!(is_reserved_mount_target(Path::new("/tmp/scratch")));
        assert!(is_reserved_mount_target(Path::new("/scratch")));
        assert!(is_reserved_mount_target(Path::new("/scratch/dir")));
        assert!(is_reserved_mount_target(Path::new("/opt/orbit-rust")));
        assert!(is_reserved_mount_target(Path::new("/opt/orbit-rust/bin")));
        assert!(is_reserved_mount_target(Path::new("/dev")));
        assert!(is_reserved_mount_target(Path::new("/dev/null")));
        assert!(is_reserved_mount_target(Path::new("/proc")));
        assert!(is_reserved_mount_target(Path::new("/sys")));
        assert!(is_reserved_mount_target(Path::new("/etc")));
        assert!(is_reserved_mount_target(Path::new("/etc/passwd")));
        assert!(is_reserved_mount_target(Path::new("/usr")));
        assert!(is_reserved_mount_target(Path::new("/bin")));
        assert!(is_reserved_mount_target(Path::new("/sbin")));
        assert!(is_reserved_mount_target(Path::new("/lib")));
        assert!(is_reserved_mount_target(Path::new("/lib64")));
        assert!(is_reserved_mount_target(Path::new("/orbit-command.sh")));
        assert!(is_reserved_mount_target(Path::new("/orbit-source")));

        // Allowed targets
        assert!(!is_reserved_mount_target(Path::new(
            "/opt/orbit-cache/cargo"
        )));
        assert!(!is_reserved_mount_target(Path::new(
            "/opt/orbit-cache/packages"
        )));
        assert!(!is_reserved_mount_target(Path::new(
            "/opt/orbit-cache/nested/dir"
        )));
        assert!(!is_reserved_mount_target(Path::new("/mnt/data")));
        assert!(!is_reserved_mount_target(Path::new("/data")));
        assert!(!is_reserved_mount_target(Path::new("/var/cache")));
    }

    #[test]
    fn test_mount_spec_validation() {
        let dir = tempfile::tempdir().unwrap();
        let valid_mount = MountSpec::read_only(dir.path(), "/opt/orbit-cache/packages");
        assert!(valid_mount.validate().is_ok());

        // Non-existent source
        let bad_source = MountSpec::read_only(
            dir.path().join("does_not_exist"),
            "/opt/orbit-cache/packages",
        );
        assert!(bad_source.validate().is_err());

        // Reserved target
        let reserved_mount = MountSpec::read_only(dir.path(), "/workspace");
        assert!(reserved_mount.validate().is_err());

        // Relative target
        let relative_mount = MountSpec::read_only(dir.path(), "opt/orbit-cache/packages");
        assert!(relative_mount.validate().is_err());
    }

    #[test]
    fn test_validate_mounts_duplicate_and_overlapping() {
        let dir1 = tempfile::tempdir().unwrap();
        let dir2 = tempfile::tempdir().unwrap();
        let policy = EffectivePolicy::new(crate::policy::BasePolicy::new(
            crate::filesystem::ResolvedPath::from_canonical(dir1.path()),
        ));

        // Duplicate targets
        let duplicates = vec![
            MountSpec::read_only(dir1.path(), "/mnt/data"),
            MountSpec::read_write(dir2.path(), "/mnt/data"),
        ];
        let err = validate_mounts(&duplicates, &policy).unwrap_err();
        assert!(err.to_string().contains("duplicate mount target"));

        // Overlapping targets
        let overlapping = vec![
            MountSpec::read_only(dir1.path(), "/mnt/data"),
            MountSpec::read_write(dir2.path(), "/mnt/data/subdir"),
        ];
        let err = validate_mounts(&overlapping, &policy).unwrap_err();
        assert!(
            err.to_string()
                .contains("conflicting or overlapping mount targets")
        );

        // Non-overlapping targets
        let non_overlapping = vec![
            MountSpec::read_only(dir1.path(), "/mnt/data1"),
            MountSpec::read_write(dir2.path(), "/mnt/data2"),
        ];
        assert!(validate_mounts(&non_overlapping, &policy).is_ok());
    }

    #[test]
    fn test_validate_mounts_capability_collision() {
        let ws_dir = tempfile::tempdir().unwrap();
        let cap_canonical = PathBuf::from("/var/data/shared");
        let mount_src = tempfile::tempdir().unwrap();

        let mut policy = EffectivePolicy::new(crate::policy::BasePolicy::new(
            crate::filesystem::ResolvedPath::from_canonical(ws_dir.path()),
        ));
        policy
            .persistent_read
            .push(crate::filesystem::ResolvedPath::from_canonical(
                &cap_canonical,
            ));

        // Mount target collides with capability path
        let collides = vec![MountSpec::read_only(mount_src.path(), &cap_canonical)];
        let err = validate_mounts(&collides, &policy).unwrap_err();
        assert!(
            err.to_string()
                .contains("conflicts or overlaps with active capability grant")
        );

        // Mount target is subpath of capability path
        let subpath = vec![MountSpec::read_only(
            mount_src.path(),
            cap_canonical.join("sub"),
        )];
        let err = validate_mounts(&subpath, &policy).unwrap_err();
        assert!(
            err.to_string()
                .contains("conflicts or overlaps with active capability grant")
        );

        // Non-colliding target succeeds
        let valid = vec![MountSpec::read_only(
            mount_src.path(),
            "/opt/orbit-cache/cargo",
        )];
        assert!(validate_mounts(&valid, &policy).is_ok());
    }

    #[test]
    fn test_verification_evidence_mounts_format() {
        let dir = tempfile::tempdir().unwrap();
        let spec = ProcessSpec {
            command: "echo test".into(),
            timeout: Duration::from_secs(5),
            scratch: None,
            limits: ProcessLimits::default(),
            read_only_workspace: false,
            rust_toolchain: None,
            mounts: vec![
                MountSpec::read_only(dir.path(), "/opt/orbit-cache/packages/"),
                MountSpec::read_write(dir.path(), "/mnt/extra"),
            ],
            workspace_files: 0,
            skipped_entries: 0,
            fingerprint: Some(WorkspaceFingerprint::default()),
        };
        let formatted: Vec<String> = spec
            .mounts
            .iter()
            .map(|m| {
                let target = m.normalized_target().unwrap_or_else(|_| m.target.clone());
                let access = match m.access {
                    MountAccess::ReadOnly => "ro",
                    MountAccess::ReadWrite => "rw",
                };
                format!("{}: {access}", target.display())
            })
            .collect();
        assert_eq!(
            formatted,
            vec![
                "/opt/orbit-cache/packages: ro".to_string(),
                "/mnt/extra: rw".to_string(),
            ]
        );
        assert!(
            !formatted
                .iter()
                .any(|m| m.contains(dir.path().to_str().unwrap()))
        );
    }
}
