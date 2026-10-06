use std::{pin::Pin, process::Stdio};

use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use crate::{
    error::{RuntimeError, failed},
    execution::{
        ExecutionEnvironment, ExecutionPolicy, ExecutionRequest, ExecutionResult, OutputLog,
        environment::is_sensitive_key, supervisor::run_child_process,
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
                .envs(crate::execution::shell_environment::snapshot())
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

            if let Some(scratch) = &request.scratch {
                command.env("ORBIT_SCRATCH", scratch);
            }

            let limits = policy.resources.to_process_limits();
            // Only async-signal-safe system calls may run between fork and exec.
            unsafe {
                command.pre_exec(move || {
                    for (resource, value) in [
                        (libc::RLIMIT_AS, limits.max_memory_bytes),
                        (libc::RLIMIT_FSIZE, limits.max_file_size_bytes),
                        (libc::RLIMIT_NOFILE, limits.max_open_files),
                        (libc::RLIMIT_NPROC, limits.max_processes),
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
                    Ok(())
                });
            }

            let child = command.spawn().map_err(failed)?;
            let process_group = child.id();
            let guard = ProcessGroup(process_group);
            let verification = request
                .fingerprint
                .clone()
                .map(|input| VerificationEvidence {
                    input,
                    output: None,
                    platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
                    isolation: "native user process; no filesystem or network isolation".into(),
                    rust_toolchain: None,
                    writable_snapshot: false,
                    persistent_scratch: request.scratch.is_some(),
                    mounts: Vec::new(),
                });

            let result = run_child_process(
                child,
                request.timeout,
                cancellation,
                output_log,
                "native",
                false,
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

struct ProcessGroup(Option<u32>);

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if let Some(id) = self.0 {
            unsafe {
                libc::kill(-(id as i32), libc::SIGKILL);
            }
        }
    }
}
