use std::{process::Stdio, time::Duration};

use anyhow::{Context, Result, ensure};

pub async fn check_sandbox() -> Result<()> {
    super::backend::BwrapBackend::new()?;
    let mut command = tokio::process::Command::new("/usr/bin/bwrap");
    command
        .args([
            "--die-with-parent",
            "--unshare-user",
            "--unshare-pid",
            "--unshare-net",
            "--ro-bind",
            "/",
            "/",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "--",
            "/bin/true",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(5), command.output())
        .await
        .context("sandbox availability check timed out")??;
    ensure!(
        output.status.success(),
        "sandbox unavailable: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    );
    Ok(())
}
