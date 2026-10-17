use std::{pin::Pin, process::Stdio};

use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::{
    error::RuntimeError,
    execution::{
        ExecutionEnvironment, ExecutionPolicy, ExecutionRequest, ExecutionResult, OutputLog,
        environment::is_sensitive_key, process::ProcessGroup, supervisor::run_child_process,
    },
    verification::VerificationEvidence,
};

use super::ExecutionBackend;

pub struct NativeBackend;

impl ExecutionBackend for NativeBackend {
    fn name(&self) -> &'static str {
        "native"
    }

    fn is_available(&self) -> Result<(), RuntimeError> {
        Ok(())
    }

    fn execute<'a>(
        &'a self,
        request: &'a ExecutionRequest,
        policy: &'a ExecutionPolicy,
        env: &'a ExecutionEnvironment,
        output_log: Option<OutputLog>,
        cancellation: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<ExecutionResult, RuntimeError>> + Send + 'a>> {
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(RuntimeError::Cancelled);
            }

            let shell = env
                .env_vars
                .get("SHELL")
                .map(String::as_str)
                .unwrap_or("/bin/sh");
            let mut command = Command::new(shell);
            command
                .args(["-c", &request.command])
                .current_dir(&env.cwd)
                .env_clear()
                .envs(&env.env_vars)
                .envs(
                    policy
                        .env_overrides
                        .iter()
                        .filter(|(key, _)| !is_sensitive_key(key)),
                )
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .process_group(0)
                .kill_on_drop(true);

            let limits = policy.resources.to_process_limits();
            // Only async-signal-safe system calls may run between fork and exec.
            unsafe {
                command.pre_exec(move || {
                    for (resource, value) in [
                        (libc::RLIMIT_AS, limits.max_memory_bytes),
                        (libc::RLIMIT_FSIZE, limits.max_file_size_bytes),
                        (libc::RLIMIT_NOFILE, limits.max_open_files),
                    ] {
                        if value == 0 || value == u64::MAX {
                            continue;
                        }
                        let mut current = libc::rlimit {
                            rlim_cur: 0,
                            rlim_max: 0,
                        };
                        if libc::getrlimit(resource, &mut current) != 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                        current.rlim_cur = (value as libc::rlim_t).min(current.rlim_max);
                        if libc::setrlimit(resource, &current) != 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                    }
                    // RLIMIT_NPROC counts every process owned by the target user.
                    // Native commands retain the host limit rather than a sandbox quota.
                    Ok(())
                });
            }

            let (child, guard) = ProcessGroup::spawn(&mut command).map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound
                    && std::path::Path::new(shell).is_absolute()
                    && std::fs::metadata(shell)
                        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
                {
                    RuntimeError::DependencyUnavailable {
                        program: shell.into(),
                        message: error.to_string(),
                    }
                } else {
                    RuntimeError::Io(error)
                }
            })?;
            let process_group = child.id();
            let verification = request
                .fingerprint
                .clone()
                .map(|input| VerificationEvidence {
                    input,
                    output: None,
                    platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
                    isolation: "native user process; no filesystem or network isolation".into(),
                });

            let result = run_child_process(
                child,
                request.timeout,
                cancellation,
                output_log,
                "native",
                request.workspace_files,
                request.skipped_entries,
                verification,
                process_group,
            )
            .await;
            drop(guard);
            result.map(Into::into)
        })
    }
}
