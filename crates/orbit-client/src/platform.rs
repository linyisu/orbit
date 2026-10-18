pub mod editor;

#[cfg(unix)]
use std::process::{Command, Stdio};
#[cfg(unix)]
use std::time::Duration;

use anyhow::Result;

pub use orbit_auth::directory;

pub async fn launch_browser(url: &str) {
    #[cfg(target_os = "linux")]
    let command = "xdg-open";
    #[cfg(target_os = "macos")]
    let command = "open";
    #[cfg(windows)]
    {
        let url = url.to_owned();
        let _ = tokio::task::spawn_blocking(move || open_link_in_browser(&url)).await;
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
    #[cfg(windows)]
    return open_native_browser(url.as_str());
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    anyhow::bail!("当前系统不支持打开默认浏览器");
    // `xdg-open` may hand off to an already-running Chromium process. Keep
    // every descriptor detached from the alternate screen; otherwise browser
    // diagnostics can be printed through the active TUI and corrupt its frame.
    #[cfg(unix)]
    {
        command
            .arg(url.as_str())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
    }

    #[cfg(unix)]
    Ok(())
}

#[cfg(windows)]
fn open_native_browser(url: &str) -> Result<()> {
    use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};
    let url: Vec<u16> = url
        .encode_utf16()
        .chain(Some(0))
        .collect();
    // SAFETY: the validated URL is NUL-terminated and lives through the synchronous call;
    // null optional pointers request the system browser without a shell command string.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            std::ptr::null(),
            url.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    } as isize;
    anyhow::ensure!(result > 32, "无法启动默认浏览器（Windows 错误 {result}）");
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
