use std::{collections::VecDeque, time::Duration};

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio_util::sync::CancellationToken;

use super::process::{CommandResult, MAX_OUTPUT, OutputLog};
use crate::{
    error::{RuntimeError, failed},
    verification::VerificationEvidence,
};

struct Captured {
    bytes: Vec<u8>,
    total: u64,
    retained_truncated: bool,
}

async fn capture(
    mut pipe: impl AsyncRead + Unpin,
    mut retained: Option<super::output::RetainedOutput>,
) -> std::io::Result<Captured> {
    let retaining = retained.is_some();
    let mut tail = VecDeque::with_capacity(MAX_OUTPUT);
    let mut total = 0u64;
    let mut buffer = [0; 4096];

    loop {
        let count = pipe.read(&mut buffer).await?;

        if count == 0 {
            let mut bytes: Vec<u8> = tail.into();

            if total > MAX_OUTPUT as u64 {
                let partial = bytes.iter().take_while(|b| **b & 0xc0 == 0x80).count();
                bytes.drain(..partial);
            }

            if let Some(file) = retained.as_mut() {
                file.flush().await?;
            }

            return Ok(Captured {
                bytes,
                total,
                retained_truncated: retaining
                    && retained.as_ref().is_some_and(|file| file.written() < total),
            });
        }

        total = total.saturating_add(count as u64);

        if let Some(file) = retained.as_mut() {
            file.append(&buffer[..count]).await?;
        }

        let overflow = (tail.len() + count).saturating_sub(MAX_OUTPUT);
        tail.drain(..overflow);
        tail.extend(&buffer[..count]);
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_child_process(
    mut child: tokio::process::Child,
    timeout: Duration,
    token: CancellationToken,
    output_log: Option<OutputLog>,
    sandbox_label: &'static str,
    persistent_scratch: bool,
    workspace_files: usize,
    skipped_entries: usize,
    verification: Option<VerificationEvidence>,
    process_group: Option<u32>,
) -> Result<CommandResult, RuntimeError> {
    let started = std::time::Instant::now();
    let (output_id, stdout_log, stderr_log) = output_log
        .map(OutputLog::split)
        .map_or((None, None, None), |(id, stdout, stderr)| {
            (Some(id), Some(stdout), Some(stderr))
        });

    let stdout = capture(child.stdout.take().expect("piped stdout"), stdout_log);
    let stderr = capture(child.stderr.take().expect("piped stderr"), stderr_log);

    let completion = async {
        let mut timed_out = false;
        let mut cancelled = false;
        let status = tokio::select! {
            result = child.wait() => result,
            _ = token.cancelled() => {
                cancelled = true;
                if let Some(id) = process_group {
                    unsafe { libc::kill(-(id as i32), libc::SIGKILL); }
                }
                child.start_kill()?;
                child.wait().await
            },
            _ = tokio::time::sleep(timeout) => {
                timed_out = true;
                if let Some(id) = process_group {
                    unsafe { libc::kill(-(id as i32), libc::SIGKILL); }
                }
                child.start_kill()?;
                child.wait().await
            },
        }?;

        if let Some(id) = process_group {
            unsafe {
                libc::kill(-(id as i32), libc::SIGKILL);
            }
        }
        Ok::<_, std::io::Error>((status, timed_out, cancelled))
    };

    let (result, stdout, stderr) = tokio::join!(completion, stdout, stderr);
    let (status, timed_out, cancelled) = result.map_err(failed)?;
    let stdout = stdout.map_err(failed)?;
    let stderr = stderr.map_err(failed)?;

    let stdout_total_bytes = stdout.total;
    let stderr_total_bytes = stderr.total;
    let output_truncated = stdout.total > MAX_OUTPUT as u64 || stderr.total > MAX_OUTPUT as u64;
    let retained_output_truncated = stdout.retained_truncated || stderr.retained_truncated;
    let stdout = String::from_utf8_lossy(&stdout.bytes).into_owned();
    let stderr = String::from_utf8_lossy(&stderr.bytes).into_owned();

    Ok(CommandResult {
        exit_code: status.code(),
        elapsed_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
        stdout,
        stderr,
        timed_out,
        cancelled,
        output_truncated,
        stdout_total_bytes,
        stderr_total_bytes,
        output_id,
        retained_output_truncated,
        sandbox: sandbox_label,
        scratch: if persistent_scratch {
            Some("/scratch (persistent session files; no persistent processes)")
        } else {
            None
        },
        workspace_files,
        skipped_entries,
        verification_error: None,
        verification,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn capture_keeps_bounded_unicode_tail_and_counts_all_output() {
        let data = format!("{}FINAL", "中文".repeat(20_000));
        let result = capture(data.as_bytes(), None).await.unwrap();

        assert_eq!(result.total, data.len() as u64);
        assert!(result.bytes.len() <= MAX_OUTPUT);

        let tail = String::from_utf8(result.bytes).unwrap();

        assert!(tail.ends_with("FINAL"));
        assert!(data.ends_with(&tail));

        let short = capture(b"small".as_slice(), None).await.unwrap();

        assert_eq!(short.bytes, b"small");
        assert_eq!(short.total, 5);
    }

    #[tokio::test]
    async fn capture_retains_a_paginated_log_with_a_hard_limit() {
        let directory = tempfile::tempdir().unwrap();
        let log = OutputLog::create(directory.path()).await.unwrap();
        let id = log.id().to_owned();
        let (_, stdout, _) = log.split();
        let result = capture(b"first\nsecond\n".as_slice(), Some(stdout))
            .await
            .unwrap();

        assert!(!result.retained_truncated);
        assert_eq!(result.total, 13);
        assert_eq!(
            std::fs::read_to_string(directory.path().join(format!("{id}.stdout"))).unwrap(),
            "first\nsecond\n"
        );
    }
}
