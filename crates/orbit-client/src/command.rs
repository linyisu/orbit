use anyhow::{Context, Result, ensure};
use tokio::{io::AsyncReadExt, process::Child};

use super::interaction::Progress;

const OUTPUT_LIMIT: usize = 1024 * 1024;
const DIAGNOSTIC_LIMIT: usize = 16 * 1024;

pub async fn output(mut child: Child, progress: Option<&Progress>) -> Result<std::process::Output> {
    let stdout = child.stdout.take();
    let mut stderr = child.stderr.take().context("missing command diagnostics")?;
    let read_output = async {
        let mut bytes = Vec::new();
        if let Some(stdout) = stdout {
            stdout
                .take((OUTPUT_LIMIT + 1) as u64)
                .read_to_end(&mut bytes)
                .await?;
        }
        ensure!(
            bytes.len() <= OUTPUT_LIMIT,
            "目标返回内容过多，请选择更具体的目录"
        );
        Ok::<_, anyhow::Error>(bytes)
    };
    let read_errors = async {
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let count = stderr.read(&mut buffer).await?;
            if count == 0 {
                break;
            }
            if let Some(progress) = progress {
                progress.log(&String::from_utf8_lossy(&buffer[..count]));
            }
            bytes.extend_from_slice(&buffer[..count]);
            let excess = bytes.len().saturating_sub(DIAGNOSTIC_LIMIT);
            bytes.drain(..excess);
        }
        Ok::<_, anyhow::Error>(bytes)
    };
    let (stdout, stderr, status) = tokio::try_join!(read_output, read_errors, async {
        Ok::<_, anyhow::Error>(child.wait().await?)
    })?;
    Ok(std::process::Output {
        status,
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Stdio;

    #[tokio::test]
    async fn diagnostics_are_streamed_without_blocking_stdout_or_growing_indefinitely() {
        let (progress, updates) = super::super::interaction::channel();
        let child = tokio::process::Command::new("sh")
            .args(["-c", "i=0; while [ $i -lt 4000 ]; do printf '%080d\\n' $i >&2; i=$((i+1)); done; printf done"])
            .stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true).spawn().unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            output(child, Some(&progress)),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(result.status.success());
        assert_eq!(result.stdout, b"done");
        assert!(result.stderr.len() <= DIAGNOSTIC_LIMIT);
        assert!(!updates.logs.lock().unwrap().is_empty());
        assert!(updates.logs.lock().unwrap().len() <= 200);
    }
}
