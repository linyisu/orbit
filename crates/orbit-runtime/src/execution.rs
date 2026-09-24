use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};

use serde::Serialize;
use tempfile::TempDir;
use tokio::{io::AsyncReadExt, process::Command};
use tokio_util::sync::CancellationToken;

use crate::{
    error::{RuntimeError, failed},
    filesystem::{Workspace, WorkspaceError},
};

const MAX_ENTRIES: usize = 10_000;
const MAX_DEPTH: usize = 32;
const MAX_SNAPSHOT_BYTES: usize = 64 * 1024 * 1024;
const MAX_OUTPUT_BYTES: usize = 32 * 1024;

#[derive(Clone, Debug)]
pub struct CommandSpec {
    pub command: String,
    pub timeout: Duration,
    pub rust_toolchain: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
pub struct CommandResult {
    pub exit_code: Option<i32>,
    pub elapsed_ms: u64,
    pub stdout: String,
    pub stderr: String,
    pub stdout_total_bytes: usize,
    pub stderr_total_bytes: usize,
    pub output_truncated: bool,
    pub timed_out: bool,
    pub sandbox: &'static str,
}

struct Snapshot {
    directory: TempDir,
    entries: usize,
    bytes: usize,
}

impl Snapshot {
    fn create(
        workspace: &Workspace,
        cancellation: &CancellationToken,
    ) -> Result<Self, RuntimeError> {
        let directory = tempfile::Builder::new()
            .prefix("orbit-command-")
            .tempdir()
            .map_err(failed)?;
        fs::create_dir(directory.path().join("workspace")).map_err(failed)?;
        fs::create_dir(directory.path().join("scratch")).map_err(failed)?;
        let mut snapshot = Self {
            directory,
            entries: 0,
            bytes: 0,
        };
        snapshot.copy_directory(workspace, Path::new(""), cancellation, 0)?;
        Ok(snapshot)
    }

    fn copy_directory(
        &mut self,
        workspace: &Workspace,
        relative: &Path,
        cancellation: &CancellationToken,
        depth: usize,
    ) -> Result<(), RuntimeError> {
        if cancellation.is_cancelled() {
            return Err(RuntimeError::Cancelled);
        }
        if depth > MAX_DEPTH {
            return Err(failed("workspace snapshot exceeds maximum directory depth"));
        }

        for name in workspace.directory_entries(relative)? {
            if cancellation.is_cancelled() {
                return Err(RuntimeError::Cancelled);
            }
            self.entries += 1;
            if self.entries > MAX_ENTRIES {
                return Err(failed("workspace snapshot exceeds maximum entry count"));
            }
            if matches!(
                name.to_str(),
                Some(".git" | ".env" | "target" | "node_modules" | ".venv" | "__pycache__")
            ) || name.to_str().is_some_and(|name| name.starts_with(".env."))
            {
                continue;
            }

            let child = relative.join(name);
            let source = workspace.root().join(&child);
            let metadata = fs::symlink_metadata(&source).map_err(failed)?;
            if metadata.file_type().is_symlink() {
                continue;
            }

            let destination = self.directory.path().join("workspace").join(&child);
            if metadata.is_dir() {
                match workspace.open_directory(&child) {
                    Ok(_) => {}
                    Err(WorkspaceError::ProtectedPath) => continue,
                    Err(error) => return Err(error.into()),
                }
                fs::create_dir(&destination).map_err(failed)?;
                self.copy_directory(workspace, &child, cancellation, depth + 1)?;
            } else if metadata.is_file() {
                let mut source = match workspace.open_read(&child) {
                    Ok(source) => source,
                    Err(WorkspaceError::ProtectedPath) => continue,
                    Err(error) => return Err(error.into()),
                };
                let available = MAX_SNAPSHOT_BYTES.saturating_sub(self.bytes);
                let mut content = Vec::new();
                Read::by_ref(&mut source)
                    .take(available as u64 + 1)
                    .read_to_end(&mut content)
                    .map_err(failed)?;
                if content.len() > available {
                    return Err(failed("workspace snapshot exceeds 64 MiB"));
                }
                self.bytes += content.len();
                fs::write(&destination, content).map_err(failed)?;
                let mode = if metadata.mode() & 0o111 == 0 {
                    0o600
                } else {
                    0o700
                };
                fs::set_permissions(&destination, fs::Permissions::from_mode(mode))
                    .map_err(failed)?;
            }
        }
        Ok(())
    }
}

fn bwrap_path() -> Result<PathBuf, RuntimeError> {
    for path in ["/usr/bin/bwrap", "/bin/bwrap"] {
        let path = Path::new(path);
        if let Ok(metadata) = path.metadata()
            && metadata.is_file()
            && metadata.mode() & 0o6000 == 0
        {
            return Ok(path.to_path_buf());
        }
    }
    Err(failed(
        "sandbox unavailable: install non-setuid Bubblewrap; refusing unsandboxed execution",
    ))
}

fn sandbox_command(
    snapshot: &Snapshot,
    script: &Path,
    rust_toolchain: Option<&Path>,
) -> Result<Command, RuntimeError> {
    let mut command = Command::new(bwrap_path()?);
    if !Path::new("/usr/bin/prlimit").is_file() {
        return Err(failed(
            "sandbox unavailable: /usr/bin/prlimit is required for resource limits",
        ));
    }
    command
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .args([
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
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--size",
            "67108864",
            "--tmpfs",
            "/tmp",
            "--dir",
            "/etc",
            "--dir",
            "/opt",
        ]);

    for path in ["/usr", "/bin", "/sbin", "/lib", "/lib64"] {
        if Path::new(path).exists() {
            command.args(["--ro-bind", path, path]);
        }
    }
    for path in [
        "/etc/ld.so.cache",
        "/etc/resolv.conf",
        "/etc/hosts",
        "/etc/ssl",
        "/etc/pki",
        "/etc/ca-certificates",
    ] {
        if Path::new(path).exists() {
            command.arg("--ro-bind").arg(path).arg(path);
        }
    }

    command
        .arg("--bind")
        .arg(snapshot.directory.path().join("workspace"))
        .arg("/workspace")
        .arg("--bind")
        .arg(snapshot.directory.path().join("scratch"))
        .arg("/scratch")
        .arg("--ro-bind")
        .arg(script)
        .arg("/orbit-command.sh");
    if let Some(toolchain) = rust_toolchain {
        command
            .arg("--ro-bind")
            .arg(toolchain)
            .arg("/opt/orbit-rust");
    }
    command
        .args([
            "--dir",
            "/scratch/home",
            "--chdir",
            "/workspace",
            "--setenv",
            "HOME",
            "/scratch/home",
            "--setenv",
            "TMPDIR",
            "/tmp",
            "--setenv",
            "CARGO_HOME",
            "/scratch/cargo",
            "--setenv",
            "CARGO_TARGET_DIR",
            "/scratch/target",
        ])
        .arg("--setenv")
        .arg("PATH")
        .arg(if rust_toolchain.is_some() {
            "/opt/orbit-rust/bin:/usr/local/bin:/usr/bin:/bin"
        } else {
            "/usr/local/bin:/usr/bin:/bin"
        })
        .args([
            "--",
            "/usr/bin/prlimit",
            "--as=4294967296",
            "--fsize=67108864",
            "--nproc=1024",
            "--nofile=1024",
            "--",
            "/bin/sh",
            "/orbit-command.sh",
        ]);
    Ok(command)
}

async fn read_bounded<R: tokio::io::AsyncRead + Unpin>(
    mut reader: R,
) -> Result<(String, usize, bool), RuntimeError> {
    let mut kept = Vec::new();
    let mut total = 0usize;
    let mut buffer = [0u8; 8192];

    loop {
        let count = reader.read(&mut buffer).await.map_err(failed)?;
        if count == 0 {
            break;
        }
        total = total.saturating_add(count);
        let remaining = MAX_OUTPUT_BYTES.saturating_sub(kept.len());
        kept.extend_from_slice(&buffer[..count.min(remaining)]);
    }

    Ok((
        String::from_utf8_lossy(&kept).into_owned(),
        total,
        total > kept.len(),
    ))
}

pub async fn run(
    workspace: Workspace,
    spec: CommandSpec,
    cancellation: CancellationToken,
) -> Result<CommandResult, RuntimeError> {
    if cancellation.is_cancelled() {
        return Err(RuntimeError::Cancelled);
    }
    let toolchain = match spec.rust_toolchain.as_deref() {
        Some(path) => {
            let canonical = path.canonicalize().map_err(failed)?;
            if !canonical.is_dir()
                || canonical.starts_with(workspace.root())
                || !canonical.join("bin/rustc").is_file()
                || !canonical.join("bin/cargo").is_file()
            {
                return Err(failed(
                    "rust_toolchain must be an external directory containing bin/rustc and bin/cargo",
                ));
            }
            Some(canonical)
        }
        None => None,
    };
    let snapshot_token = cancellation.clone();
    let snapshot =
        tokio::task::spawn_blocking(move || Snapshot::create(&workspace, &snapshot_token))
            .await
            .map_err(failed)??;
    let script = snapshot.directory.path().join("command.sh");
    let mut file = fs::File::create(&script).map_err(failed)?;
    file.write_all(spec.command.as_bytes()).map_err(failed)?;
    file.flush().map_err(failed)?;

    let start = Instant::now();
    let mut child = sandbox_command(&snapshot, &script, toolchain.as_deref())?
        .spawn()
        .map_err(failed)?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| failed("command stdout unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| failed("command stderr unavailable"))?;
    let stdout_task = tokio::spawn(read_bounded(stdout));
    let stderr_task = tokio::spawn(read_bounded(stderr));

    let (exit_code, timed_out, cancelled) = tokio::select! {
        biased;
        _ = cancellation.cancelled() => {
            child.start_kill().map_err(failed)?;
            child.wait().await.map_err(failed)?;
            (None, false, true)
        }
        _ = tokio::time::sleep(spec.timeout) => {
            child.start_kill().map_err(failed)?;
            child.wait().await.map_err(failed)?;
            (None, true, false)
        }
        status = child.wait() => (status.map_err(failed)?.code(), false, false),
    };

    let (stdout, stdout_total_bytes, stdout_truncated) =
        tokio::time::timeout(Duration::from_secs(2), stdout_task)
            .await
            .map_err(failed)?
            .map_err(failed)??;
    let (stderr, stderr_total_bytes, stderr_truncated) =
        tokio::time::timeout(Duration::from_secs(2), stderr_task)
            .await
            .map_err(failed)?
            .map_err(failed)??;
    if cancelled {
        return Err(RuntimeError::Cancelled);
    }
    if exit_code == Some(1) && stderr.starts_with("bwrap: ") {
        return Err(failed(format!(
            "sandbox failed to start: {}",
            stderr.trim()
        )));
    }

    Ok(CommandResult {
        exit_code,
        elapsed_ms: start.elapsed().as_millis().min(u64::MAX as u128) as u64,
        stdout,
        stderr,
        stdout_total_bytes,
        stderr_total_bytes,
        output_truncated: stdout_truncated || stderr_truncated,
        timed_out,
        sandbox: "bubblewrap; disposable workspace snapshot; network allowed",
    })
}
