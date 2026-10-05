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
        progress.report("准备服务程序，计算版本校验值");
    }
    let binary = upload.unwrap_or(std::env::current_exe()?);
    let (machine, digest) = tokio::task::spawn_blocking({
        let binary = binary.clone();
        move || binary_identity(&binary)
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
    let mut file = tokio::fs::File::open(binary).await?;
    let size = file.metadata().await?.len();
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
    Ok(executable)
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn upload_script(directory: &str, digest: &str) -> String {
    format!(
        "set -eu; umask 077; directory={}; mkdir -p -- \"$directory\"; temporary=$(mktemp \"$directory/.orbit.XXXXXX\"); trap 'rm -f -- \"$temporary\"' EXIT HUP INT TERM; cat > \"$temporary\"; printf '%s  %s\\n' {} \"$temporary\" | sha256sum -c - >&2; chmod 700 \"$temporary\"; mv -f -- \"$temporary\" \"$directory/orbit\"",
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

fn binary_identity(path: &Path) -> Result<(u16, String)> {
    let mut file = std::fs::File::open(path)?;
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
