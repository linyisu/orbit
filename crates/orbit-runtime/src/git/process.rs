//! Bounded Git inspection processes with native descendant ownership.

use std::{
    io,
    process::{Command, ExitStatus, Stdio},
    time::Instant,
};

use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

use crate::execution::process::ProcessGroup;

pub enum OutputLimit {
    Complete(usize),
    Truncate(usize),
}

pub enum InspectionOutput {
    Complete { stdout: Vec<u8>, status: ExitStatus },
    Truncated(Vec<u8>),
}

/// Pin a Unix inspection child's working directory to an already authorized directory.
/// The retained descriptor is used only in the child; the caller's directory is never changed.
#[cfg(unix)]
pub fn set_directory(command: &mut Command, directory: &std::fs::File) -> io::Result<()> {
    use std::os::{fd::AsRawFd, unix::process::CommandExt};

    let directory = directory.try_clone()?;
    // SAFETY: fchdir is async-signal-safe and uses a retained descriptor without allocation.
    unsafe {
        command.pre_exec(move || {
            if libc::fchdir(directory.as_raw_fd()) == 0 {
                Ok(())
            } else {
                Err(io::Error::last_os_error())
            }
        });
    }
    Ok(())
}

/// Set a Windows inspection directory using a retained, non-delete-shared directory handle.
/// The caller must retain that authorized handle until the inspection finishes.
#[cfg(windows)]
pub fn set_directory(command: &mut Command, directory: &std::fs::File) -> io::Result<()> {
    use std::os::windows::{ffi::OsStringExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::GetFinalPathNameByHandleW;

    // SAFETY: the directory handle is live; a null buffer queries the required UTF-16 capacity.
    let capacity =
        unsafe { GetFinalPathNameByHandleW(directory.as_raw_handle(), std::ptr::null_mut(), 0, 0) };
    if capacity == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut path = vec![0u16; capacity as usize];
    // SAFETY: path has capacity writable UTF-16 units and the directory remains pinned.
    let length = unsafe {
        GetFinalPathNameByHandleW(directory.as_raw_handle(), path.as_mut_ptr(), capacity, 0)
    };
    if length == 0 {
        return Err(io::Error::last_os_error());
    }
    if length >= capacity {
        return Err(io::Error::other(
            "inspection directory changed during lookup",
        ));
    }
    command.current_dir(std::ffi::OsString::from_wide(&path[..length as usize]));
    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum InspectionError {
    #[error("Git inspection cancelled")]
    Cancelled,
    #[error("Git inspection timed out")]
    TimedOut,
    #[error("Git output exceeds the {0} byte inspection limit")]
    OutputLimit(usize),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Run an already configured read-only Git command under a native process owner.
/// Captures only stdout; stdin and stderr are closed. Every return path terminates descendants
/// and reaps the leader. The absolute deadline includes spawning and output collection.
/// A private I/O runtime permits synchronous callers, including callers inside another runtime,
/// without blocking on an uninterruptible pipe read or nesting Tokio runtimes.
pub fn inspect(
    command: Command,
    cancellation: CancellationToken,
    deadline: Instant,
    limit: OutputLimit,
) -> Result<InspectionOutput, InspectionError> {
    std::thread::Builder::new()
        .name("orbit-git-inspection".into())
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(run(command, cancellation, deadline, limit))
        })?
        .join()
        .map_err(|_| io::Error::other("Git inspection worker panicked"))?
}

async fn run(
    command: Command,
    cancellation: CancellationToken,
    deadline: Instant,
    limit: OutputLimit,
) -> Result<InspectionOutput, InspectionError> {
    if cancellation.is_cancelled() {
        return Err(InspectionError::Cancelled);
    }
    if Instant::now() >= deadline {
        return Err(InspectionError::TimedOut);
    }
    let mut command = tokio::process::Command::from(command);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let (mut child, mut owner) = ProcessGroup::spawn(&mut command)?;
    let mut stdout = child
        .stdout
        .take()
        .expect("inspection stdout is piped");
    let maximum = match limit {
        OutputLimit::Complete(bytes) | OutputLimit::Truncate(bytes) => bytes,
    };
    let capture = async {
        let mut output = Vec::new();
        let mut buffer = [0; 8_192];
        loop {
            let count = stdout.read(&mut buffer).await?;
            if count == 0 {
                break;
            }
            let remaining = maximum
                .saturating_add(1)
                .saturating_sub(output.len());
            output.extend_from_slice(&buffer[..count.min(remaining)]);
            if output.len() > maximum {
                return match limit {
                    OutputLimit::Complete(_) => Err(InspectionError::OutputLimit(maximum)),
                    OutputLimit::Truncate(_) => {
                        output.truncate(maximum);
                        Ok(InspectionOutput::Truncated(output))
                    }
                };
            }
        }
        let status = child.wait().await?;
        Ok(InspectionOutput::Complete {
            stdout: output,
            status,
        })
    };
    let result = tokio::select! {
        biased;
        _ = cancellation.cancelled() => Err(InspectionError::Cancelled),
        _ = tokio::time::sleep_until(deadline.into()) => Err(InspectionError::TimedOut),
        result = capture => result,
    };
    owner.kill();
    let _ = child.wait().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{io::Write, time::Duration};

    const FIXTURE: &str = "git::process::tests::capture_fixture";
    const ROLE: &str = "ORBIT_GIT_CAPTURE_FIXTURE";

    #[test]
    #[ignore]
    fn capture_fixture() {
        let Ok(role) = std::env::var(ROLE) else {
            return;
        };
        if role == "output" {
            std::io::stdout()
                .write_all(&vec![b'x'; 65_536])
                .unwrap();
            std::io::stdout().flush().unwrap();
        }
        if role == "complete" {
            println!("inspection complete");
            return;
        }
        if role == "directory" {
            println!("{}", std::fs::read_to_string("marker").unwrap());
            return;
        }
        std::thread::sleep(Duration::from_secs(30));
    }

    fn command(role: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--ignored", "--exact", FIXTURE, "--nocapture"])
            .env(ROLE, role);
        command
    }

    #[test]
    fn inspection_uses_the_retained_directory_without_changing_the_parent() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("repository");
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("marker"), "retained repository").unwrap();
        let workspace = crate::filesystem::Workspace::new(&path).unwrap();
        let directory = workspace
            .open_directory(std::path::Path::new(""))
            .unwrap();
        let parent_directory = std::env::current_dir().unwrap();
        let mut command = command("directory");
        set_directory(&mut command, &directory).unwrap();
        #[cfg(unix)]
        {
            std::fs::rename(&path, temporary.path().join("moved")).unwrap();
            std::fs::create_dir(&path).unwrap();
            std::fs::write(path.join("marker"), "replaced repository").unwrap();
        }
        #[cfg(windows)]
        assert!(std::fs::rename(&path, temporary.path().join("moved")).is_err());
        let result = inspect(
            command,
            CancellationToken::new(),
            Instant::now() + Duration::from_secs(10),
            OutputLimit::Complete(4_096),
        )
        .unwrap();
        let InspectionOutput::Complete { stdout, status } = result else {
            panic!("directory fixture output must be complete");
        };
        assert!(status.success());
        assert!(
            String::from_utf8(stdout)
                .unwrap()
                .contains("retained repository")
        );
        assert_eq!(std::env::current_dir().unwrap(), parent_directory);
    }

    #[test]
    fn output_overflow_preserves_strict_and_truncated_contracts() {
        for truncate in [false, true] {
            let start = Instant::now();
            let result = inspect(
                command("output"),
                CancellationToken::new(),
                start + Duration::from_secs(10),
                if truncate {
                    OutputLimit::Truncate(128)
                } else {
                    OutputLimit::Complete(128)
                },
            );
            if truncate {
                assert!(
                    matches!(result, Ok(InspectionOutput::Truncated(bytes)) if bytes.len() == 128)
                );
            } else {
                assert!(matches!(result, Err(InspectionError::OutputLimit(128))));
            }
            assert!(start.elapsed() < Duration::from_secs(10));
        }
    }

    #[test]
    fn silent_process_obeys_deadline_and_cancellation() {
        let start = Instant::now();
        let result = inspect(
            command("wait"),
            CancellationToken::new(),
            start + Duration::from_millis(300),
            OutputLimit::Complete(1_024),
        );
        assert!(matches!(result, Err(InspectionError::TimedOut)));
        assert!(start.elapsed() < Duration::from_secs(10));

        let cancellation = CancellationToken::new();
        let signal = cancellation.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            signal.cancel();
        });
        let result = inspect(
            command("wait"),
            cancellation,
            Instant::now() + Duration::from_secs(10),
            OutputLimit::Complete(1_024),
        );
        canceller.join().unwrap();
        assert!(matches!(result, Err(InspectionError::Cancelled)));
    }

    #[tokio::test]
    async fn synchronous_inspection_can_be_called_inside_an_existing_runtime() {
        let result = inspect(
            command("complete"),
            CancellationToken::new(),
            Instant::now() + Duration::from_secs(10),
            OutputLimit::Complete(4_096),
        )
        .unwrap();
        match result {
            InspectionOutput::Complete { stdout, status } => {
                assert!(status.success());
                assert!(
                    String::from_utf8(stdout)
                        .unwrap()
                        .contains("inspection complete")
                );
            }
            InspectionOutput::Truncated(_) => panic!("small fixture output must be complete"),
        }
    }
}
