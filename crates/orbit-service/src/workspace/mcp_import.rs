//! Explicit, target-owned MCP repository discovery and reviewed installation.
mod discover;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result, ensure};
use orbit_protocol::resources::{McpCandidate, Snapshot};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{io::AsyncReadExt, process::Command};

pub(super) struct Scan {
    storage: tempfile::TempDir,
    session: String,
    workspace: PathBuf,
    source: String,
    candidates: Vec<McpCandidate>,
    created: Instant,
}
impl Scan {
    pub(super) async fn acquire(
        session: String,
        workspace: PathBuf,
        source: String,
        home: Option<PathBuf>,
    ) -> Result<Self> {
        ensure!(
            !source.trim().is_empty() && source.len() <= 4096,
            "请输入有效的 MCP 来源地址或目录"
        );
        let home = home
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .context("找不到目标机器的个人目录")?;
        let cache = home.join(".local/share/orbit/mcp");
        reject_links(&cache)?;
        fs::create_dir_all(&cache)?;
        let storage = tempfile::Builder::new()
            .prefix("source-")
            .tempdir_in(cache)?;
        let root = super::skill_import::acquire_source(
            source.trim(),
            &workspace,
            Some(&home),
            storage.path(),
        )
        .await?;
        tokio::task::spawn_blocking(move || {
            let root = fs::canonicalize(root)?;
            let root = if root.is_file() {
                root.parent().context("来源目录无效")?.to_path_buf()
            } else {
                root
            };
            let copied = storage.path().join("source");
            let mut limits = (0, 0);
            copy_source(&root, &copied, 0, &mut limits)?;
            let candidates = discover::discover(&copied)?;
            Ok(Self {
                storage,
                session,
                workspace: fs::canonicalize(workspace)?,
                source,
                candidates,
                created: Instant::now(),
            })
        })
        .await?
    }
    fn token(&self) -> String {
        self.storage
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    }
    pub(super) fn snapshot(&self) -> Snapshot {
        Snapshot::McpSource {
            token: self.token(),
            source: self.source.clone(),
            candidates: self.candidates.clone(),
        }
    }
    pub(super) async fn install(
        self,
        token: &str,
        id: &str,
        session: &str,
        workspace: &Path,
    ) -> Result<Snapshot> {
        ensure!(
            self.token() == token
                && self.session == session
                && self.workspace == fs::canonicalize(workspace)?,
            "MCP 来源已改变，请重新扫描"
        );
        ensure!(
            self.created.elapsed() < Duration::from_secs(900),
            "MCP 来源已过期，请重新扫描"
        );
        let candidate = self
            .candidates
            .iter()
            .find(|entry| entry.id == id)
            .context("启动入口不在此次扫描结果中")?
            .clone();
        let directory = self
            .storage
            .path()
            .join("source")
            .join(discover::candidate_directory(&candidate.id)?);
        reject_links(&directory)?;
        for command in &candidate.install {
            run(command, &directory).await?;
        }
        // The path does not change after installation, including virtualenv scripts.
        let _installed = self.storage.keep();
        Ok(Snapshot::McpInstalled {
            name: candidate.name,
            launch: candidate.launch,
        })
    }
}
fn reject_links(path: &Path) -> Result<()> {
    for part in path.ancestors() {
        if let Ok(metadata) = fs::symlink_metadata(part) {
            ensure!(
                !metadata.file_type().is_symlink(),
                "MCP 来源不能经过符号链接：{}",
                part.display()
            );
        }
    }
    Ok(())
}
fn copy_source(
    source: &Path,
    target: &Path,
    depth: usize,
    limits: &mut (u64, usize),
) -> Result<()> {
    ensure!(depth <= 24, "MCP 来源目录层级过深");
    reject_links(source)?;
    fs::create_dir(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if [".git", "node_modules", ".venv", "target", "__pycache__"]
            .iter()
            .any(|name| entry.file_name() == *name)
        {
            continue;
        }
        limits.1 += 1;
        ensure!(limits.1 <= 20000, "MCP 来源文件过多，请选择子目录");
        let kind = entry.file_type()?;
        ensure!(
            !kind.is_symlink(),
            "MCP 来源不能包含符号链接：{}",
            entry.path().display()
        );
        let out = target.join(entry.file_name());
        if kind.is_dir() {
            copy_source(&entry.path(), &out, depth + 1, limits)?;
        } else {
            ensure!(kind.is_file(), "MCP 来源必须为普通文件");
            let len = entry.metadata()?.len();
            limits.0 += len;
            ensure!(
                len <= 32 * 1024 * 1024 && limits.0 <= 256 * 1024 * 1024,
                "MCP 来源超过大小限制，请选择子目录"
            );
            fs::copy(entry.path(), out)?;
        }
    }
    Ok(())
}
fn read_metadata(path: &Path) -> Result<String> {
    let mut value = String::new();
    fs::File::open(path)?
        .take(1024 * 1024 + 1)
        .read_to_string(&mut value)?;
    ensure!(value.len() <= 1024 * 1024, "MCP 元数据超过 1 MiB");
    Ok(value)
}
fn executable(name: &str) -> String {
    let environment = orbit_runtime::execution::shell_environment::snapshot();
    std::env::split_paths(
        environment
            .get("PATH")
            .map(String::as_str)
            .unwrap_or("/usr/bin:/bin"),
    )
    .map(|directory| directory.join(name))
    .find(|path| path.is_file())
    .map(|path| path.to_string_lossy().into_owned())
    .unwrap_or_else(|| name.to_owned())
}
async fn drain(mut reader: impl tokio::io::AsyncRead + Unpin) -> std::io::Result<Vec<u8>> {
    let mut tail = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        tail.extend_from_slice(&buffer[..count]);
        if tail.len() > 16384 {
            tail.drain(..tail.len() - 16384);
        }
    }
    Ok(tail)
}
struct ProcessGroup(Option<u32>);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(process) = self.0 {
            unsafe {
                libc::kill(-(process as i32), libc::SIGKILL);
            }
        }
    }
}

async fn run(arguments: &[String], directory: &Path) -> Result<()> {
    let (program, args) = arguments.split_first().context("安装命令为空")?;
    let mut command = Command::new(program);
    let environment = orbit_runtime::execution::shell_environment::snapshot();
    command
        .args(args)
        .current_dir(directory)
        .env_clear()
        .envs(environment)
        .env("CI", "1")
        .env("PIP_NO_INPUT", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .spawn()
        .with_context(|| format!("无法启动 {program}，请在目标机器安装对应运行环境"))?;
    let _group = ProcessGroup(child.id());
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let result = tokio::time::timeout(Duration::from_secs(180), async {
        let (status, out, err) = tokio::join!(child.wait(), drain(stdout), drain(stderr));
        Ok::<_, std::io::Error>((status?, out?, err?))
    })
    .await;
    // The group guard also runs if this future is cancelled, so descendants
    // cannot outlive a timeout or a closed installation request.
    let (status, out, err) = result.context("安装超时（180 秒），请检查目标机器的网络和依赖")??;
    ensure!(
        status.success(),
        "依赖安装失败：{}\n{}\n{}",
        arguments.join(" "),
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err)
    );
    Ok(())
}
