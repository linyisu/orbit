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

use crate::workspace::{Workspace, WorkspaceError};

const MAX_OUTPUT: usize = 32 * 1024;
const MIB: usize = 1024 * 1024;
/// A single command may retain enough output for a large compiler or test log,
/// without making a runaway process capable of filling the user's disk.
pub const MAX_RETAINED_OUTPUT_PER_STREAM: u64 = 16 * 1024 * 1024;
pub const MAX_RETAINED_OUTPUT_PER_SESSION: u64 = 128 * 1024 * 1024;
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

fn command(
    snapshot: &Path,
    script: &Path,
    writable_snapshot: bool,
    _timeout: Duration,
    limits: &crate::run_command::CommandLimits,
    scratch: Option<&Path>,
) -> Result<Command, ToolError> {
    let rust_toolchain = limits.rust_toolchain.as_deref();
    use std::os::unix::fs::MetadataExt;
    let executable = PathBuf::from("/usr/bin/bwrap");
    let metadata = executable.metadata().map_err(|_| failed("sandbox unavailable: install Bubblewrap at /usr/bin/bwrap; refusing unsandboxed execution"))?;
    if !metadata.is_file() || metadata.mode() & 0o6000 != 0 {
        return Err(failed(
            "sandbox unavailable: setuid/setgid Bubblewrap is not supported",
        ));
    }
    if !Path::new("/usr/bin/prlimit").is_file() {
        return Err(failed(
            "sandbox unavailable: /usr/bin/prlimit is required for resource limits",
        ));
    }
    let mut command = Command::new(executable);
    command
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command.args([
        "--unshare-all",
        "--unshare-user",
        "--disable-userns",
        "--die-with-parent",
        "--new-session",
        "--cap-drop",
        "ALL",
    ]);
    for path in ["/usr", "/bin", "/sbin", "/lib", "/lib64"] {
        if Path::new(path).exists() {
            command.args(["--ro-bind", path, path]);
        }
    }
    if Path::new("/etc/ld.so.cache").exists() {
        command.args(["--ro-bind", "/etc/ld.so.cache", "/etc/ld.so.cache"]);
    }
    command
        .arg("--ro-bind")
        .arg(script)
        .arg("/orbit-command.sh");
    if writable_snapshot {
        command.arg("--ro-bind").arg(snapshot).arg("/orbit-source");
        command.args([
            "--size",
            &(limits.writable_snapshot_mib * MIB).to_string(),
            "--tmpfs",
            "/workspace",
        ]);
    } else {
        command.arg("--ro-bind").arg(snapshot).arg("/workspace");
    }
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
    for cache in &limits.dependency_caches {
        let path = cache.path.canonicalize().map_err(failed)?;
        if !path.is_dir() {
            return Err(failed("configured dependency cache must be a directory"));
        }
        command
            .arg("--ro-bind")
            .arg(path)
            .arg(format!("/opt/orbit-cache/{}", cache.name));
    }
    command.args([
        "--proc",
        "/proc",
        "--dev",
        "/dev",
        "--size",
        "67108864",
        "--tmpfs",
        "/tmp",
        "--dir",
        "/tmp/home",
        "--chdir",
        "/workspace",
        "--clearenv",
        "--setenv",
        "PATH",
        if rust_toolchain.is_some() {
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
        "/usr/bin/prlimit",
        "--as=4294967296:4294967296",
        "--fsize=67108864:67108864",
        "--nofile=1024:1024",
        "--nproc=1024:1024",
        "--",
        "/bin/sh",
    ]);
    if writable_snapshot {
        command.args([
            "-c",
            "cp -R /orbit-source/. /workspace/ && exec /bin/sh /orbit-command.sh",
        ]);
    } else {
        command.arg("/orbit-command.sh");
    }
    Ok(command)
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
    let mut child = command(
        snapshot.directory.as_ref().expect("copied snapshot").path(),
        script_file.path(),
        writable_snapshot,
        timeout,
        limits,
        scratch,
    )?
    .spawn()
    .map_err(failed)?;
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
            _ = token.cancelled() => { cancelled = true; child.start_kill()?; child.wait().await },
            _ = tokio::time::sleep(timeout) => { timed_out = true; child.start_kill()?; child.wait().await },
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
        sandbox: if writable_snapshot {
            "bubblewrap; no network; disposable writable workspace snapshot; changes discarded"
        } else {
            "bubblewrap; no network; read-only filtered workspace snapshot"
        },
        scratch: scratch.map(|_| "/scratch (persistent session files; no persistent processes)"),
        workspace_files: snapshot.files,
        skipped_entries: snapshot.skipped,
        verification: Some(VerificationEvidence {
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
        }),
    })
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
