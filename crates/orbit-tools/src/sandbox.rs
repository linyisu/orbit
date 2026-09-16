use std::{
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use orbit_agent::tool::ToolError;
use serde::Serialize;
use tempfile::TempDir;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
};
use tokio_util::sync::CancellationToken;

use crate::workspace::{Workspace, WorkspaceError};

const MAX_OUTPUT: usize = 32 * 1024;
const MAX_SNAPSHOT: usize = 64 * 1024 * 1024;

#[derive(Debug, Serialize)]
pub struct CommandResult {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub cancelled: bool,
    pub output_truncated: bool,
    pub sandbox: &'static str,
    pub workspace_files: usize,
    pub skipped_entries: usize,
}

struct Snapshot {
    directory: TempDir,
    files: usize,
    skipped: usize,
    bytes: usize,
    visited: usize,
}

impl Snapshot {
    fn create(workspace: Workspace, cancellation: CancellationToken) -> Result<Self, ToolError> {
        let directory = tempfile::Builder::new()
            .prefix("orbit-sandbox-")
            .tempdir()
            .map_err(failed)?;
        let mut snapshot = Self {
            directory,
            files: 0,
            skipped: 0,
            bytes: 0,
            visited: 0,
        };
        snapshot.copy_directory(&workspace, Path::new(""), &cancellation, 0)?;
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
            if self.visited > 10_000 {
                return Err(failed("workspace snapshot exceeds 10000 entries"));
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
            let destination = self.directory.path().join(&path);
            // All opens use the workspace's directory descriptors and protection
            // policy. No source symlinks or protected hard-link aliases are copied.
            match workspace.open_read(&path) {
                Ok(file) => {
                    let available = MAX_SNAPSHOT.saturating_sub(self.bytes);
                    let mut content = Vec::new();
                    file.take(available as u64 + 1)
                        .read_to_end(&mut content)
                        .map_err(failed)?;
                    if content.len() > available {
                        return Err(failed(
                            "workspace snapshot exceeds 64 MiB; no command was started",
                        ));
                    }
                    self.bytes += content.len();
                    std::fs::write(destination, content).map_err(failed)?;
                    self.files += 1;
                }
                Err(WorkspaceError::NotFile) => {
                    // Only recurse when a no-follow directory open succeeds; FIFOs,
                    // sockets and devices are deliberately absent from the snapshot.
                    if workspace.directory_entries(&path).is_ok() {
                        std::fs::create_dir(&destination).map_err(failed)?;
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

fn command(snapshot: &Path, script: &str) -> Result<Command, ToolError> {
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
    command.arg("--ro-bind").arg(snapshot).arg("/workspace");
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
        "/usr/bin:/bin",
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
        "--cpu=30:30",
        "--as=1073741824:1073741824",
        "--fsize=67108864:67108864",
        "--nofile=64:64",
        "--nproc=256:256",
        "--",
        "/bin/sh",
        "-c",
        script,
        "orbit-command",
    ]);
    Ok(command)
}

async fn capture(
    mut pipe: impl AsyncRead + Unpin,
    limit: CancellationToken,
    truncated: Arc<AtomicBool>,
) -> std::io::Result<Vec<u8>> {
    let mut result = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let count = pipe.read(&mut buffer).await?;
        if count == 0 {
            return Ok(result);
        }
        let keep = count.min(MAX_OUTPUT.saturating_sub(result.len()));
        result.extend_from_slice(&buffer[..keep]);
        if keep < count {
            truncated.store(true, Ordering::Relaxed);
            limit.cancel();
        }
    }
}

pub async fn run(
    workspace: Workspace,
    script: String,
    timeout: Duration,
    token: CancellationToken,
) -> Result<CommandResult, ToolError> {
    if token.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    let copy_token = token.clone();
    let snapshot = tokio::task::spawn_blocking(move || Snapshot::create(workspace, copy_token))
        .await
        .map_err(failed)??;
    if token.is_cancelled() {
        return Err(ToolError::Cancelled);
    }
    let mut child = command(snapshot.directory.path(), &script)?
        .spawn()
        .map_err(failed)?;
    let limit = CancellationToken::new();
    let truncated = Arc::new(AtomicBool::new(false));
    let stdout = capture(
        child.stdout.take().expect("piped stdout"),
        limit.clone(),
        truncated.clone(),
    );
    let stderr = capture(
        child.stderr.take().expect("piped stderr"),
        limit.clone(),
        truncated.clone(),
    );
    let completion = async {
        let mut timed_out = false;
        let mut cancelled = false;
        let status = tokio::select! {
            result = child.wait() => result,
            _ = token.cancelled() => { cancelled = true; child.start_kill()?; child.wait().await },
            _ = limit.cancelled() => { child.start_kill()?; child.wait().await },
            _ = tokio::time::sleep(timeout) => { timed_out = true; child.start_kill()?; child.wait().await },
        }?;
        Ok::<_, std::io::Error>((status, timed_out, cancelled))
    };
    let (result, stdout, stderr) = tokio::join!(completion, stdout, stderr);
    let (status, timed_out, cancelled) = result.map_err(failed)?;
    let stdout = String::from_utf8_lossy(&stdout.map_err(failed)?).into_owned();
    let stderr = String::from_utf8_lossy(&stderr.map_err(failed)?).into_owned();
    if !status.success() && stderr.starts_with("bwrap:") {
        return Err(failed(format!(
            "sandbox startup failed; refusing unsandboxed execution: {stderr}"
        )));
    }
    Ok(CommandResult {
        exit_code: status.code(),
        stdout,
        stderr,
        timed_out,
        cancelled,
        output_truncated: truncated.load(Ordering::Relaxed),
        sandbox: "bubblewrap; no network; read-only filtered workspace snapshot",
        workspace_files: snapshot.files,
        skipped_entries: snapshot.skipped,
    })
}

fn failed(error: impl std::fmt::Display) -> ToolError {
    ToolError::ExecutionFailed(error.to_string())
}
