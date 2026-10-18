#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::{collections::VecDeque, time::Duration};

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio_util::sync::CancellationToken;

use super::process::{CommandResult, MAX_OUTPUT, OutputLog, ProcessGroup};
use crate::{
    error::{RuntimeError, failed},
    verification::VerificationEvidence,
};

struct Captured {
    bytes: Vec<u8>,
    total: u64,
    retained_truncated: bool,
    retained_error: Option<String>,
    interrupted: bool,
}

async fn capture(
    mut pipe: impl AsyncRead + Unpin,
    mut retained: Option<super::output::RetainedOutput>,
    finished: CancellationToken,
) -> std::io::Result<Captured> {
    let retaining = retained.is_some();
    let mut tail = VecDeque::with_capacity(MAX_OUTPUT);
    let mut total = 0u64;
    let mut buffer = [0; 4_096];

    let deadline = async {
        finished.cancelled().await;
        tokio::time::sleep(Duration::from_secs(1)).await;
    };
    tokio::pin!(deadline);
    let mut interrupted = false;
    let mut retained_error = None;
    loop {
        let count = tokio::select! {
            count = pipe.read(&mut buffer) => count?,
            _ = &mut deadline => {
                interrupted = true;
                0
            }
        };

        if count == 0 {
            let mut bytes: Vec<u8> = tail.into();

            if total > MAX_OUTPUT as u64 {
                let partial = bytes
                    .iter()
                    .take_while(|b| **b & 0xc0 == 0x80)
                    .count();
                bytes.drain(..partial);
            }

            if let Some(file) = retained.as_mut()
                && let Err(error) = file.flush().await
            {
                retained_error = Some(error.to_string());
            }

            return Ok(Captured {
                bytes,
                total,
                interrupted,
                retained_error: retained_error.clone(),
                retained_truncated: retaining
                    && (interrupted
                        || retained_error.is_some()
                        || retained
                            .as_ref()
                            .is_some_and(|file| file.written() < total)),
            });
        }

        total = total.saturating_add(count as u64);

        if let Some(file) = retained.as_mut()
            && let Err(error) = file.append(&buffer[..count]).await
        {
            // Retention is optional. Keep draining the pipe and capturing the
            // bounded tail even if the temporary disk becomes unavailable.
            retained_error = Some(error.to_string());
            retained = None;
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
    workspace_files: usize,
    skipped_entries: usize,
    verification: Option<VerificationEvidence>,
    mut process_group: Option<ProcessGroup>,
) -> Result<CommandResult, RuntimeError> {
    let started = std::time::Instant::now();
    let (output_id, stdout_log, stderr_log) = output_log
        .map(OutputLog::split)
        .map_or((None, None, None), |(id, stdout, stderr)| {
            (Some(id), Some(stdout), Some(stderr))
        });

    let finished = CancellationToken::new();
    let stdout = capture(
        child
            .stdout
            .take()
            .expect("piped stdout"),
        stdout_log,
        finished.clone(),
    );
    let stderr = capture(
        child
            .stderr
            .take()
            .expect("piped stderr"),
        stderr_log,
        finished.clone(),
    );

    let completion = async {
        let _finished = finished.drop_guard();
        let mut timed_out = false;
        let mut cancelled = false;
        let status = tokio::select! {
            result = child.wait() => result,
            _ = token.cancelled() => {
                cancelled = true;
                if let Some(group) = process_group.as_mut() {
                    group.kill();
                }
                child.start_kill()?;
                child.wait().await
            }
            _ = tokio::time::sleep(timeout) => {
                timed_out = true;
                if let Some(group) = process_group.as_mut() {
                    group.kill();
                }
                child.start_kill()?;
                child.wait().await
            }
        }?;

        if let Some(group) = process_group.as_mut() {
            group.kill();
        }
        Ok::<_, std::io::Error>((status, timed_out, cancelled))
    };

    let (result, stdout, stderr) = tokio::join!(completion, stdout, stderr);
    let (status, timed_out, cancelled) = result.map_err(failed)?;
    let stdout = stdout.map_err(failed)?;
    let stderr = stderr.map_err(failed)?;

    let stdout_total_bytes = stdout.total;
    let stderr_total_bytes = stderr.total;
    let output_truncated = stdout.interrupted
        || stderr.interrupted
        || stdout.total > MAX_OUTPUT as u64
        || stderr.total > MAX_OUTPUT as u64;
    let retained_output_truncated = stdout.retained_truncated || stderr.retained_truncated;
    let retained_output_error = match (stdout.retained_error, stderr.retained_error) {
        (None, None) => None,
        (Some(error), None) => Some(format!("stdout: {error}")),
        (None, Some(error)) => Some(format!("stderr: {error}")),
        (Some(stdout), Some(stderr)) => Some(format!("stdout: {stdout}; stderr: {stderr}")),
    };
    let stdout = String::from_utf8_lossy(&stdout.bytes).into_owned();
    let stderr = String::from_utf8_lossy(&stderr.bytes).into_owned();

    Ok(CommandResult {
        exit_code: status.code(),
        #[cfg(unix)]
        signal: status.signal(),
        #[cfg(windows)]
        signal: None,
        #[cfg(unix)]
        core_dumped: status.core_dumped(),
        #[cfg(windows)]
        core_dumped: false,
        elapsed_ms: started
            .elapsed()
            .as_millis()
            .min(u64::MAX as u128) as u64,
        stdout,
        stderr,
        timed_out,
        cancelled,
        output_truncated,
        stdout_total_bytes,
        stderr_total_bytes,
        output_id,
        retained_output_truncated,
        retained_output_error,
        sandbox: sandbox_label,
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
        let result = capture(data.as_bytes(), None, CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(result.total, data.len() as u64);
        assert!(result.bytes.len() <= MAX_OUTPUT);

        let tail = String::from_utf8(result.bytes).unwrap();

        assert!(tail.ends_with("FINAL"));
        assert!(data.ends_with(&tail));

        let short = capture(b"small".as_slice(), None, CancellationToken::new())
            .await
            .unwrap();

        assert_eq!(short.bytes, b"small");
        assert_eq!(short.total, 5);
    }

    #[tokio::test]
    async fn capture_preserves_complete_short_output_in_the_retained_log() {
        let directory = tempfile::tempdir().unwrap();
        let log = OutputLog::create(directory.path())
            .await
            .unwrap();
        let id = log.id().to_owned();
        let (_, stdout, _) = log.split();
        let result = capture(
            b"first\nsecond\n".as_slice(),
            Some(stdout),
            CancellationToken::new(),
        )
        .await
        .unwrap();

        assert!(!result.retained_truncated);
        assert_eq!(result.total, 13);
        assert_eq!(
            std::fs::read_to_string(
                directory
                    .path()
                    .join(format!("{id}.stdout"))
            )
            .unwrap(),
            "first\nsecond\n"
        );
    }
}

#[cfg(test)]
mod drain_tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[cfg(unix)]
    #[tokio::test]
    async fn signal_termination_retains_output_without_claiming_timeout_or_cancellation() {
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .args(["-c", "printf before; kill -TERM $$"])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let (child, group) = ProcessGroup::spawn(&mut command).unwrap();
        let result = run_child_process(
            child,
            Duration::from_secs(5),
            CancellationToken::new(),
            None,
            "native",
            0,
            0,
            None,
            Some(group),
        )
        .await
        .unwrap();
        assert_eq!(result.exit_code, None);
        assert_eq!(result.signal, Some(libc::SIGTERM));
        assert_eq!(result.stdout, "before");
        assert!(!result.timed_out && !result.cancelled && !result.core_dumped);
    }

    #[tokio::test]
    async fn completed_process_does_not_wait_for_a_detached_pipe_owner() {
        let (mut writer, reader) = tokio::io::duplex(64);
        writer
            .write_all(b"last output")
            .await
            .unwrap();
        let finished = CancellationToken::new();
        finished.cancel();
        let captured =
            tokio::time::timeout(Duration::from_secs(2), capture(reader, None, finished))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(captured.bytes, b"last output");
        assert!(captured.interrupted);
        drop(writer);
    }
}
