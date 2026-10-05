use std::{
    io::Read,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};

use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::transport::Target;

const BOOTSTRAP_TIMEOUT: Duration = Duration::from_secs(180);

#[cfg(not(target_env = "musl"))]
const EMBEDDED_SERVER: &[u8] = include_bytes!(env!("ORBIT_EMBEDDED_SERVER"));
#[cfg(target_env = "musl")]
const EMBEDDED_SERVER: &[u8] = &[];

#[derive(Clone, Debug)]
enum ServerBinary {
    File(PathBuf),
    Embedded(&'static [u8]),
}

impl ServerBinary {
    fn identity(&self) -> Result<(u16, String)> {
        match self {
            Self::File(path) => binary_identity(std::fs::File::open(path)?),
            Self::Embedded(bytes) => binary_identity(*bytes),
        }
    }

    async fn reader(&self) -> Result<(Box<dyn tokio::io::AsyncRead + Send + Unpin>, u64)> {
        match self {
            Self::File(path) => {
                let file = tokio::fs::File::open(path).await?;
                let size = file.metadata().await?.len();
                Ok((Box::new(file), size))
            }
            Self::Embedded(bytes) => {
                Ok((Box::new(std::io::Cursor::new(*bytes)), bytes.len() as u64))
            }
        }
    }
}

pub async fn executable(
    target: &Target,
    upload: Option<PathBuf>,
    unattended: bool,
    interactive: Option<&super::interaction::Interaction>,
    progress: Option<&super::interaction::Progress>,
) -> Result<String> {
    if matches!(target, Target::Local) {
        ensure!(
            upload.is_none(),
            "--upload-server is only used with SSH or WSL"
        );
        return Ok(std::env::current_exe()?.to_string_lossy().into_owned());
    }
    super::interaction::timeout(
        BOOTSTRAP_TIMEOUT,
        progress,
        prepare(target, upload, unattended, interactive, progress),
    )
    .await
    .context("workspace server deployment failed")
}

async fn prepare(
    target: &Target,
    upload: Option<PathBuf>,
    unattended: bool,
    interactive: Option<&super::interaction::Interaction>,
    progress: Option<&super::interaction::Progress>,
) -> Result<String> {
    if let Some(progress) = progress {
        progress.report("连接目标并检测系统与架构");
    }
    let platform = shell(
        target,
        unattended,
        interactive,
        progress,
        "printf '%s\\n' \"$(uname -s)\" \"$(uname -m)\" \"$HOME\"".into(),
    )
    .await?;
    let lines: Vec<_> = platform.lines().collect();
    ensure!(lines.len() >= 3, "could not detect target platform");
    let [system, architecture, home] = lines[lines.len() - 3..] else {
        unreachable!()
    };
    ensure!(
        system == "Linux",
        "workspace execution currently requires Linux (including WSL)"
    );
    ensure!(
        home.starts_with('/') && !home.contains(['\0', '\r', '\n']),
        "target HOME is invalid"
    );
    if let Some(progress) = progress {
        progress.report("选择与目标架构匹配的服务程序");
    }
    let binary = select_binary(upload, &std::env::current_exe()?, architecture)?;
    if let Some(progress) = progress {
        progress.report(match &binary {
            ServerBinary::Embedded(_) => "准备内置的静态工作区服务".into(),
            ServerBinary::File(path) => format!("准备服务程序：{}", path.display()),
        });
    }
    let (machine, digest) = tokio::task::spawn_blocking({
        let binary = binary.clone();
        move || binary.identity()
    })
    .await??;
    let expected = match architecture {
        "x86_64" => 62,
        "aarch64" => 183,
        _ => bail!(
            "unsupported target architecture: {architecture}; use --server with an installed Orbit"
        ),
    };
    ensure!(
        machine == expected,
        "target architecture differs from this binary; provide a matching Linux binary with --upload-server or an installed target executable with --server"
    );
    let directory = format!("{home}/.cache/orbit/servers/{digest}");
    let executable = format!("{directory}/orbit");
    if let Some(progress) = progress {
        progress.report("检查目标上的服务版本");
    }
    let probe = shell(
        target,
        unattended,
        interactive,
        progress,
        format!("if test -x {}; then printf ready; fi", quote(&executable)),
    )
    .await?;
    if probe.lines().last() == Some("ready") {
        if let Some(progress) = progress {
            progress.report("复用已安装的工作区服务");
        }
        verify(target, &executable, unattended, interactive, progress).await?;
        return Ok(executable);
    }
    if !unattended {
        eprintln!("正在为 {} 安装工作区服务…", target.label());
    }
    if let Some(progress) = progress {
        progress.report("上传并校验工作区服务程序");
    }
    let script = upload_script(&directory, &digest);
    let mut command =
        target.command(&["sh".into(), "-c".into(), script], unattended, interactive)?;
    command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = command.spawn()?;
    let mut input = child.stdin.take().context("missing server upload pipe")?;
    let (mut file, size) = binary.reader().await?;
    let upload = async {
        let mut buffer = [0; 64 * 1024];
        let mut sent = 0;
        let mut last_percent = 0;
        loop {
            let count = file.read(&mut buffer).await?;
            if count == 0 {
                break;
            }
            input.write_all(&buffer[..count]).await?;
            sent += count as u64;
            let percent = sent.saturating_mul(100) / size.max(1);
            if percent >= last_percent + 10 {
                if let Some(progress) = progress {
                    progress.report(format!("上传工作区服务：{percent}%"));
                }
                last_percent = percent;
            }
        }
        input.shutdown().await?;
        drop(input);
        Ok::<_, std::io::Error>(())
    };
    let (sent, output) = tokio::join!(upload, super::command::output(child, progress));
    let output = output?;
    ensure!(
        output.status.success(),
        "workspace server upload failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    sent?;
    verify(target, &executable, unattended, interactive, progress).await?;
    Ok(executable)
}

fn select_binary(
    upload: Option<PathBuf>,
    current: &Path,
    architecture: &str,
) -> Result<ServerBinary> {
    if let Some(upload) = upload {
        return Ok(ServerBinary::File(upload));
    }
    ensure!(
        matches!(architecture, "x86_64" | "aarch64"),
        "unsupported target architecture: {architecture}"
    );
    ensure!(
        architecture == std::env::consts::ARCH,
        "客户端未内置 {architecture} 架构的服务程序；请通过 --upload-server 指定匹配架构的 Linux 程序"
    );
    if !EMBEDDED_SERVER.is_empty() {
        return Ok(ServerBinary::Embedded(EMBEDDED_SERVER));
    }
    ensure!(
        cfg!(all(target_env = "musl", target_feature = "crt-static")),
        "此构建缺少内置的静态工作区服务，无法自动部署"
    );
    Ok(ServerBinary::File(current.to_owned()))
}

pub(super) async fn verify(
    target: &Target,
    executable: &str,
    unattended: bool,
    interactive: Option<&super::interaction::Interaction>,
    progress: Option<&super::interaction::Progress>,
) -> Result<()> {
    if let Some(progress) = progress {
        progress.report("检查服务程序能否在目标系统运行");
    }
    let child = target
        .command(
            &[
                "sh".into(),
                "-c".into(),
                format!("{} workspace-service --help", quote(executable)),
            ],
            unattended,
            interactive,
        )?
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let output = super::interaction::timeout(
        Duration::from_secs(30),
        progress,
        super::command::output(child, progress),
    )
    .await?;
    ensure!(
        output.status.success(),
        "{}",
        runtime_error(&String::from_utf8_lossy(&output.stderr))
    );
    Ok(())
}

pub(super) fn runtime_error(diagnostics: &str) -> String {
    if diagnostics.contains("GLIBC_") && diagnostics.contains("not found") {
        "指定的服务程序依赖目标系统未提供的 glibc 版本。请移除 --server / --upload-server，使用客户端内置的静态服务，或指定兼容目标系统的程序。".into()
    } else if diagnostics.contains("error while loading shared libraries") {
        "指定的服务程序依赖目标系统未提供的动态库。请移除 --server / --upload-server，使用客户端内置的静态服务，或指定兼容目标系统的程序。".into()
    } else {
        format!("目标服务程序无法启动：{}", diagnostics.trim())
    }
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn upload_script(directory: &str, digest: &str) -> String {
    format!(
        "set -eu; umask 077; directory={}; mkdir -p -- \"$directory\"; temporary=$(mktemp \"$directory/.orbit.XXXXXX\"); trap 'rm -f -- \"$temporary\"' EXIT HUP INT TERM; cat > \"$temporary\"; printf '%s  %s\\n' {} \"$temporary\" | sha256sum -c - >/dev/null; chmod 700 \"$temporary\"; mv -f -- \"$temporary\" \"$directory/orbit\"",
        quote(directory),
        quote(digest)
    )
}

async fn shell(
    target: &Target,
    unattended: bool,
    interactive: Option<&super::interaction::Interaction>,
    progress: Option<&super::interaction::Progress>,
    script: String,
) -> Result<String> {
    let child = target
        .command(&["sh".into(), "-c".into(), script], unattended, interactive)?
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let output = super::interaction::timeout(
        Duration::from_secs(30),
        progress,
        super::command::output(child, progress),
    )
    .await?;
    ensure!(
        output.status.success(),
        "target environment check failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).context("target environment response is not UTF-8")
}

fn binary_identity(mut file: impl Read) -> Result<(u16, String)> {
    let mut header = [0; 20];
    file.read_exact(&mut header)?;
    ensure!(
        &header[..4] == b"\x7fELF" && header[5] == 1,
        "--upload-server must be a little-endian Linux ELF binary"
    );
    let machine = u16::from_le_bytes([header[18], header[19]]);
    let mut digest = Sha256::new();
    digest.update(header);
    let mut bytes = [0; 65536];
    loop {
        let count = file.read(&mut bytes)?;
        if count == 0 {
            break;
        }
        digest.update(&bytes[..count]);
    }
    Ok((
        machine,
        digest
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deployment_uses_the_embedded_server_without_companion_files() {
        let root = tempfile::tempdir().unwrap();
        let current = root.path().join("orbit");
        std::fs::write(&current, b"not a portable executable").unwrap();
        let selected = select_binary(None, &current, std::env::consts::ARCH).unwrap();
        if cfg!(target_env = "musl") {
            assert!(matches!(selected, ServerBinary::File(path) if path == current));
        } else {
            assert!(matches!(selected, ServerBinary::Embedded(_)));
            assert!(selected.identity().is_ok());
        }
        let other = if std::env::consts::ARCH == "x86_64" {
            "aarch64"
        } else {
            "x86_64"
        };
        assert!(select_binary(None, &current, other).is_err());
        assert!(matches!(
            select_binary(Some("/explicit".into()), &current, other).unwrap(),
            ServerBinary::File(path) if path == Path::new("/explicit")
        ));
    }

    #[tokio::test]
    async fn incompatible_binary_fails_preflight_before_the_protocol_handshake() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let executable = root.path().join("old linux 'server'");
        std::fs::write(
            &executable,
            "#!/bin/sh\nprintf '%s\\n' \"libm.so.6: version 'GLIBC_2.43' not found\" >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let (progress, updates) = super::super::interaction::channel();
        let error = verify(
            &Target::Local,
            executable.to_str().unwrap(),
            true,
            None,
            Some(&progress),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("glibc"));
        assert!(!error.to_string().contains("unexpected end of file"));
        assert!(
            updates
                .logs
                .lock()
                .unwrap()
                .iter()
                .any(|line| line.contains("GLIBC_2.43"))
        );
    }

    #[tokio::test]
    async fn deployment_checks_checksum_and_atomically_installs_in_quoted_paths() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("path with 'quotes' $(touch bad)");
        let bytes = b"sample binary";
        let digest: String = Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        for (hash, success) in [(&digest, true), (&"0".repeat(64), false)] {
            let mut child = tokio::process::Command::new("sh")
                .arg("-c")
                .arg(upload_script(directory.to_str().unwrap(), hash))
                .stdin(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(bytes).await.unwrap();
            assert_eq!(child.wait().await.unwrap().success(), success);
            assert_eq!(std::fs::read(directory.join("orbit")).unwrap(), bytes);
            assert_eq!(std::fs::read_dir(&directory).unwrap().count(), 1);
        }
    }
}
