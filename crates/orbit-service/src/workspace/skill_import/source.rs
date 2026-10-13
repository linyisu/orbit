use super::{reject_symlink_ancestors, validate_relative};
use anyhow::{Context, Result, bail, ensure};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

#[derive(Debug, PartialEq)]
struct GitSource {
    url: String,
    branch: Option<String>,
    directory: PathBuf,
}

pub(super) async fn acquire(
    source: &str,
    workspace: &Path,
    home: Option<&Path>,
    storage: &Path,
) -> Result<PathBuf> {
    let local = if let Some(relative) = source.strip_prefix("~/") {
        home.map(|home| home.join(relative))
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(relative)))
            .context("无法确定服务器上的个人目录")?
    } else {
        let path = PathBuf::from(source);
        if path.is_absolute() {
            path
        } else {
            workspace.join(path)
        }
    };
    if std::fs::symlink_metadata(&local).is_ok() {
        reject_symlink_ancestors(&local)?;
        return Ok(local);
    }
    let git = parse(source)?;
    let checkout = storage.join("checkout");
    let mut command = Command::new("git");
    command
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LANG", "C.UTF-8")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env(
            "GIT_SSH_COMMAND",
            "ssh -F /dev/null -o BatchMode=yes -o ConnectTimeout=15 -o StrictHostKeyChecking=yes",
        )
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "protocol.file.allow=never",
            "-c",
            "protocol.ext.allow=never",
            "clone",
            "--quiet",
            "--depth",
            "1",
            "--single-branch",
            "--no-tags",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    command.process_group(0);
    // SSH agent authentication is allowed, while prompts and arbitrary ~/.ssh
    // commands/config are not. Existing known_hosts remains authoritative.
    if let Some(home) = home
        .map(Path::as_os_str)
        .map(|value| value.to_owned())
        .or_else(|| std::env::var_os("HOME"))
    {
        command.env("HOME", home);
    }
    if let Some(socket) = std::env::var_os("SSH_AUTH_SOCK") {
        command.env("SSH_AUTH_SOCK", socket);
    }
    let mut proxy_values = Vec::new();
    for name in [
        "https_proxy",
        "HTTPS_PROXY",
        "http_proxy",
        "HTTP_PROXY",
        "all_proxy",
        "ALL_PROXY",
        "no_proxy",
        "NO_PROXY",
    ] {
        if let Some(value) = std::env::var_os(name) {
            if let Some(value) = value.to_str() {
                proxy_values.push(value.to_owned());
            }
            command.env(name, value);
        }
    }
    if let Some(branch) = git.branch {
        command.args(["--branch", &branch]);
    }
    command.arg("--").arg(&git.url).arg(&checkout);
    let mut child = command
        .spawn()
        .context("无法启动 Git，请确认工作区服务器已安装 Git")?;
    let process = child.id();
    let mut stderr = child
        .stderr
        .take()
        .context("无法读取 Git 输出")?
        .take(64 * 1024 + 1);
    let output = tokio::time::timeout(Duration::from_secs(60), async {
        let read = async {
            let mut errors = Vec::new();
            stderr.read_to_end(&mut errors).await?;
            Ok::<_, std::io::Error>(errors)
        };
        let (status, errors) = tokio::join!(child.wait(), read);
        Ok::<_, std::io::Error>((status?, errors?))
    })
    .await;
    let (status, errors) = match output {
        Ok(Ok(result)) => result,
        failure => {
            #[cfg(unix)]
            if let Some(process) = process {
                unsafe {
                    libc::kill(-(process as i32), libc::SIGKILL);
                }
            }
            let _ = child.start_kill();
            let _ = child.wait().await;
            match failure {
                Err(_) => bail!("获取技能来源超时（60 秒），请检查地址和网络后重试"),
                Ok(Err(error)) => return Err(error).context("无法读取 Git 获取结果"),
                Ok(Ok(_)) => unreachable!(),
            }
        }
    };
    let mut errors = String::from_utf8_lossy(&errors).into_owned();
    for proxy in proxy_values {
        if !proxy.is_empty() {
            errors = errors.replace(&proxy, "[代理地址]");
        }
    }
    ensure!(
        status.success(),
        "获取技能来源失败：{}",
        errors.chars().take(1500).collect::<String>().trim()
    );
    let root = checkout.join(git.directory);
    reject_symlink_ancestors(&root)?;
    ensure!(
        root.exists(),
        "来源中未找到指定子目录，请检查 GitHub 分支与路径"
    );
    Ok(root)
}

fn parse(source: &str) -> Result<GitSource> {
    ensure!(!source.contains(['\n', '\r', '\0']), "技能来源地址无效");
    let mut result = GitSource {
        url: String::new(),
        branch: None,
        directory: PathBuf::new(),
    };
    if let Some((host, path)) = source
        .strip_prefix("git@")
        .and_then(|value| value.split_once(':'))
    {
        ensure!(
            !host.is_empty()
                && !host.starts_with('-')
                && host
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'.' || c == b'-'),
            "SSH 主机名无效"
        );
        ensure!(
            !path.is_empty() && !path.starts_with('-') && !path.contains(char::is_whitespace),
            "SSH 仓库路径无效"
        );
        result.url = source.into();
        return Ok(result);
    }
    let shorthand = source.split('/').collect::<Vec<_>>();
    if shorthand.len() == 2
        && shorthand.iter().all(|part| {
            !part.is_empty()
                && *part != "."
                && *part != ".."
                && part
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'))
        })
    {
        result.url = format!("https://github.com/{}.git", source.trim_end_matches(".git"));
        return Ok(result);
    }
    let parsed = url::Url::parse(source)
        .context("来源应为本地目录、SKILL.md、GitHub owner/repo、HTTPS 或 SSH Git 地址")?;
    ensure!(
        matches!(parsed.scheme(), "https" | "ssh")
            && parsed.host_str().is_some()
            && parsed.host_str().is_some_and(|host| !host.starts_with('-'))
            && parsed.password().is_none()
            && !parsed.username().starts_with('-')
            && (parsed.scheme() == "ssh" || parsed.username().is_empty()),
        "请使用 HTTPS 或 SSH Git 地址，不要在地址中包含密码"
    );
    ensure!(
        parsed.fragment().is_none() && parsed.query().is_none(),
        "请移除技能来源地址的查询参数与锚点"
    );
    result.url = source.into();
    if parsed.host_str() == Some("github.com") {
        let segments = parsed
            .path_segments()
            .context("GitHub 地址无效")?
            .filter(|part| !part.is_empty())
            .map(decode)
            .collect::<Result<Vec<_>>>()?;
        ensure!(segments.len() >= 2, "GitHub 地址需包含 owner/repo");
        if segments.len() > 2 {
            ensure!(
                matches!(segments[2].as_str(), "tree" | "blob") && segments.len() >= 4,
                "请提供 GitHub 仓库或 tree/分支/目录 地址"
            );
            let branch = &segments[3];
            ensure!(
                !branch.is_empty()
                    && !branch.starts_with('-')
                    && !branch.contains(char::is_whitespace),
                "GitHub 分支名称无效"
            );
            result.branch = Some(branch.clone());
            let path = segments[4..].join("/");
            if !path.is_empty() {
                validate_relative(&path)?;
                result.directory = path.into();
            }
            result.url = format!(
                "https://github.com/{}/{}.git",
                segments[0],
                segments[1].trim_end_matches(".git")
            );
        }
    }
    Ok(result)
}

fn decode(segment: &str) -> Result<String> {
    let mut result = Vec::new();
    let mut bytes = segment.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = bytes.next().and_then(|b| (b as char).to_digit(16));
            let low = bytes.next().and_then(|b| (b as char).to_digit(16));
            match (high, low) {
                (Some(high), Some(low)) => result.push((high * 16 + low) as u8),
                _ => bail!("GitHub 地址包含无效编码"),
            }
        } else {
            result.push(byte);
        }
    }
    let value = String::from_utf8(result).context("GitHub 路径不是 UTF-8")?;
    ensure!(
        !value.contains(['\0', '\n', '\r', '\\']),
        "GitHub 路径包含无效字符"
    );
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_github_git_ssh_and_tree_paths_without_shell_interpolation() {
        assert_eq!(
            parse("team/skills").unwrap().url,
            "https://github.com/team/skills.git"
        );
        let tree = parse("https://github.com/team/skills/tree/main/skills/review").unwrap();
        assert_eq!(tree.branch.as_deref(), Some("main"));
        assert_eq!(tree.directory, Path::new("skills/review"));
        assert_eq!(tree.url, "https://github.com/team/skills.git");
        assert_eq!(
            parse("https://github.com/team/skills/tree/feature%2Fnew/skills")
                .unwrap()
                .branch
                .as_deref(),
            Some("feature/new")
        );
        assert_eq!(
            parse("https://github.com/team/skills/blob/main/skills/review/SKILL.md")
                .unwrap()
                .directory,
            Path::new("skills/review/SKILL.md")
        );
        assert!(parse("git@example.com:team/skills.git").is_ok());
        assert!(parse("ssh://git@example.com/team/skills.git").is_ok());
        for invalid in [
            "--upload-pack=bad",
            "file:///tmp/source",
            "http://example.com/source",
            "https://user:secret@example.com/source",
            "https://github.com/a/b/tree/main/%2e%2e%2fsecret",
            "git@-bad:repo",
            "https://github.com/a/b/tree/main/%00",
        ] {
            assert!(parse(invalid).is_err(), "accepted {invalid}");
        }
    }
}
