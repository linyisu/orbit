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

            let shell = shell(&env.env_vars)?;
            let mut command = Command::new(&shell);
            command.args(shell_arguments(&shell, &request.command));
            command
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
                .kill_on_drop(true);

            let limits = policy.resources.to_process_limits();
            limits.validate_native()?;
            // Only async-signal-safe system calls may run between fork and exec.
            #[cfg(unix)]
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
                    && std::path::Path::new(&shell).is_absolute()
                    && std::fs::metadata(&shell)
                        .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
                {
                    RuntimeError::DependencyUnavailable {
                        program: shell,
                        message: error.to_string(),
                    }
                } else {
                    RuntimeError::Io(error)
                }
            })?;
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
                Some(guard),
            )
            .await;
            result.map(Into::into)
        })
    }
}

fn shell(environment: &std::collections::BTreeMap<String, String>) -> std::io::Result<String> {
    #[cfg(unix)]
    {
        Ok(environment
            .get("SHELL")
            .cloned()
            .unwrap_or_else(|| "/bin/sh".into()))
    }
    #[cfg(windows)]
    {
        for key in ["SHELL", "ComSpec"] {
            if let Some((_, value)) = environment
                .iter()
                .find(|(name, value)| name.eq_ignore_ascii_case(key) && !value.is_empty())
            {
                return Ok(value.clone());
            }
        }
        super::super::process::system_executable("cmd.exe")
            .map(|path| path.to_string_lossy().into_owned())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "Windows command interpreter is unavailable",
                )
            })
    }
}

fn shell_arguments<'a>(shell: &str, command: &'a str) -> Vec<&'a str> {
    #[cfg(windows)]
    {
        let name = std::path::Path::new(shell)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_ascii_lowercase();
        if name == "cmd" {
            return vec!["/D", "/S", "/C", command];
        }
        if matches!(name.as_str(), "powershell" | "pwsh") {
            return vec![
                "-NoLogo",
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                command,
            ];
        }
    }
    #[cfg(unix)]
    let _ = shell;
    vec!["-c", command]
}
