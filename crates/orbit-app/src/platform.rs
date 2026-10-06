pub mod editor;
pub mod workers;

use std::{
    path::PathBuf,
    process::{Command, Stdio},
    time::Duration,
};

use anyhow::{Context, Result, bail};

pub fn directory(
    explicit: Option<PathBuf>,
    config_home: Option<PathBuf>,
    home: Option<PathBuf>,
) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(std::path::absolute(path)?);
    }
    let config = config_home
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(|| home.map(|home| home.join(".config")))
        .context("无法确定凭据目录，请设置 --auth-dir")?;
    if !config.is_absolute() {
        bail!("XDG_CONFIG_HOME 必须是绝对路径，或通过 --auth-dir 指定凭据目录");
    }

    Ok(config.join("orbit/auth"))
}

pub async fn launch_browser(url: &str) {
    #[cfg(target_os = "linux")]
    let command = "xdg-open";
    #[cfg(target_os = "macos")]
    let command = "open";
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = url;
        return;
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let _ = tokio::time::timeout(
            Duration::from_secs(3),
            tokio::process::Command::new(command)
                .arg(url)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .kill_on_drop(true)
                .status(),
        )
        .await;
    }
}

pub fn browser_url(value: &str) -> Result<url::Url> {
    let url = url::Url::parse(value)?;

    if !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        anyhow::bail!("仅能在浏览器中打开不含凭据的 HTTP(S) 链接");
    }

    Ok(url)
}

pub fn open_link_in_browser(value: &str) -> Result<()> {
    let url = browser_url(value)?;
    #[cfg(target_os = "linux")]
    let mut command = Command::new("xdg-open");
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", ""]);
        command
    };
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    anyhow::bail!("当前系统不支持打开默认浏览器");
    // `xdg-open` may hand off to an already-running Chromium process. Keep
    // every descriptor detached from the alternate screen; otherwise browser
    // diagnostics can be printed through the active TUI and corrupt its frame.
    command
        .arg(url.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injected_home_controls_the_store_without_reading_host_environment() {
        let root = tempfile::tempdir().unwrap();
        let actual = directory(
            None,
            Some(root.path().join("config")),
            Some(root.path().join("home")),
        )
        .unwrap();

        assert_eq!(actual, root.path().join("config/orbit/auth"));
        assert!(directory(None, None, None).is_err());
        assert!(directory(None, Some("relative".into()), None).is_err());
    }
}
