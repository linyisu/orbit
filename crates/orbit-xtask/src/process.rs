use std::{process::Output, time::Duration};

use anyhow::{Context, Result, ensure};
use tokio::process::Command;

pub async fn run(command: &mut Command) -> Result<Output> {
    let description = format!("{command:?}");
    let output = tokio::time::timeout(
        Duration::from_secs(120),
        command.kill_on_drop(true).output(),
    )
    .await
    .with_context(|| format!("command timed out: {description}"))?
    .with_context(|| format!("cannot start: {description}"))?;
    ensure!(
        output.status.success(),
        "{description} failed ({}):\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(output)
}

pub fn stdout(output: Output) -> Result<String> {
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}
