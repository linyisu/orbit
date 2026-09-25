use std::{
    collections::{HashMap, HashSet, VecDeque},
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
    filesystem::inodes::{collect_dir_inodes, collect_opaque_inodes_for_dev},
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
}

#[derive(Debug)]
pub enum Placeholder {
    File { path: PathBuf, dev: u64, ino: u64 },
    Dir { path: PathBuf, dev: u64, ino: u64 },
}

#[derive(Debug, Default)]
pub struct PlaceholderGuard(pub Vec<Placeholder>);

impl PlaceholderGuard {
    pub fn new() -> Self {
        Self(Vec::new())
    }

    pub fn push(&mut self, placeholder: Placeholder) {
        self.0.push(placeholder);
    }
}

impl Drop for PlaceholderGuard {
    fn drop(&mut self) {
        use std::os::unix::fs::MetadataExt;
        for item in &self.0 {
            match item {
                Placeholder::File { path, dev, ino } => {
                    let matches = std::fs::symlink_metadata(path).is_ok_and(|meta| {
                        meta.dev() == *dev
                            && meta.ino() == *ino
                            && meta.is_file()
                            && meta.len() == 0
                    });
                    if matches {
                        let _ = std::fs::remove_file(path);
                    }
                }
                Placeholder::Dir { path, dev, ino } => {
                    let matches = std::fs::symlink_metadata(path).is_ok_and(|meta| {
                        meta.dev() == *dev && meta.ino() == *ino && meta.is_dir()
                    });
                    if matches {
                        let _ = std::fs::remove_dir(path);
                    }
                }
            }
        }
    }
}

impl BwrapSandboxBackend {
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
        placeholder_guard: &mut PlaceholderGuard,
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

        let ws_target = custom_workspace.unwrap_or_else(|| policy.base.workspace.canonical());

        let protected_inodes = if let Some(source) = writable_snapshot_source {
            command.arg("--ro-bind").arg(source).arg("/orbit-source");
            command.args([
                "--size",
                &spec.limits.max_file_size_bytes.to_string(),
                "--tmpfs",
                "/workspace",
            ]);
            HashSet::new()
        } else {
            if spec.read_only_workspace {
                command.arg("--ro-bind").arg(ws_target).arg("/workspace");
            } else {
                command.arg("--bind").arg(ws_target).arg("/workspace");
            }

            let (masked, inodes) = collect_masked_paths(
                ws_target,
                &spec.protected_paths,
                &spec.opaque_runtime_roots,
                spec.read_only_workspace,
                placeholder_guard,
            )?;
            for (container_path, is_dir) in masked {
                if let Some(path_str) = container_path.to_str() {
                    if is_dir {
                        command.args(["--tmpfs", path_str, "--remount-ro", path_str]);
                    } else {
                        command.args(["--ro-bind", "/dev/null", path_str]);
                    }
                }
            }
            inodes
        };

        mount_scratch_and_tmp(&mut command, spec.scratch.as_deref())?;
        mount_toolchain_and_mounts(&mut command, rust_toolchain, mounts, policy)?;
        mount_capabilities(
            &mut command,
            policy,
            ws_target,
            &spec.protected_paths,
            &protected_inodes,
            &spec.opaque_runtime_roots,
        )?;

        configure_limits_and_entrypoint(
            &mut command,
            &self.prlimit_path,
            &spec.limits,
            rust_toolchain.is_some(),
            writable_snapshot_source.is_some(),
            spec.scratch.is_some(),
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

            let mut placeholder_guard = PlaceholderGuard::new();
            let mut cmd = self.build_command(
                policy,
                script_file.path(),
                &spec,
                None,
                None,
                spec.rust_toolchain.as_deref(),
                &spec.mounts,
                &mut placeholder_guard,
            )?;
            let child = cmd.spawn().map_err(failed)?;

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

            let result = run_child_process(
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
            .await;

            drop(placeholder_guard);
            result
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

fn is_opaque_runtime_root(path: &Path, opaque_runtime_roots: &[PathBuf]) -> bool {
    let path_canon = path.canonicalize().ok();
    opaque_runtime_roots.iter().any(|root| {
        if path == root || path.starts_with(root) {
            return true;
        }
        if let Some(ref pc) = path_canon {
            if pc == root || pc.starts_with(root) {
                return true;
            }
            if let Ok(rc) = root.canonicalize() {
                return pc == &rc || pc.starts_with(&rc);
            }
        }
        false
    })
}

fn is_protected_directory(path: &Path, opaque_runtime_roots: &[PathBuf]) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    name == ".git" || is_opaque_runtime_root(path, opaque_runtime_roots)
}

fn mount_capabilities(
    command: &mut Command,
    policy: &EffectivePolicy,
    ws_target: &Path,
    protected_paths: &[PathBuf],
    protected_inodes: &HashSet<(u64, u64)>,
    opaque_runtime_roots: &[PathBuf],
) -> Result<(), RuntimeError> {
    let mut protected_inodes = protected_inodes.clone();
    let mut scanned_opaque_roots = HashSet::new();
    let mut validate_cap = |canonical: &Path| -> Result<(), RuntimeError> {
        use std::os::unix::fs::MetadataExt;
        if crate::execution::process::is_reserved_system_path(canonical) {
            return Err(failed(format!(
                "capability target '{}' is a reserved runtime path",
                canonical.display()
            )));
        }
        if ws_target.starts_with(canonical) {
            return Err(failed(format!(
                "capability target '{}' is an ancestor of workspace",
                canonical.display()
            )));
        }
        if canonical.starts_with(ws_target) {
            return Err(failed(format!(
                "capability target '{}' is inside workspace",
                canonical.display()
            )));
        }
        for p in protected_paths {
            if p.starts_with(canonical) || canonical.starts_with(p) {
                return Err(failed(format!(
                    "capability target '{}' conflicts with protected path '{}'",
                    canonical.display(),
                    p.display()
                )));
            }
        }
        if let Ok(meta) = std::fs::symlink_metadata(canonical) {
            if meta.is_file() && meta.nlink() > 1 {
                collect_opaque_inodes_for_dev(
                    meta.dev(),
                    opaque_runtime_roots,
                    &mut protected_inodes,
                    &mut scanned_opaque_roots,
                )?;
            }
            if protected_inodes.contains(&(meta.dev(), meta.ino())) {
                return Err(failed(format!(
                    "capability target '{}' is a hardlink alias of a protected path",
                    canonical.display()
                )));
            }
            if meta.is_dir() && !meta.file_type().is_symlink() {
                let mut queue = VecDeque::from([(canonical.to_path_buf(), 0usize)]);
                let mut visited = 0usize;
                const MAX_CAP_SCAN_ENTRIES: usize = 10_000;
                const MAX_CAP_SCAN_DEPTH: usize = 16;
                while let Some((curr, depth)) = queue.pop_front() {
                    let entries = std::fs::read_dir(&curr).map_err(|e| {
                        failed(format!(
                            "failed to read capability directory '{}': {e}",
                            curr.display()
                        ))
                    })?;
                    for entry in entries {
                        let entry = entry.map_err(|e| {
                            failed(format!(
                                "failed to read entry in capability directory '{}': {e}",
                                curr.display()
                            ))
                        })?;
                        visited += 1;
                        if visited > MAX_CAP_SCAN_ENTRIES {
                            return Err(failed(format!(
                                "capability directory '{}' exceeds maximum scan limit of {MAX_CAP_SCAN_ENTRIES} entries",
                                canonical.display()
                            )));
                        }
                        let em = entry.path().symlink_metadata().map_err(|e| {
                            failed(format!(
                                "failed to inspect entry in capability directory '{}': {e}",
                                entry.path().display()
                            ))
                        })?;
                        if em.is_file() && em.nlink() > 1 {
                            collect_opaque_inodes_for_dev(
                                em.dev(),
                                opaque_runtime_roots,
                                &mut protected_inodes,
                                &mut scanned_opaque_roots,
                            )?;
                        }
                        if protected_inodes.contains(&(em.dev(), em.ino())) {
                            return Err(failed(format!(
                                "capability directory '{}' contains hardlink to protected path at '{}'",
                                canonical.display(),
                                entry.path().display()
                            )));
                        }
                        if em.is_dir() && !em.file_type().is_symlink() {
                            if depth >= MAX_CAP_SCAN_DEPTH {
                                return Err(failed(format!(
                                    "capability directory '{}' depth exceeds maximum limit of {MAX_CAP_SCAN_DEPTH} at '{}'",
                                    canonical.display(),
                                    entry.path().display()
                                )));
                            }
                            queue.push_back((entry.path(), depth + 1));
                        }
                    }
                }
            }
        }
        Ok(())
    };

    for path in &policy.persistent_read_write {
        let canonical = path.canonical();
        if canonical.exists() {
            validate_cap(canonical)?;
            command.arg("--bind").arg(canonical).arg(canonical);
        }
    }

    for path in &policy.persistent_read {
        let canonical = path.canonical();
        if canonical.exists() {
            validate_cap(canonical)?;
            command.arg("--ro-bind").arg(canonical).arg(canonical);
        }
    }

    if let Ok(store) = policy.session_grants.read() {
        for grant in store.grants() {
            if grant.environment.is_local() {
                let canonical = grant.path.canonical();
                if canonical.exists() {
                    validate_cap(canonical)?;
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

pub type MaskedPathsAndInodes = (Vec<(PathBuf, bool)>, HashSet<(u64, u64)>);

struct WorkspaceEntry {
    relative: PathBuf,
    dev: u64,
    ino: u64,
    is_dir: bool,
    nlink: u64,
    is_file: bool,
}

fn collect_masked_paths(
    ws_target: &Path,
    protected_paths: &[PathBuf],
    opaque_runtime_roots: &[PathBuf],
    read_only_workspace: bool,
    placeholder_guard: &mut PlaceholderGuard,
) -> Result<MaskedPathsAndInodes, RuntimeError> {
    use std::os::unix::fs::MetadataExt;

    let mut masked = Vec::new();
    let mut seen = HashSet::new();
    let mut protected_inodes = HashSet::new();

    let mut all_protected = protected_paths.to_vec();
    if !read_only_workspace {
        for name in [".git", ".env", ".env.local"] {
            let candidate = ws_target.join(name);
            if !all_protected.contains(&candidate) {
                all_protected.push(candidate);
            }
        }
    }

    // 1. Process all_protected
    for protected in &all_protected {
        let rel = protected
            .strip_prefix(ws_target)
            .ok()
            .map(|p| p.to_path_buf())
            .or_else(|| {
                if let (Ok(p_canon), Ok(ws_canon)) =
                    (protected.canonicalize(), ws_target.canonicalize())
                {
                    p_canon
                        .strip_prefix(&ws_canon)
                        .ok()
                        .map(|r| r.to_path_buf())
                } else {
                    None
                }
            });
        let is_inside_ws = rel.as_ref().is_some_and(|r| !r.as_os_str().is_empty());

        match std::fs::symlink_metadata(protected) {
            Ok(meta) => {
                protected_inodes.insert((meta.dev(), meta.ino()));
                if meta.is_dir() && !meta.file_type().is_symlink() {
                    let should_scan_descendants =
                        is_inside_ws || !is_opaque_runtime_root(protected, opaque_runtime_roots);
                    if should_scan_descendants {
                        collect_dir_inodes(protected, &mut protected_inodes)?;
                    }
                }
                if let Some(rel) = rel.filter(|r| !r.as_os_str().is_empty()) {
                    let container_path = Path::new("/workspace").join(rel);
                    if seen.insert(container_path.clone()) {
                        masked.push((container_path, meta.is_dir()));
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // When workspace is read-only, bwrap mounts with --ro-bind; container cannot create any files.
                // When workspace is read-write, create a guarded placeholder so bwrap can mask it.
                let target_rel = if !read_only_workspace {
                    rel.filter(|r| !r.as_os_str().is_empty())
                } else {
                    None
                };
                if let Some(rel) = target_rel {
                    let container_path = Path::new("/workspace").join(rel);
                    if let Some(parent) = protected.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    if is_protected_directory(protected, opaque_runtime_roots) {
                        match std::fs::create_dir(protected) {
                            Ok(()) => {
                                let meta = std::fs::symlink_metadata(protected).map_err(|e| {
                                    failed(format!(
                                        "failed to stat placeholder directory '{}': {e}",
                                        protected.display()
                                    ))
                                })?;
                                placeholder_guard.push(Placeholder::Dir {
                                    path: protected.clone(),
                                    dev: meta.dev(),
                                    ino: meta.ino(),
                                });
                                protected_inodes.insert((meta.dev(), meta.ino()));
                                if seen.insert(container_path.clone()) {
                                    masked.push((container_path, true));
                                }
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                                let meta = std::fs::symlink_metadata(protected).map_err(|e| {
                                    failed(format!(
                                        "failed to stat existing protected directory '{}': {e}",
                                        protected.display()
                                    ))
                                })?;
                                protected_inodes.insert((meta.dev(), meta.ino()));
                                if seen.insert(container_path.clone()) {
                                    masked.push((container_path, meta.is_dir()));
                                }
                            }
                            Err(e) => {
                                return Err(failed(format!(
                                    "failed to create placeholder directory for protected path '{}': {e}",
                                    protected.display()
                                )));
                            }
                        }
                    } else {
                        match std::fs::File::create_new(protected) {
                            Ok(file) => {
                                drop(file);
                                let meta = std::fs::symlink_metadata(protected).map_err(|e| {
                                    failed(format!(
                                        "failed to stat placeholder file '{}': {e}",
                                        protected.display()
                                    ))
                                })?;
                                placeholder_guard.push(Placeholder::File {
                                    path: protected.clone(),
                                    dev: meta.dev(),
                                    ino: meta.ino(),
                                });
                                protected_inodes.insert((meta.dev(), meta.ino()));
                                if seen.insert(container_path.clone()) {
                                    masked.push((container_path, false));
                                }
                            }
                            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                                let meta = std::fs::symlink_metadata(protected).map_err(|e| {
                                    failed(format!(
                                        "failed to stat existing protected file '{}': {e}",
                                        protected.display()
                                    ))
                                })?;
                                protected_inodes.insert((meta.dev(), meta.ino()));
                                if seen.insert(container_path.clone()) {
                                    masked.push((container_path, meta.is_dir()));
                                }
                            }
                            Err(e) => {
                                return Err(failed(format!(
                                    "failed to create placeholder file for protected path '{}': {e}",
                                    protected.display()
                                )));
                            }
                        }
                    }
                }
            }
            Err(e) => {
                return Err(failed(format!(
                    "failed to inspect protected path '{}': {e}",
                    protected.display()
                )));
            }
        }
    }

    // 2. Scan ws_target for .git, .env*, .orbit-write-* and identify hardlinks
    const MAX_SCAN_DIRS: usize = 50_000;
    const MAX_DEPTH: usize = 32;

    let mut queue = VecDeque::new();
    queue.push_back((PathBuf::new(), 0usize));
    let mut count = 0;
    let mut files_to_check: Vec<WorkspaceEntry> = Vec::new();

    while let Some((rel, depth)) = queue.pop_front() {
        count += 1;
        if count > MAX_SCAN_DIRS {
            return Err(failed(format!(
                "workspace directory count exceeds {MAX_SCAN_DIRS} while scanning protected paths; refusing to start sandbox"
            )));
        }
        if depth > MAX_DEPTH {
            return Err(failed(format!(
                "workspace directory depth exceeds {MAX_DEPTH} at '{}'; refusing to start sandbox",
                rel.display()
            )));
        }

        let current_host = ws_target.join(&rel);
        let entries = match std::fs::read_dir(&current_host) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(failed(format!(
                    "failed to read directory '{}' while scanning protected paths: {e}",
                    current_host.display()
                )));
            }
        };

        for entry in entries {
            let entry = entry.map_err(|e| {
                failed(format!(
                    "failed to read directory entry in '{}': {e}",
                    current_host.display()
                ))
            })?;
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            let child_rel = rel.join(&file_name);
            let child_host = ws_target.join(&child_rel);
            let meta = match std::fs::symlink_metadata(&child_host) {
                Ok(m) => m,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    return Err(failed(format!(
                        "failed to inspect file '{}': {e}",
                        child_host.display()
                    )));
                }
            };

            let is_dir = meta.is_dir();
            let is_symlink = meta.file_type().is_symlink();
            let dev = meta.dev();
            let ino = meta.ino();

            let is_protected_name = name == ".git"
                || name == ".env"
                || name.starts_with(".env.")
                || name.starts_with(".orbit-write-");

            if is_protected_name {
                protected_inodes.insert((dev, ino));
                let container_path = Path::new("/workspace").join(&child_rel);
                if seen.insert(container_path.clone()) {
                    masked.push((container_path, is_dir && !is_symlink));
                }
                if is_dir && !is_symlink {
                    collect_dir_inodes(&child_host, &mut protected_inodes)?;
                }
                continue;
            }

            files_to_check.push(WorkspaceEntry {
                relative: child_rel.clone(),
                dev,
                ino,
                is_dir: is_dir && !is_symlink,
                nlink: meta.nlink(),
                is_file: meta.is_file(),
            });

            if is_dir && !is_symlink {
                queue.push_back((child_rel, depth + 1));
            }
        }
    }

    // A hardlink with fewer visible names than its link count may point into
    // an external opaque root. Inspect such roots on demand, avoiding a full
    // scan for ordinary workspaces and internal-only hardlinks.
    let mut visible_links = HashMap::new();
    for entry in &files_to_check {
        if entry.is_file && entry.nlink > 1 {
            *visible_links.entry((entry.dev, entry.ino)).or_insert(0u64) += 1;
        }
    }
    let mut scanned_opaque_roots = HashSet::new();
    for entry in &files_to_check {
        if entry.is_file
            && entry.nlink > 1
            && entry.nlink
                > visible_links
                    .get(&(entry.dev, entry.ino))
                    .copied()
                    .unwrap_or(0)
        {
            collect_opaque_inodes_for_dev(
                entry.dev,
                opaque_runtime_roots,
                &mut protected_inodes,
                &mut scanned_opaque_roots,
            )?;
        }
    }

    // Mask all files matching protected_inodes (including discovered aliases).
    for entry in files_to_check {
        if protected_inodes.contains(&(entry.dev, entry.ino)) {
            let container_path = Path::new("/workspace").join(&entry.relative);
            if seen.insert(container_path.clone()) {
                masked.push((container_path, entry.is_dir));
            }
        }
    }

    // Filter so that if a parent directory is already masked with tmpfs, child paths are omitted
    let mut filtered = Vec::new();
    masked.sort_by_key(|(p, _)| p.components().count());
    for (path, is_dir) in masked {
        let is_covered = filtered
            .iter()
            .any(|(parent, parent_is_dir): &(PathBuf, bool)| {
                *parent_is_dir && path.starts_with(parent) && &path != parent
            });
        if !is_covered {
            filtered.push((path, is_dir));
        }
    }

    Ok((filtered, protected_inodes))
}

pub fn validate_mounts(mounts: &[MountSpec], policy: &EffectivePolicy) -> Result<(), RuntimeError> {
    let mut normalized_targets = Vec::with_capacity(mounts.len());
    for mount in mounts {
        mount.validate()?;
        validate_mount_source(&mount.source, policy)?;
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

fn validate_mount_source(source: &Path, policy: &EffectivePolicy) -> Result<PathBuf, RuntimeError> {
    let canonical = source.canonicalize().map_err(failed)?;
    if crate::execution::process::is_reserved_system_path(&canonical) {
        return Err(failed(format!(
            "mount source '{}' is a reserved system path",
            canonical.display()
        )));
    }

    let protected = policy
        .protected_paths
        .read()
        .map_err(|_| failed("protected path policy lock is poisoned"))?;
    for path in protected.iter() {
        if canonical.starts_with(path) || path.starts_with(&canonical) {
            return Err(failed(format!(
                "mount source '{}' overlaps protected path '{}'",
                canonical.display(),
                path.display()
            )));
        }
    }
    Ok(canonical)
}

fn mount_toolchain_and_mounts(
    command: &mut Command,
    rust_toolchain: Option<&Path>,
    mounts: &[MountSpec],
    policy: &EffectivePolicy,
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
        let path = validate_mount_source(&mount.source, policy)?;
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

        let home = path.join("home");
        match std::fs::symlink_metadata(&home) {
            Ok(metadata) if !metadata.is_dir() || metadata.file_type().is_symlink() => {
                return Err(failed("scratch home must be a regular directory"));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::DirBuilder::new()
                    .mode(0o700)
                    .create(&home)
                    .map_err(failed)?;
            }
            Err(error) => return Err(failed(error)),
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
        "--chdir",
        "/workspace",
    ]);

    if scratch.is_none() {
        command.args(["--dir", "/tmp/home"]);
    }

    Ok(())
}

fn configure_limits_and_entrypoint(
    command: &mut Command,
    prlimit: &Path,
    limits: &ProcessLimits,
    has_rust: bool,
    is_writable_tmpfs: bool,
    persistent_home: bool,
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
        if persistent_home {
            "/scratch/home"
        } else {
            "/tmp/home"
        },
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
        verification_error: None,
        verification,
    })
}

#[allow(clippy::too_many_arguments)]
#[deprecated(note = "legacy snapshot execution API; use BwrapSandboxBackend instead")]
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

    let mut placeholder_guard = PlaceholderGuard::new();
    let mut child_cmd = backend.build_command(
        policy,
        script_file.path(),
        &spec,
        Some(snapshot_path),
        writable_source,
        rust_toolchain,
        mounts,
        &mut placeholder_guard,
    )?;
    let child = child_cmd.spawn().map_err(failed)?;

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

#[deprecated(note = "legacy snapshot execution API; use BwrapSandboxBackend instead")]
pub async fn run(
    workspace: Workspace,
    script: String,
    timeout: Duration,
    token: CancellationToken,
) -> Result<CommandResult, RuntimeError> {
    #[allow(deprecated)]
    run_with_options(workspace, script, timeout, token, false).await
}

#[deprecated(note = "legacy snapshot execution API; use BwrapSandboxBackend instead")]
pub async fn run_with_options(
    workspace: Workspace,
    script: String,
    timeout: Duration,
    token: CancellationToken,
    writable_snapshot: bool,
) -> Result<CommandResult, RuntimeError> {
    #[allow(deprecated)]
    run_with_toolchain(workspace, script, timeout, token, writable_snapshot, None).await
}

#[deprecated(note = "legacy snapshot execution API; use BwrapSandboxBackend instead")]
pub async fn run_with_toolchain(
    workspace: Workspace,
    script: String,
    timeout: Duration,
    token: CancellationToken,
    writable_snapshot: bool,
    rust_toolchain: Option<&Path>,
) -> Result<CommandResult, RuntimeError> {
    #[allow(deprecated)]
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

#[deprecated(note = "legacy snapshot execution API; use BwrapSandboxBackend instead")]
pub async fn run_with_resources(
    workspace: Workspace,
    script: String,
    timeout: Duration,
    token: CancellationToken,
    writable_snapshot: bool,
    rust_toolchain: Option<&Path>,
    scratch: Option<&Path>,
) -> Result<CommandResult, RuntimeError> {
    #[allow(deprecated)]
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
#[deprecated(note = "legacy snapshot execution API; use BwrapSandboxBackend instead")]
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
    #[allow(deprecated)]
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
    fn rejects_mount_sources_overlapping_protected_paths() {
        let workspace = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let secret = source.path().join("credential");
        std::fs::write(&secret, "secret").unwrap();
        let policy = EffectivePolicy::new(crate::policy::BasePolicy::new(
            crate::filesystem::ResolvedPath::from_canonical(workspace.path()),
        ));
        policy.protected_paths.write().unwrap().push(secret.clone());

        let ancestor = MountSpec::read_only(source.path(), "/opt/orbit-cache/source");
        let exact = MountSpec::read_only(&secret, "/opt/orbit-cache/secret");

        assert!(validate_mounts(&[ancestor], &policy).is_err());
        assert!(validate_mounts(&[exact], &policy).is_err());
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
            protected_paths: Vec::new(),
            opaque_runtime_roots: Vec::new(),
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

    #[test]
    fn test_collect_masked_paths_masks_deep_env_and_nonexistent_placeholders_and_hardlinks() {
        let ws_dir = tempfile::tempdir().unwrap();
        let ws = ws_dir.path();

        // 1. Create a .env file and hardlink to it
        let env_file = ws.join(".env");
        std::fs::write(&env_file, "SECRET=123").unwrap();
        let env_alias = ws.join("alias.env");
        std::fs::hard_link(&env_file, &env_alias).unwrap();

        // 2. Real config.toml and hardlink to it
        let config_file = ws.join("config.toml");
        std::fs::write(&config_file, "key=456").unwrap();
        let config_alias = ws.join("alias.toml");
        std::fs::hard_link(&config_file, &config_alias).unwrap();

        // 3. Non-existent protected paths: one file, one directory
        let nonexistent_file = ws.join("secret.key");
        let nonexistent_dir = ws.join("orbit.db.workspaces");

        let protected_paths = vec![
            config_file.clone(),
            nonexistent_file.clone(),
            nonexistent_dir.clone(),
        ];
        let opaque_runtime_roots = vec![nonexistent_dir.clone()];

        // Case A: Read-only workspace - does NOT create placeholders
        {
            let mut guard = PlaceholderGuard::new();
            let (masked, _) = collect_masked_paths(
                ws,
                &protected_paths,
                &opaque_runtime_roots,
                true,
                &mut guard,
            )
            .unwrap();
            assert!(!nonexistent_file.exists());
            assert!(!nonexistent_dir.exists());

            // But existing config and alias are masked!
            assert!(
                masked
                    .iter()
                    .any(|(p, is_dir)| p == Path::new("/workspace/config.toml") && !*is_dir)
            );
            assert!(
                masked
                    .iter()
                    .any(|(p, is_dir)| p == Path::new("/workspace/alias.toml") && !*is_dir)
            );
            assert!(
                masked
                    .iter()
                    .any(|(p, is_dir)| p == Path::new("/workspace/.env") && !*is_dir)
            );
            assert!(
                masked
                    .iter()
                    .any(|(p, is_dir)| p == Path::new("/workspace/alias.env") && !*is_dir)
            );
        }

        // Case B: Read-write workspace - creates guarded placeholders and cleans them up
        {
            let mut guard = PlaceholderGuard::new();
            let (masked, _) = collect_masked_paths(
                ws,
                &protected_paths,
                &opaque_runtime_roots,
                false,
                &mut guard,
            )
            .unwrap();
            assert!(nonexistent_file.is_file());
            assert!(nonexistent_dir.is_dir());

            // Masked list contains the placeholders with correct is_dir flags
            assert!(
                masked
                    .iter()
                    .any(|(p, is_dir)| p == Path::new("/workspace/secret.key") && !*is_dir)
            );
            assert!(
                masked
                    .iter()
                    .any(|(p, is_dir)| p == Path::new("/workspace/orbit.db.workspaces") && *is_dir)
            );

            // Masked list contains the hardlink aliases
            assert!(
                masked
                    .iter()
                    .any(|(p, is_dir)| p == Path::new("/workspace/alias.toml") && !*is_dir)
            );
            assert!(
                masked
                    .iter()
                    .any(|(p, is_dir)| p == Path::new("/workspace/alias.env") && !*is_dir)
            );

            drop(guard);
            // After drop, placeholders are removed!
            assert!(!nonexistent_file.exists());
            assert!(!nonexistent_dir.exists());
            // Existing files are NOT removed!
            assert!(config_file.exists());
            assert!(env_file.exists());
        }

        // Case C: Protected directory contents are recursively added to protected_inodes
        {
            let artifact_dir = ws.join("my.artifacts");
            std::fs::create_dir_all(&artifact_dir).unwrap();
            let secret_file = artifact_dir.join("token.txt");
            std::fs::write(&secret_file, "supersecret").unwrap();
            let secret_alias = ws.join("leak_token.txt");
            std::fs::hard_link(&secret_file, &secret_alias).unwrap();

            let mut guard = PlaceholderGuard::new();
            let (masked, _) = collect_masked_paths(
                ws,
                std::slice::from_ref(&artifact_dir),
                &[],
                true,
                &mut guard,
            )
            .unwrap();

            // Both the artifact directory and the hardlink outside it are masked!
            assert!(
                masked
                    .iter()
                    .any(|(p, is_dir)| p == Path::new("/workspace/my.artifacts") && *is_dir)
            );
            assert!(
                masked
                    .iter()
                    .any(|(p, is_dir)| p == Path::new("/workspace/leak_token.txt") && !*is_dir)
            );
        }

        // Case D: Non-existent .git and .env are protected with placeholders in RW mode
        {
            let fresh_ws = tempfile::tempdir().unwrap();
            let mut guard = PlaceholderGuard::new();
            let (masked, _) =
                collect_masked_paths(fresh_ws.path(), &[], &[], false, &mut guard).unwrap();

            // .git (dir) and .env (file) were created as placeholders and masked!
            assert!(fresh_ws.path().join(".git").is_dir());
            assert!(fresh_ws.path().join(".env").is_file());
            assert!(
                masked
                    .iter()
                    .any(|(p, is_dir)| p == Path::new("/workspace/.git") && *is_dir)
            );
            assert!(
                masked
                    .iter()
                    .any(|(p, is_dir)| p == Path::new("/workspace/.env") && !*is_dir)
            );

            drop(guard);
            // Cleaned up after drop
            assert!(!fresh_ws.path().join(".git").exists());
            assert!(!fresh_ws.path().join(".env").exists());
        }
    }

    #[test]
    fn test_mount_capabilities_rejects_deep_nested_hardlinks() {
        let ws_dir = tempfile::tempdir().unwrap();
        let ws = ws_dir.path();
        let config_file = ws.join("config.toml");
        std::fs::write(&config_file, "super_api_key").unwrap();

        // Collect protected inodes for the workspace
        let mut guard = PlaceholderGuard::new();
        let (_, protected_inodes) = collect_masked_paths(
            ws,
            std::slice::from_ref(&config_file),
            &[],
            true,
            &mut guard,
        )
        .unwrap();

        // Create an external capability directory with a deeply nested hardlink to config.toml
        let cap_dir = tempfile::tempdir().unwrap();
        let nested_dir = cap_dir.path().join("sub1").join("sub2");
        std::fs::create_dir_all(&nested_dir).unwrap();
        let hardlink = nested_dir.join("aliased_config.txt");
        std::fs::hard_link(&config_file, &hardlink).unwrap();

        let mut policy = EffectivePolicy::new(crate::policy::BasePolicy::new(
            crate::filesystem::ResolvedPath::from_canonical(ws),
        ));
        let resolved_cap = crate::filesystem::ResolvedPath::from_canonical(cap_dir.path());
        policy.persistent_read.push(resolved_cap);

        let mut command = tokio::process::Command::new("/bin/true");
        let err = mount_capabilities(
            &mut command,
            &policy,
            ws,
            &[config_file],
            &protected_inodes,
            &[],
        )
        .unwrap_err();

        assert!(
            err.to_string()
                .contains("contains hardlink to protected path")
        );
    }

    #[test]
    fn mount_capabilities_rejects_opaque_root_descendant_alias() {
        let ws_dir = tempfile::tempdir().unwrap();
        let storage = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let opaque = storage.path().join("orbit.workspaces");
        std::fs::create_dir(&opaque).unwrap();
        let secret = opaque.join("secret.txt");
        std::fs::write(&secret, "private runtime data").unwrap();
        let alias = external.path().join("alias.txt");
        std::fs::hard_link(&secret, &alias).unwrap();

        let mut policy = EffectivePolicy::new(crate::policy::BasePolicy::new(
            crate::filesystem::ResolvedPath::from_canonical(ws_dir.path()),
        ));
        policy
            .persistent_read
            .push(crate::filesystem::ResolvedPath::from_canonical(&alias));
        let mut command = tokio::process::Command::new("/bin/true");
        let error = mount_capabilities(
            &mut command,
            &policy,
            ws_dir.path(),
            std::slice::from_ref(&opaque),
            &HashSet::new(),
            std::slice::from_ref(&opaque),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("hardlink alias of a protected path")
        );
    }

    #[test]
    fn test_collect_dir_inodes_rejects_depth_exceeding_limit() {
        let temp = tempfile::tempdir().unwrap();
        let mut curr = temp.path().to_path_buf();
        // Create 34 nested directory levels (exceeding MAX_PROTECTED_DIR_DEPTH = 32)
        for i in 0..34 {
            curr = curr.join(format!("dir_{i}"));
        }
        std::fs::create_dir_all(&curr).unwrap();
        let file = curr.join("secret.txt");
        std::fs::write(&file, "leak").unwrap();

        let mut inodes = HashSet::new();
        let err = collect_dir_inodes(temp.path(), &mut inodes).unwrap_err();
        assert!(
            err.to_string()
                .contains("depth exceeds maximum limit of 32")
        );
    }

    #[test]
    fn test_mount_capabilities_rejects_depth_exceeding_limit() {
        let ws_dir = tempfile::tempdir().unwrap();
        let cap_dir = tempfile::tempdir().unwrap();
        let mut curr = cap_dir.path().to_path_buf();
        // Create 18 nested directory levels (exceeding MAX_CAP_SCAN_DEPTH = 16)
        for i in 0..18 {
            curr = curr.join(format!("sub_{i}"));
        }
        std::fs::create_dir_all(&curr).unwrap();
        let file = curr.join("data.txt");
        std::fs::write(&file, "hello").unwrap();

        let mut policy = EffectivePolicy::new(crate::policy::BasePolicy::new(
            crate::filesystem::ResolvedPath::from_canonical(ws_dir.path()),
        ));
        let resolved_cap = crate::filesystem::ResolvedPath::from_canonical(cap_dir.path());
        policy.persistent_read.push(resolved_cap);

        let mut command = tokio::process::Command::new("/bin/true");
        let protected_inodes = HashSet::new();
        let err = mount_capabilities(
            &mut command,
            &policy,
            ws_dir.path(),
            &[],
            &protected_inodes,
            &[],
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("depth exceeds maximum limit of 16")
        );
    }

    #[test]
    fn opaque_roots_skip_idle_scans_and_fail_closed_for_ambiguous_hardlinks() {
        use std::os::unix::fs::MetadataExt;

        let ws_dir = tempfile::tempdir().unwrap();
        let ws = ws_dir.path();

        // 1. Create an external opaque runtime storage directory (e.g. orbit.workspaces) that exceeds MAX_PROTECTED_DIR_DEPTH (32)
        let storage_parent = tempfile::tempdir().unwrap();
        let external_storage = storage_parent.path().join("orbit.workspaces");
        let mut curr = external_storage.clone();
        for i in 0..35 {
            curr = curr.join(format!("level_{i}"));
        }
        std::fs::create_dir_all(&curr).unwrap();
        std::fs::write(curr.join("deep.txt"), "content").unwrap();

        // Verify that direct collect_dir_inodes on external_storage WOULD fail closed
        let mut test_inodes = HashSet::new();
        assert!(collect_dir_inodes(&external_storage, &mut test_inodes).is_err());

        let mut guard = PlaceholderGuard::new();
        let (masked, inodes) = collect_masked_paths(
            ws,
            std::slice::from_ref(&external_storage),
            std::slice::from_ref(&external_storage),
            false,
            &mut guard,
        )
        .unwrap();
        let ext_meta = std::fs::symlink_metadata(&external_storage).unwrap();
        assert!(inodes.contains(&(ext_meta.dev(), ext_meta.ino())));
        assert!(
            masked
                .iter()
                .all(|(path, _)| !path.to_string_lossy().contains("level_"))
        );

        // An external hardlink on the same device makes the skipped root
        // ambiguous. Its deep scan cannot complete, so the command is denied.
        let external_file_dir = tempfile::tempdir().unwrap();
        let external_db = external_file_dir.path().join("orbit.db");
        std::fs::write(&external_db, "sqlite_header").unwrap();
        let ws_db_alias = ws.join("leaked_orbit.db");
        std::fs::hard_link(&external_db, &ws_db_alias).unwrap();

        let protected_paths = vec![external_storage.clone(), external_db.clone()];
        let opaque_runtime_roots = vec![external_storage.clone()];

        let mut guard = PlaceholderGuard::new();
        let error = collect_masked_paths(
            ws,
            &protected_paths,
            &opaque_runtime_roots,
            false,
            &mut guard,
        )
        .unwrap_err();
        assert!(error.to_string().contains("depth exceeds maximum limit"));
    }

    #[test]
    fn test_external_protected_directory_descendant_hardlinks_are_masked() {
        use std::os::unix::fs::MetadataExt;

        let ws_dir = tempfile::tempdir().unwrap();
        let ws = ws_dir.path();

        // 1. Create a regular external protected directory (e.g. user credentials folder)
        let external_secrets = tempfile::tempdir().unwrap();
        let secret_file = external_secrets.path().join("api_key.txt");
        std::fs::write(&secret_file, "secret_content_12345").unwrap();

        // 2. Create a nested secret file in a subdirectory of external protected directory
        let nested_secrets_dir = external_secrets.path().join("nested_folder");
        std::fs::create_dir_all(&nested_secrets_dir).unwrap();
        let nested_secret_file = nested_secrets_dir.join("token.json");
        std::fs::write(&nested_secret_file, r#"{"token": "xyz"}"#).unwrap();

        // 3. Hardlink to secret_file and nested_secret_file from inside the workspace
        let ws_leak1 = ws.join("leaked_key.txt");
        std::fs::hard_link(&secret_file, &ws_leak1).unwrap();
        let ws_leak2 = ws.join("leaked_token.json");
        std::fs::hard_link(&nested_secret_file, &ws_leak2).unwrap();

        let protected_paths = vec![external_secrets.path().to_path_buf()];

        let mut guard = PlaceholderGuard::new();
        let (masked, inodes) =
            collect_masked_paths(ws, &protected_paths, &[], false, &mut guard).unwrap();

        // The secret files inside external directory MUST be collected into protected_inodes
        let secret_meta1 = std::fs::symlink_metadata(&secret_file).unwrap();
        let secret_meta2 = std::fs::symlink_metadata(&nested_secret_file).unwrap();
        assert!(
            inodes.contains(&(secret_meta1.dev(), secret_meta1.ino())),
            "descendant file inside external protected directory must be tracked in protected_inodes"
        );
        assert!(
            inodes.contains(&(secret_meta2.dev(), secret_meta2.ino())),
            "nested descendant file inside external protected directory must be tracked in protected_inodes"
        );

        // Both hardlink aliases inside workspace MUST be masked!
        assert!(
            masked
                .iter()
                .any(|(p, is_dir)| p == Path::new("/workspace/leaked_key.txt") && !*is_dir),
            "hardlink alias inside workspace pointing to file in external protected directory must be masked"
        );
        assert!(
            masked
                .iter()
                .any(|(p, is_dir)| p == Path::new("/workspace/leaked_token.json") && !*is_dir),
            "nested hardlink alias inside workspace pointing to file in external protected directory must be masked"
        );
    }

    #[test]
    fn test_external_directory_matching_heuristic_name_scans_descendants_when_not_in_opaque_roots()
    {
        use std::os::unix::fs::MetadataExt;

        let ws_dir = tempfile::tempdir().unwrap();
        let ws = ws_dir.path();

        // Directory named like the old heuristic suffix (e.g. .workspaces), but it is a user directory
        let external_dir = tempfile::tempdir().unwrap();
        let custom_workspaces = external_dir.path().join("my_project.workspaces");
        std::fs::create_dir_all(&custom_workspaces).unwrap();
        let secret = custom_workspaces.join("secret_data.csv");
        std::fs::write(&secret, "confidential").unwrap();

        let ws_link = ws.join("leak_data.csv");
        std::fs::hard_link(&secret, &ws_link).unwrap();

        let protected_paths = vec![custom_workspaces.clone()];
        // NOT in opaque_runtime_roots
        let opaque_runtime_roots = vec![];

        let mut guard = PlaceholderGuard::new();
        let (masked, inodes) = collect_masked_paths(
            ws,
            &protected_paths,
            &opaque_runtime_roots,
            false,
            &mut guard,
        )
        .unwrap();

        let meta = std::fs::symlink_metadata(&secret).unwrap();
        assert!(
            inodes.contains(&(meta.dev(), meta.ino())),
            "descendant of user directory named *.workspaces must be scanned when not registered as opaque runtime root"
        );
        assert!(
            masked
                .iter()
                .any(|(p, is_dir)| p == Path::new("/workspace/leak_data.csv") && !*is_dir),
            "hardlink to secret inside user directory named *.workspaces must be masked"
        );
    }
}
