mod isolation;

pub use isolation::browser_reader_command;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
};
use tokio_util::sync::CancellationToken;

use self::isolation::{configure_isolation, mount_system_paths, seccomp_filter};
use crate::error::{RuntimeError, failed};
use crate::verification::{FileFingerprint, VerificationEvidence, WorkspaceFingerprint};
use crate::{
    execution::process::{
        CommandResult, MAX_OUTPUT, MIB, MountAccess, MountSpec, OutputLog, ProcessLimits,
        ProcessSpec, TMP_SIZE_BYTES, system_executable,
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
                snapshot.fingerprint.skipped_entries,
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
        let bwrap_path = system_executable("bwrap").ok_or_else(|| {
            failed("sandbox unavailable: install Bubblewrap at /usr/bin/bwrap; refusing unsandboxed execution")
        })?;

        let prlimit_path = system_executable("prlimit").ok_or_else(|| {
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
        task_tmp: Option<&Path>,
    ) -> Result<Command, RuntimeError> {
        validate_mounts(mounts, policy)?;

        let mut command = Command::new(&self.bwrap_path);
        let seccomp = seccomp_filter()?;

        command
            .env_clear()
            .arg("--clearenv")
            .stdin(Stdio::from(seccomp))
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

        let mut protected_inodes = if let Some(source) = writable_snapshot_source {
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

            let (masked, inodes) = collect_masked_paths_with_templates(
                ws_target,
                &spec.protected_paths,
                &spec.opaque_runtime_roots,
                spec.read_only_workspace,
                spec.unmask_git,
                placeholder_guard,
                &policy.read_templates,
                &policy.write_templates,
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

        for template in &policy.read_templates {
            if !policy.write_templates.contains(template) {
                let rel = template
                    .strip_prefix(ws_target)
                    .map_err(|_| failed("template outside workspace"))?;
                command
                    .arg("--ro-bind")
                    .arg(template)
                    .arg(Path::new("/workspace").join(rel));
            }
        }

        mount_scratch_and_tmp(
            &mut command,
            spec.scratch.as_deref(),
            task_tmp,
            spec.limits.max_tmp_bytes,
        )?;
        mount_toolchain_and_mounts(&mut command, rust_toolchain, mounts, policy)?;
        mount_capabilities(
            &mut command,
            policy,
            ws_target,
            &spec.protected_paths,
            &protected_inodes,
            &spec.opaque_runtime_roots,
        )?;

        if spec.unmask_git {
            apply_git_mounts(
                &mut command,
                ws_target,
                spec,
                &mut protected_inodes,
                policy,
                placeholder_guard,
            )?;
        }

        let container_cwd = resolve_container_cwd(ws_target, mounts, spec.cwd.as_deref());
        command.arg("--chdir").arg(container_cwd);

        configure_limits_and_entrypoint(
            &mut command,
            &self.prlimit_path,
            &spec.limits,
            rust_toolchain.is_some(),
            writable_snapshot_source.is_some(),
            spec.scratch.is_some(),
            &spec.env,
            mounts,
            Some(policy),
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

            let task_tmp = tempfile::Builder::new()
                .prefix("orbit-task-tmp-")
                .tempdir()
                .map_err(failed)?;

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
                Some(task_tmp.path()),
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
            .await?;

            drop(placeholder_guard);

            if result.exit_code != Some(0) && isolation::user_namespace_denied(&result.stderr) {
                return Err(failed(format!(
                    "sandbox initialization failed before the command could run: {}\n\
                     The target host denied Bubblewrap user/network namespace setup. \
                     On Ubuntu with apparmor_restrict_unprivileged_userns=1, an administrator \
                     must install and enable a Bubblewrap-specific AppArmor profile. \
                     Retrying the command will not repair host policy.",
                    result.stderr.trim()
                )));
            }

            Ok(result)
        })
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

struct ProtectedInodesAndRoots {
    inodes: HashSet<(u64, u64)>,
    opaque_roots: Vec<PathBuf>,
}

fn collect_all_protected_inodes(
    policy: &EffectivePolicy,
) -> Result<ProtectedInodesAndRoots, RuntimeError> {
    use std::os::unix::fs::MetadataExt;

    let mut protected_inodes = HashSet::new();

    let opaque_runtime_roots = policy
        .opaque_runtime_roots
        .read()
        .map(|guard| guard.clone())
        .unwrap_or_default();

    for root in &opaque_runtime_roots {
        if let Ok(meta) = std::fs::symlink_metadata(root) {
            protected_inodes.insert((meta.dev(), meta.ino()));
        }
    }

    let protected = policy
        .protected_paths
        .read()
        .map_err(|_| failed("protected path policy lock is poisoned"))?;

    for path in protected.iter() {
        if let Ok(meta) = std::fs::symlink_metadata(path) {
            protected_inodes.insert((meta.dev(), meta.ino()));
            if meta.is_dir()
                && !meta.file_type().is_symlink()
                && !is_opaque_runtime_root(path, &opaque_runtime_roots)
            {
                collect_dir_inodes(path, &mut protected_inodes)?;
            }
        }
    }

    let ws_target = policy.base.workspace.canonical();
    if ws_target.is_dir() {
        let mut queue = VecDeque::from([(PathBuf::new(), 0usize)]);
        let mut count = 0usize;
        const MAX_SCAN_DIRS: usize = 10_000;
        const MAX_DEPTH: usize = 16;

        while let Some((rel, depth)) = queue.pop_front() {
            count += 1;
            if count > MAX_SCAN_DIRS || depth > MAX_DEPTH {
                break;
            }

            let current_host = ws_target.join(&rel);
            let Ok(entries) = std::fs::read_dir(&current_host) else {
                continue;
            };

            for entry in entries.flatten() {
                let file_name = entry.file_name();
                let name = file_name.to_string_lossy();
                let child_rel = rel.join(&file_name);
                let child_host = ws_target.join(&child_rel);
                let Ok(meta) = std::fs::symlink_metadata(&child_host) else {
                    continue;
                };

                let is_dir = meta.is_dir();
                let is_symlink = meta.file_type().is_symlink();

                if crate::filesystem::path_security::is_protected_workspace_name(&name) {
                    protected_inodes.insert((meta.dev(), meta.ino()));
                    if is_dir && !is_symlink {
                        let _ = collect_dir_inodes(&child_host, &mut protected_inodes);
                    }
                    continue;
                }

                if is_dir && !is_symlink && depth < MAX_DEPTH {
                    queue.push_back((child_rel, depth + 1));
                }
            }
        }
    }

    Ok(ProtectedInodesAndRoots {
        inodes: protected_inodes,
        opaque_roots: opaque_runtime_roots,
    })
}

fn validate_inode_against_protected(
    canonical: &Path,
    protected_inodes: &mut HashSet<(u64, u64)>,
    opaque_runtime_roots: &[PathBuf],
    scanned_opaque_roots: &mut HashSet<PathBuf>,
    target_kind: &str,
    max_scan_entries: Option<usize>,
) -> Result<(), RuntimeError> {
    use std::os::unix::fs::MetadataExt;

    if let Ok(meta) = std::fs::symlink_metadata(canonical) {
        if meta.is_file() && meta.nlink() > 1 {
            collect_opaque_inodes_for_dev(
                meta.dev(),
                opaque_runtime_roots,
                protected_inodes,
                scanned_opaque_roots,
            )?;
        }

        if protected_inodes.contains(&(meta.dev(), meta.ino())) {
            return Err(failed(format!(
                "{target_kind} '{}' is a hardlink alias of a protected path",
                canonical.display()
            )));
        }

        if meta.is_dir() && !meta.file_type().is_symlink() {
            let mut queue = VecDeque::from([(canonical.to_path_buf(), 0usize)]);
            let mut visited = 0usize;

            let max_depth = if max_scan_entries.is_some() { 16 } else { 64 };

            while let Some((curr, depth)) = queue.pop_front() {
                let entries = std::fs::read_dir(&curr).map_err(|e| {
                    failed(format!(
                        "failed to read {target_kind} directory '{}': {e}",
                        curr.display()
                    ))
                })?;

                for entry in entries {
                    let entry = entry.map_err(|e| {
                        failed(format!(
                            "failed to read entry in {target_kind} directory '{}': {e}",
                            curr.display()
                        ))
                    })?;

                    visited += 1;

                    if let Some(limit) = max_scan_entries.filter(|&limit| visited > limit) {
                        return Err(failed(format!(
                            "{target_kind} directory '{}' exceeds maximum scan limit of {limit} entries",
                            canonical.display()
                        )));
                    }

                    let em = entry.path().symlink_metadata().map_err(|e| {
                        failed(format!(
                            "failed to inspect entry in {target_kind} directory '{}': {e}",
                            entry.path().display()
                        ))
                    })?;

                    if em.is_file() && em.nlink() > 1 {
                        collect_opaque_inodes_for_dev(
                            em.dev(),
                            opaque_runtime_roots,
                            protected_inodes,
                            scanned_opaque_roots,
                        )?;
                    }

                    if protected_inodes.contains(&(em.dev(), em.ino())) {
                        return Err(failed(format!(
                            "{target_kind} directory '{}' contains hardlink to protected path at '{}'",
                            canonical.display(),
                            entry.path().display()
                        )));
                    }

                    if em.is_dir() && !em.file_type().is_symlink() {
                        if depth >= max_depth {
                            return Err(failed(format!(
                                "{target_kind} directory '{}' depth exceeds maximum limit of {max_depth} at '{}'",
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

        validate_inode_against_protected(
            canonical,
            &mut protected_inodes,
            opaque_runtime_roots,
            &mut scanned_opaque_roots,
            "capability target",
            Some(10_000),
        )
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

fn apply_git_mounts(
    command: &mut Command,
    ws_target: &Path,
    spec: &ProcessSpec,
    protected_inodes: &mut HashSet<(u64, u64)>,
    policy: &EffectivePolicy,
    placeholders: &mut PlaceholderGuard,
) -> Result<(), RuntimeError> {
    use crate::git::discovery::GitRepositoryInfo;
    use std::os::unix::fs::MetadataExt;

    let read_only_workspace = spec.read_only_workspace;
    let allow_config_write = spec.allow_git_config_write;
    let protected_paths = &spec.protected_paths;
    let opaque_roots = &spec.opaque_runtime_roots;

    command.args(["--setenv", "GIT_TERMINAL_PROMPT", "0"]);
    command.args(["--setenv", "GIT_PAGER", "cat"]);

    if read_only_workspace {
        command.args(["--setenv", "GIT_OPTIONAL_LOCKS", "0"]);
    }

    let Some(repo) = GitRepositoryInfo::discover(ws_target)
        .map_err(|e| failed(format!("failed to inspect git repository: {e}")))?
    else {
        return Ok(());
    };

    for directory in [&repo.git_dir, &repo.common_dir] {
        if directory.starts_with(ws_target) {
            continue;
        }
        let allowed = policy
            .persistent_read_write
            .iter()
            .any(|p| directory.starts_with(p.canonical()))
            || (read_only_workspace
                && policy
                    .persistent_read
                    .iter()
                    .any(|p| directory.starts_with(p.canonical())))
            || policy
                .session_grants
                .read()
                .map_err(|_| failed("grant lock poisoned"))?
                .grants()
                .iter()
                .any(|g| {
                    g.environment.is_local()
                        && directory.starts_with(g.path.canonical())
                        && if read_only_workspace { g.read } else { g.write }
                });

        if !allowed {
            return Err(failed(format!(
                "external Git metadata '{}' requires request_capability",
                directory.display()
            )));
        }

        validate_mount_source(directory, policy)?;
        if protected_paths
            .iter()
            .any(|p| p.starts_with(directory) || directory.starts_with(p))
        {
            return Err(failed(
                "external Git metadata conflicts with protected path",
            ));
        }

        validate_inode_against_protected(
            directory,
            protected_inodes,
            opaque_roots,
            &mut HashSet::new(),
            "Git metadata",
            Some(10_000),
        )?;
    }

    // Config/hook bind sources must not turn a symlink into an unauthorized host mount.
    for source in [
        repo.config_path(),
        repo.worktree_config_path(),
        repo.hooks_path(),
    ] {
        if !source.exists() {
            continue;
        }
        let canonical = source.canonicalize().map_err(failed)?;
        if protected_paths
            .iter()
            .any(|p| p.starts_with(&canonical) || canonical.starts_with(p))
        {
            return Err(failed(
                "Git configuration or hooks conflict with a protected path",
            ));
        }

        if !canonical.starts_with(ws_target) {
            let needs_write = allow_config_write && !read_only_workspace;
            let allowed = policy
                .persistent_read_write
                .iter()
                .any(|p| canonical.starts_with(p.canonical()))
                || (!needs_write
                    && policy
                        .persistent_read
                        .iter()
                        .any(|p| canonical.starts_with(p.canonical())))
                || policy
                    .session_grants
                    .read()
                    .map_err(|_| failed("grant lock poisoned"))?
                    .grants()
                    .iter()
                    .any(|g| {
                        g.environment.is_local()
                            && canonical.starts_with(g.path.canonical())
                            && if needs_write { g.write } else { g.read }
                    });

            if !allowed {
                return Err(failed(
                    "external Git configuration or hooks require request_capability",
                ));
            }

            validate_mount_source(&canonical, policy)?;
        }

        validate_inode_against_protected(
            &canonical,
            protected_inodes,
            opaque_roots,
            &mut HashSet::new(),
            "Git config/hooks",
            Some(10_000),
        )?;
    }

    if repo.is_gitfile {
        for directory in [&repo.git_dir, &repo.common_dir] {
            if !directory.starts_with(ws_target) {
                command
                    .arg(if read_only_workspace {
                        "--ro-bind"
                    } else {
                        "--bind"
                    })
                    .arg(directory)
                    .arg(directory);
            }
        }
    }

    if !read_only_workspace && !allow_config_write {
        for source in [
            repo.config_path(),
            repo.worktree_config_path(),
            repo.hooks_path(),
        ] {
            let target = if let Ok(relative) = source.strip_prefix(ws_target) {
                Path::new("/workspace").join(relative)
            } else {
                source.clone()
            };
            let directory = source == repo.hooks_path();
            if !source.exists() {
                use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
                if directory {
                    std::fs::DirBuilder::new()
                        .mode(0o700)
                        .create(&source)
                        .map_err(failed)?;
                } else {
                    std::fs::OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(&source)
                        .map_err(failed)?;
                }

                let meta = std::fs::symlink_metadata(&source).map_err(failed)?;
                placeholders.push(if directory {
                    Placeholder::Dir {
                        path: source.clone(),
                        dev: meta.dev(),
                        ino: meta.ino(),
                    }
                } else {
                    Placeholder::File {
                        path: source.clone(),
                        dev: meta.dev(),
                        ino: meta.ino(),
                    }
                });
            }

            command.arg("--ro-bind").arg(&source).arg(target);
            let meta = std::fs::symlink_metadata(&source).map_err(failed)?;
            protected_inodes.insert((meta.dev(), meta.ino()));
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

#[cfg(test)]
fn collect_masked_paths(
    ws_target: &Path,
    protected_paths: &[PathBuf],
    opaque_runtime_roots: &[PathBuf],
    read_only_workspace: bool,
    unmask_git: bool,
    placeholder_guard: &mut PlaceholderGuard,
) -> Result<MaskedPathsAndInodes, RuntimeError> {
    collect_masked_paths_with_templates(
        ws_target,
        protected_paths,
        opaque_runtime_roots,
        read_only_workspace,
        unmask_git,
        placeholder_guard,
        &[],
        &[],
    )
}

#[allow(clippy::too_many_arguments)]
fn collect_masked_paths_with_templates(
    ws_target: &Path,
    protected_paths: &[PathBuf],
    opaque_runtime_roots: &[PathBuf],
    read_only_workspace: bool,
    unmask_git: bool,
    placeholder_guard: &mut PlaceholderGuard,
    read_templates: &[PathBuf],
    write_templates: &[PathBuf],
) -> Result<MaskedPathsAndInodes, RuntimeError> {
    use std::os::unix::fs::MetadataExt;

    let mut masked = Vec::new();
    let mut seen = HashSet::new();
    let mut protected_inodes = HashSet::new();

    let mut all_protected = protected_paths.to_vec();

    if !read_only_workspace {
        let names = if unmask_git {
            &[".env", ".env.local"][..]
        } else {
            &[".git", ".env", ".env.local"][..]
        };

        for name in names {
            let candidate = ws_target.join(name);

            if !all_protected.contains(&candidate) {
                all_protected.push(candidate);
            }
        }
    }

    for protected in &all_protected {
        if unmask_git && protected.file_name().is_some_and(|n| n == ".git") {
            continue;
        }
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

            let is_protected_name = if (unmask_git && name == ".git")
                || read_templates.contains(&child_host)
                || write_templates.contains(&child_host)
            {
                false
            } else {
                crate::filesystem::path_security::is_protected_workspace_name(&name)
            };

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

    for template in read_templates.iter().chain(write_templates.iter()) {
        let metadata = std::fs::metadata(template).map_err(failed)?;
        if protected_inodes.contains(&(metadata.dev(), metadata.ino())) {
            return Err(failed("trusted template aliases a protected file"));
        }
    }
    // A read-only template's hardlink aliases must not provide a writable route.
    for template in read_templates
        .iter()
        .filter(|p| !write_templates.contains(p))
    {
        if let Ok(metadata) = std::fs::symlink_metadata(template) {
            protected_inodes.insert((metadata.dev(), metadata.ino()));
        }
    }

    for entry in files_to_check {
        if protected_inodes.contains(&(entry.dev, entry.ino))
            && !(read_templates.contains(&ws_target.join(&entry.relative))
                && !protected_paths.contains(&ws_target.join(&entry.relative)))
        {
            let container_path = Path::new("/workspace").join(&entry.relative);

            if seen.insert(container_path.clone()) {
                masked.push((container_path, entry.is_dir));
            }
        }
    }

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

    let opaque_roots = policy
        .opaque_runtime_roots
        .read()
        .map(|guard| guard.clone())
        .unwrap_or_default();

    for path in &opaque_roots {
        if canonical.starts_with(path) || path.starts_with(&canonical) {
            return Err(failed(format!(
                "mount source '{}' overlaps protected path '{}'",
                canonical.display(),
                path.display()
            )));
        }
    }

    let mut collected = collect_all_protected_inodes(policy)?;
    let mut scanned_opaque_roots = HashSet::new();
    validate_inode_against_protected(
        &canonical,
        &mut collected.inodes,
        &collected.opaque_roots,
        &mut scanned_opaque_roots,
        "mount source",
        None,
    )?;

    Ok(canonical)
}

fn validate_toolchain_source(
    source: &Path,
    policy: &EffectivePolicy,
) -> Result<PathBuf, RuntimeError> {
    let canonical = source.canonicalize().map_err(|error| {
        failed(format!(
            "cannot access configured Rust toolchain '{}': {error}",
            source.display()
        ))
    })?;

    if canonical == Path::new("/")
        || canonical.starts_with("/proc")
        || canonical.starts_with("/dev")
        || canonical.starts_with("/sys")
        || canonical.starts_with("/etc")
    {
        return Err(failed(format!(
            "toolchain source '{}' is a reserved system path",
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
                "toolchain source '{}' overlaps protected path '{}'",
                canonical.display(),
                path.display()
            )));
        }
    }

    let opaque_roots = policy
        .opaque_runtime_roots
        .read()
        .map(|guard| guard.clone())
        .unwrap_or_default();

    for path in &opaque_roots {
        if canonical.starts_with(path) || path.starts_with(&canonical) {
            return Err(failed(format!(
                "toolchain source '{}' overlaps protected path '{}'",
                canonical.display(),
                path.display()
            )));
        }
    }

    let mut collected = collect_all_protected_inodes(policy)?;
    let mut scanned_opaque_roots = HashSet::new();

    if canonical == Path::new("/usr") {
        let bin = canonical.join("bin");
        if bin.is_dir() {
            validate_inode_against_protected(
                &bin,
                &mut collected.inodes,
                &collected.opaque_roots,
                &mut scanned_opaque_roots,
                "toolchain source",
                Some(10_000),
            )?;
        }

        let rustlib = canonical.join("lib/rustlib");
        if rustlib.is_dir() {
            validate_inode_against_protected(
                &rustlib,
                &mut collected.inodes,
                &collected.opaque_roots,
                &mut scanned_opaque_roots,
                "toolchain source",
                Some(10_000),
            )?;
        }
    } else {
        validate_inode_against_protected(
            &canonical,
            &mut collected.inodes,
            &collected.opaque_roots,
            &mut scanned_opaque_roots,
            "toolchain source",
            None,
        )?;
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
        let path = validate_toolchain_source(path, policy)?;

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

pub fn resolve_container_cwd(
    ws_target: &Path,
    mounts: &[MountSpec],
    cwd: Option<&Path>,
) -> PathBuf {
    let Some(cwd) = cwd else {
        return PathBuf::from("/workspace");
    };

    if cwd.as_os_str().is_empty() || cwd == Path::new(".") {
        return PathBuf::from("/workspace");
    }

    if cwd.starts_with("/workspace") {
        return cwd.to_path_buf();
    }

    let cwd_canon = cwd.canonicalize().ok();

    if let Ok(rel) = cwd.strip_prefix(ws_target) {
        if rel.as_os_str().is_empty() {
            return PathBuf::from("/workspace");
        } else {
            return Path::new("/workspace").join(rel);
        }
    }

    if let Some(rel) = cwd_canon
        .as_ref()
        .and_then(|cc| cc.strip_prefix(ws_target).ok())
    {
        return if rel.as_os_str().is_empty() {
            PathBuf::from("/workspace")
        } else {
            Path::new("/workspace").join(rel)
        };
    }

    let ws_canon = ws_target.canonicalize().ok();
    if let Some(rel) = ws_canon.as_ref().and_then(|wc| cwd.strip_prefix(wc).ok()) {
        return if rel.as_os_str().is_empty() {
            PathBuf::from("/workspace")
        } else {
            Path::new("/workspace").join(rel)
        };
    }

    if let Some(rel) = ws_canon
        .as_ref()
        .and_then(|wc| cwd_canon.as_ref().and_then(|cc| cc.strip_prefix(wc).ok()))
    {
        return if rel.as_os_str().is_empty() {
            PathBuf::from("/workspace")
        } else {
            Path::new("/workspace").join(rel)
        };
    }

    for mount in mounts {
        let Ok(target) = mount.normalized_target() else {
            continue;
        };
        let mount_source_canon = mount.source.canonicalize().ok();
        if let Ok(rel) = cwd.strip_prefix(&mount.source) {
            return if rel.as_os_str().is_empty() {
                target
            } else {
                target.join(rel)
            };
        }

        if let Some(rel) = mount_source_canon
            .as_ref()
            .and_then(|sc| cwd.strip_prefix(sc).ok())
        {
            return if rel.as_os_str().is_empty() {
                target
            } else {
                target.join(rel)
            };
        }

        if let Some(rel) = cwd_canon
            .as_ref()
            .and_then(|cc| cc.strip_prefix(&mount.source).ok())
        {
            return if rel.as_os_str().is_empty() {
                target
            } else {
                target.join(rel)
            };
        }

        if let Some(rel) = mount_source_canon
            .as_ref()
            .and_then(|sc| cwd_canon.as_ref().and_then(|cc| cc.strip_prefix(sc).ok()))
        {
            return if rel.as_os_str().is_empty() {
                target
            } else {
                target.join(rel)
            };
        }

        if cwd.starts_with(&target) {
            return cwd.to_path_buf();
        }
    }

    if cwd.is_relative() {
        return Path::new("/workspace").join(cwd);
    }

    cwd.to_path_buf()
}

fn mount_scratch_and_tmp(
    command: &mut Command,
    scratch: Option<&Path>,
    task_tmp: Option<&Path>,
    max_tmp_bytes: u64,
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

    command.args(["--proc", "/proc", "--dev", "/dev"]);

    if max_tmp_bytes == 0
        && let Some(tmp_dir) = task_tmp
    {
        if scratch.is_none() {
            use std::os::unix::fs::DirBuilderExt;

            let tmp_home = tmp_dir.join("home");
            let _ = std::fs::DirBuilder::new().mode(0o700).create(&tmp_home);
        }

        command.arg("--bind").arg(tmp_dir).arg("/tmp");
    } else {
        let size = if max_tmp_bytes == 0 {
            TMP_SIZE_BYTES
        } else {
            max_tmp_bytes
        };

        command.args(["--size", &size.to_string(), "--tmpfs", "/tmp"]);

        if scratch.is_none() {
            command.args(["--dir", "/tmp/home"]);
        }
    }

    Ok(())
}

fn is_valid_container_path(
    path_str: &str,
    mounts: &[MountSpec],
    policy: Option<&EffectivePolicy>,
) -> bool {
    let p = Path::new(path_str);
    if p.starts_with("/workspace")
        || p.starts_with("/scratch")
        || p.starts_with("/tmp")
        || p.starts_with("/opt/orbit-rust")
    {
        return true;
    }

    for m in mounts {
        if m.normalized_target()
            .map(|target| p == target || p.starts_with(&target))
            .unwrap_or(false)
        {
            return true;
        }
    }

    if let Some(policy) = policy {
        for cap in &policy.persistent_read {
            let c = cap.canonical();
            if p == c || p.starts_with(c) {
                return true;
            }
        }

        for cap in &policy.persistent_read_write {
            let c = cap.canonical();
            if p == c || p.starts_with(c) {
                return true;
            }
        }

        if let Ok(store) = policy.session_grants.read() {
            for grant in store.grants() {
                if grant.environment.is_local() {
                    let c = grant.path.canonical();
                    if p == c || p.starts_with(c) {
                        return true;
                    }
                }
            }
        }
    }

    false
}

#[allow(clippy::too_many_arguments)]
fn configure_limits_and_entrypoint(
    command: &mut Command,
    prlimit: &Path,
    limits: &ProcessLimits,
    has_rust: bool,
    is_writable_tmpfs: bool,
    persistent_home: bool,
    custom_env: &std::collections::BTreeMap<String, String>,
    mounts: &[MountSpec],
    policy: Option<&EffectivePolicy>,
) {
    let default_home = if persistent_home {
        "/scratch/home"
    } else {
        "/tmp/home"
    };

    let path_val = if let Some(p) = custom_env.get("PATH") {
        if has_rust && !p.split(':').any(|part| part == "/opt/orbit-rust/bin") {
            format!("/opt/orbit-rust/bin:{p}")
        } else {
            p.clone()
        }
    } else if has_rust {
        "/opt/orbit-rust/bin:/usr/bin:/bin".to_string()
    } else {
        "/usr/bin:/bin".to_string()
    };

    let home_val = if let Some(h) = custom_env.get("HOME") {
        if is_valid_container_path(h, mounts, policy) {
            h.clone()
        } else {
            default_home.to_string()
        }
    } else {
        default_home.to_string()
    };

    let tmpdir_val = if let Some(t) = custom_env.get("TMPDIR") {
        if is_valid_container_path(t, mounts, policy) {
            t.clone()
        } else {
            "/tmp".to_string()
        }
    } else {
        "/tmp".to_string()
    };

    let cargo_home = if let Some(c) = custom_env.get("CARGO_HOME") {
        if is_valid_container_path(c, mounts, policy) {
            Some(c.clone())
        } else {
            Some(format!("{default_home}/.cargo"))
        }
    } else if has_rust {
        Some(format!("{default_home}/.cargo"))
    } else {
        None
    };

    let rustup_home = if let Some(r) = custom_env.get("RUSTUP_HOME") {
        if is_valid_container_path(r, mounts, policy) {
            Some(r.clone())
        } else {
            Some(format!("{default_home}/.rustup"))
        }
    } else if has_rust {
        Some(format!("{default_home}/.rustup"))
    } else {
        None
    };

    let mut env_map = std::collections::BTreeMap::new();
    env_map.insert("PATH".to_string(), path_val);
    env_map.insert("HOME".to_string(), home_val);
    env_map.insert("TMPDIR".to_string(), tmpdir_val);
    if let Some(c) = cargo_home {
        env_map.insert("CARGO_HOME".to_string(), c);
    }
    if let Some(r) = rustup_home {
        env_map.insert("RUSTUP_HOME".to_string(), r);
    }
    env_map.insert(
        "LANG".to_string(),
        custom_env
            .get("LANG")
            .cloned()
            .unwrap_or_else(|| "C.UTF-8".to_string()),
    );
    env_map.insert(
        "TERM".to_string(),
        custom_env
            .get("TERM")
            .cloned()
            .unwrap_or_else(|| "xterm-256color".to_string()),
    );
    env_map.insert(
        "COLORTERM".to_string(),
        custom_env
            .get("COLORTERM")
            .cloned()
            .unwrap_or_else(|| "truecolor".to_string()),
    );
    env_map.insert(
        "CLICOLOR_FORCE".to_string(),
        custom_env
            .get("CLICOLOR_FORCE")
            .cloned()
            .unwrap_or_else(|| "1".to_string()),
    );

    for (k, v) in custom_env {
        if matches!(
            k.as_str(),
            "PATH"
                | "HOME"
                | "TMPDIR"
                | "LANG"
                | "TERM"
                | "COLORTERM"
                | "CLICOLOR_FORCE"
                | "CARGO_HOME"
                | "RUSTUP_HOME"
        ) {
            continue;
        }

        if !crate::execution::environment::is_sensitive_key(k) {
            env_map.insert(k.clone(), v.clone());
        }
    }

    for (k, v) in &env_map {
        command.arg("--setenv").arg(k).arg(v);
    }

    command
        .arg("--")
        .arg(prlimit.to_str().unwrap_or("/usr/bin/prlimit"));

    if limits.max_memory_bytes != 0 && limits.max_memory_bytes != u64::MAX {
        command.arg(format!(
            "--as={}:{}",
            limits.max_memory_bytes, limits.max_memory_bytes
        ));
    }

    if limits.max_file_size_bytes != 0 && limits.max_file_size_bytes != u64::MAX {
        command.arg(format!(
            "--fsize={}:{}",
            limits.max_file_size_bytes, limits.max_file_size_bytes
        ));
    }

    if limits.max_open_files != 0 && limits.max_open_files != u64::MAX {
        command.arg(format!(
            "--nofile={}:{}",
            limits.max_open_files, limits.max_open_files
        ));
    }

    if limits.max_processes != 0 && limits.max_processes != u64::MAX {
        command.arg(format!(
            "--nproc={}:{}",
            limits.max_processes, limits.max_processes
        ));
    }

    command.args(["--", "/bin/sh"]);

    if is_writable_tmpfs {
        command.args([
            "-c",
            "cp -R /orbit-source/. /workspace/ && exec /bin/sh /orbit-command.sh",
        ]);
    } else {
        command.arg("/orbit-command.sh");
    }
}

struct Captured {
    bytes: Vec<u8>,
    total: u64,
    retained_truncated: bool,
}

async fn capture(
    mut pipe: impl AsyncRead + Unpin,
    mut retained: Option<super::output::RetainedOutput>,
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
                retained_truncated: retaining
                    && retained.as_ref().is_some_and(|file| file.written() < total),
            });
        }

        total = total.saturating_add(count as u64);

        if let Some(file) = retained.as_mut() {
            file.append(&buffer[..count]).await?;
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

    let task_tmp = tempfile::Builder::new()
        .prefix("orbit-task-tmp-")
        .tempdir()
        .map_err(failed)?;

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
        Some(task_tmp.path()),
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
mod tests;
