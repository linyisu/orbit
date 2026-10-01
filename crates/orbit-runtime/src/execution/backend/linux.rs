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
            let (spec, effective) = prepare_execution_spec_and_policy(request, policy, env);

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

pub(crate) fn prepare_execution_spec_and_policy(
    request: &ExecutionRequest,
    policy: &ExecutionPolicy,
    env: &ExecutionEnvironment,
) -> (ProcessSpec, EffectivePolicy) {
    let mut spec = ProcessSpec::new(&request.command, request.timeout);

    let effective_cwd =
        if request.cwd.as_os_str().is_empty() || request.cwd == std::path::Path::new(".") {
            env.cwd.clone()
        } else if request.cwd.is_absolute() {
            request.cwd.clone()
        } else {
            env.cwd.join(&request.cwd)
        };
    spec.cwd = Some(effective_cwd);

    let mut merged_env = env.env_vars.clone();
    for (key, val) in &policy.env_overrides {
        if !crate::execution::environment::is_sensitive_key(key) {
            merged_env.insert(key.clone(), val.clone());
        }
    }
    spec.env = merged_env;

    spec.scratch = request.scratch.clone();
    spec.read_only_workspace = policy.read_only_workspace;
    spec.rust_toolchain = policy.rust_toolchain.clone();
    spec.mounts = policy.mounts.clone();
    spec.protected_paths = policy.protected_paths.clone();
    spec.opaque_runtime_roots = policy.opaque_runtime_roots.clone();
    spec.limits = policy.resources.to_process_limits();
    spec.fingerprint = request.fingerprint.clone();
    spec.workspace_files = request.workspace_files;
    spec.skipped_entries = request.skipped_entries;

    let ws_resolved = ResolvedPath::from_canonical(&env.workspace_root);
    let base_policy = BasePolicy {
        workspace: ws_resolved,
        network: policy.network,
    };

    let mut effective = EffectivePolicy::new(base_policy);

    for path in &policy.external_read {
        effective
            .persistent_read
            .push(ResolvedPath::from_canonical(path));
    }
    for path in &policy.external_read_write {
        effective
            .persistent_read_write
            .push(ResolvedPath::from_canonical(path));
    }

    (spec, effective)
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, path::PathBuf};

    use super::*;
    use crate::execution::process::MountSpec;

    #[test]
    fn test_prepare_spec_cwd_resolution() {
        let temp = tempfile::tempdir().unwrap();
        let ws = temp.path().canonicalize().unwrap();
        let env = ExecutionEnvironment::new(&ws, &ws).unwrap();
        let policy = ExecutionPolicy::dev_default();

        // 1. Empty cwd defaults to env.cwd
        let req_empty = ExecutionRequest::new("ls", "", Duration::from_secs(5));
        let (spec1, _) = prepare_execution_spec_and_policy(&req_empty, &policy, &env);
        assert_eq!(spec1.cwd, Some(ws.clone()));

        // 2. Relative cwd is joined with env.cwd
        let req_rel = ExecutionRequest::new("ls", "crates/sub", Duration::from_secs(5));
        let (spec2, _) = prepare_execution_spec_and_policy(&req_rel, &policy, &env);
        assert_eq!(spec2.cwd, Some(ws.join("crates/sub")));

        // 3. Absolute cwd is preserved
        let req_abs = ExecutionRequest::new("ls", "/var/custom", Duration::from_secs(5));
        let (spec3, _) = prepare_execution_spec_and_policy(&req_abs, &policy, &env);
        assert_eq!(spec3.cwd, Some(PathBuf::from("/var/custom")));
    }

    #[test]
    fn test_prepare_spec_env_and_sensitive_filtering() {
        let temp = tempfile::tempdir().unwrap();
        let ws = temp.path().canonicalize().unwrap();
        let mut env = ExecutionEnvironment::new(&ws, &ws).unwrap();
        env.set_var("BASE_VAR", "base_val");
        env.set_var("OVERRIDE_VAR", "original_val");

        let mut overrides = BTreeMap::new();
        overrides.insert("OVERRIDE_VAR".to_string(), "overridden_val".to_string());
        overrides.insert("NEW_VAR".to_string(), "new_val".to_string());
        overrides.insert("OPENAI_API_KEY".to_string(), "secret_token".to_string());
        overrides.insert("ORBIT_SESSION".to_string(), "internal_secret".to_string());

        let policy = ExecutionPolicy::dev_default().with_env_overrides(overrides);
        let req = ExecutionRequest::new("ls", &ws, Duration::from_secs(5));

        let (spec, _) = prepare_execution_spec_and_policy(&req, &policy, &env);
        assert_eq!(spec.env.get("BASE_VAR"), Some(&"base_val".to_string()));
        assert_eq!(
            spec.env.get("OVERRIDE_VAR"),
            Some(&"overridden_val".to_string())
        );
        assert_eq!(spec.env.get("NEW_VAR"), Some(&"new_val".to_string()));
        assert!(!spec.env.contains_key("OPENAI_API_KEY"));
        assert!(!spec.env.contains_key("ORBIT_SESSION"));
    }

    #[test]
    fn test_mount_and_external_grants_separation() {
        let temp = tempfile::tempdir().unwrap();
        let ws = temp.path().canonicalize().unwrap();
        let env = ExecutionEnvironment::new(&ws, &ws).unwrap();

        let mount = MountSpec::read_only("/host/packages", "/opt/packages");
        let ext_read = PathBuf::from("/usr/include");
        let ext_write = PathBuf::from("/var/log");

        let policy = ExecutionPolicy::dev_default()
            .with_mounts(vec![mount.clone()])
            .with_external_read(vec![ext_read.clone()])
            .with_external_read_write(vec![ext_write.clone()]);

        let req = ExecutionRequest::new("cargo check", &ws, Duration::from_secs(10));
        let (spec, effective) = prepare_execution_spec_and_policy(&req, &policy, &env);

        // Mounts remain in spec.mounts for bwrap container mounting
        assert_eq!(spec.mounts, vec![mount]);

        // External grants are mapped to effective policy
        assert_eq!(
            effective.persistent_read,
            vec![ResolvedPath::from_canonical(&ext_read)]
        );
        assert_eq!(
            effective.persistent_read_write,
            vec![ResolvedPath::from_canonical(&ext_write)]
        );

        // Mount sources MUST NOT be duplicated into external grants
        assert!(
            !effective
                .persistent_read
                .iter()
                .any(|p| p.canonical() == std::path::Path::new("/host/packages"))
        );
        assert!(
            !effective
                .persistent_read_write
                .iter()
                .any(|p| p.canonical() == std::path::Path::new("/host/packages"))
        );
    }
}
