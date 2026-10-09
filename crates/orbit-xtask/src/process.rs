use std::{
    path::Path,
    process::{Output, Stdio},
    time::Duration,
};

use anyhow::{Context, Result, ensure};
use tokio::process::Command;

pub async fn run(command: &mut Command) -> Result<Output> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    wait(command).await
}

/// Stream binary stdout to disk without Command::output replacing its destination.
pub async fn run_to_file(command: &mut Command, path: &Path) -> Result<()> {
    command
        .stdout(std::fs::File::create(path)?)
        .stderr(Stdio::piped());
    wait(command).await?;
    Ok(())
}

async fn wait(command: &mut Command) -> Result<Output> {
    let description = format!("{command:?}");
    let output = tokio::time::timeout(Duration::from_secs(120), async {
        command.kill_on_drop(true).spawn()?.wait_with_output().await
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn binary_redirection_preserves_all_bytes_and_reports_command_failure() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source");
        let destination = directory.path().join("download");
        let bytes: Vec<_> = (0..=255).cycle().take(128 * 1024 + 17).collect();
        std::fs::write(&source, &bytes).unwrap();

        let captured = run(Command::new("cat").arg(&source)).await.unwrap();
        assert_eq!(captured.stdout, bytes);
        run_to_file(Command::new("cat").arg(&source), &destination)
            .await
            .unwrap();
        assert_eq!(std::fs::read(&destination).unwrap(), bytes);

        let missing = directory.path().join("missing-source");
        let error = run_to_file(Command::new("cat").arg(&missing), &destination)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("failed") && error.contains("missing-source"));
    }
}
