use std::{pin::Pin, time::Duration};
use tokio_util::sync::CancellationToken;

use crate::{
    error::RuntimeError,
    execution::{
        backend::ExecutionBackend,
        environment::ExecutionEnvironment,
        output::OutputLog,
        policy::ExecutionPolicy,
        process::ProcessSpec,
        request::{ExecutionRequest, ExecutionResult},
        sandbox::{BwrapSandboxBackend, SandboxBackend},
    },
    filesystem::ResolvedPath,
    policy::{BasePolicy, EffectivePolicy},
};

/// BwrapBackend implements ExecutionBackend on Linux using Bubblewrap and user namespaces.
pub struct BwrapBackend {
    inner: BwrapSandboxBackend,
}

impl BwrapBackend {
    pub fn new() -> Result<Self, RuntimeError> {
        let inner = BwrapSandboxBackend::new()?;
        Ok(Self { inner })
    }
}

impl ExecutionBackend for BwrapBackend {
    fn name(&self) -> &'static str {
        "bubblewrap"
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
    ) -> Pin<Box<dyn std::future::Future<Output = Result<ExecutionResult, RuntimeError>> + Send + 'a>>
    {
        Box::pin(async move {
            let mut spec = ProcessSpec::new(&request.command, request.timeout);

            spec.scratch = request.scratch.clone();
            spec.read_only_workspace = policy.read_only_workspace;
            spec.rust_toolchain = policy.rust_toolchain.clone();
            spec.mounts = policy.mounts.clone();
            spec.protected_paths = policy.protected_paths.clone();
            spec.opaque_runtime_roots = policy.opaque_runtime_roots.clone();
            spec.limits = policy.resources.to_process_limits();

            let ws_resolved = ResolvedPath::from_canonical(&env.workspace_root);
            let base_policy = BasePolicy {
                workspace: ws_resolved,
                network: policy.network,
            };

            let mut effective = EffectivePolicy::new(base_policy);

            for mount in &policy.mounts {
                let resolved = ResolvedPath::from_canonical(&mount.source);
                match mount.access {
                    crate::execution::process::MountAccess::ReadOnly => {
                        effective.persistent_read.push(resolved);
                    }
                    crate::execution::process::MountAccess::ReadWrite => {
                        effective.persistent_read_write.push(resolved);
                    }
                }
            }

            let cmd_res = self
                .inner
                .run(spec, &effective, output_log, cancellation)
                .await?;

            Ok(ExecutionResult {
                exit_code: cmd_res.exit_code,
                elapsed: Duration::from_millis(cmd_res.elapsed_ms),
                stdout: cmd_res.stdout,
                stderr: cmd_res.stderr,
                timed_out: cmd_res.timed_out,
                cancelled: cmd_res.cancelled,
                output_truncated: cmd_res.output_truncated,
                stdout_total_bytes: cmd_res.stdout_total_bytes,
                stderr_total_bytes: cmd_res.stderr_total_bytes,
                output_id: cmd_res.output_id,
                retained_output_truncated: cmd_res.retained_output_truncated,
                sandbox: cmd_res.sandbox,
                verification_error: cmd_res.verification_error,
                verification: cmd_res.verification,
            })
        })
    }
}
