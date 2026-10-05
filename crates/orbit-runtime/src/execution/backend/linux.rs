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
            let (spec, effective) = prepare_execution_spec_and_policy(request, policy, env)?;

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
) -> Result<(ProcessSpec, EffectivePolicy), RuntimeError> {
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

    let container_home = if request.scratch.is_some() {
        std::path::Path::new("/scratch/home")
    } else {
        std::path::Path::new("/tmp/home")
    };

    let mut external_paths = Vec::new();
    for path in policy
        .external_read
        .iter()
        .chain(policy.external_read_write.iter())
    {
        let resolved = ResolvedPath::resolve(path, Some(&env.workspace_root)).map_err(|e| {
            crate::error::failed(format!(
                "failed to resolve external path '{}': {e}",
                path.display()
            ))
        })?;

        external_paths.push(resolved.canonical().to_path_buf());
    }

    crate::execution::environment::remap_path_environment(
        &mut merged_env,
        &env.workspace_root,
        &policy.mounts,
        &external_paths,
        container_home,
    );

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
    spec.unmask_git = policy.unmask_git;
    spec.allow_git_config_write = policy.allow_git_config_write;

    let ws_resolved = ResolvedPath::from_canonical(&env.workspace_root);
    let base_policy = BasePolicy {
        workspace: ws_resolved,
        network: policy.network,
    };

    let mut effective = EffectivePolicy::new(base_policy);
    effective.read_templates = policy.read_templates.clone();
    effective.write_templates = policy.write_templates.clone();

    {
        let mut protected_guard = effective
            .protected_paths
            .write()
            .map_err(|_| crate::error::failed("protected path policy lock is poisoned"))?;

        for path in &policy.protected_paths {
            let abs_path = if path.is_absolute() {
                path.clone()
            } else {
                env.workspace_root.join(path)
            };
            let canon = abs_path.canonicalize().unwrap_or(abs_path);
            if !protected_guard.contains(&canon) {
                protected_guard.push(canon);
            }
        }
    }

    {
        let mut opaque_guard = effective
            .opaque_runtime_roots
            .write()
            .map_err(|_| crate::error::failed("opaque roots policy lock is poisoned"))?;
        let mut protected_guard = effective
            .protected_paths
            .write()
            .map_err(|_| crate::error::failed("protected path policy lock is poisoned"))?;

        for path in &policy.opaque_runtime_roots {
            let abs_path = if path.is_absolute() {
                path.clone()
            } else {
                env.workspace_root.join(path)
            };
            let canon = abs_path.canonicalize().unwrap_or(abs_path);
            if !opaque_guard.contains(&canon) {
                opaque_guard.push(canon.clone());
            }
            if !protected_guard.contains(&canon) {
                protected_guard.push(canon);
            }
        }
    }

    {
        let protected_guard = effective
            .protected_paths
            .read()
            .map_err(|_| crate::error::failed("protected path policy lock is poisoned"))?;

        for path in &policy.external_read {
            let resolved = ResolvedPath::resolve(path, Some(&env.workspace_root)).map_err(|e| {
                crate::error::failed(format!(
                    "failed to resolve external_read path '{}': {e}",
                    path.display()
                ))
            })?;

            for prot in protected_guard.iter() {
                if resolved.canonical() == prot || resolved.canonical().starts_with(prot) {
                    return Err(crate::error::failed(format!(
                        "external_read path '{}' overlaps protected path '{}'",
                        resolved.canonical().display(),
                        prot.display()
                    )));
                }
            }

            effective.persistent_read.push(resolved);
        }

        for path in &policy.external_read_write {
            let resolved = ResolvedPath::resolve(path, Some(&env.workspace_root)).map_err(|e| {
                crate::error::failed(format!(
                    "failed to resolve external_read_write path '{}': {e}",
                    path.display()
                ))
            })?;

            for prot in protected_guard.iter() {
                if resolved.canonical() == prot || resolved.canonical().starts_with(prot) {
                    return Err(crate::error::failed(format!(
                        "external_read_write path '{}' overlaps protected path '{}'",
                        resolved.canonical().display(),
                        prot.display()
                    )));
                }
            }

            effective.persistent_read_write.push(resolved);
        }
    }

    Ok((spec, effective))
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

        let req_empty = ExecutionRequest::new("ls", "", Duration::from_secs(5));
        let (spec1, _) = prepare_execution_spec_and_policy(&req_empty, &policy, &env).unwrap();
        assert_eq!(spec1.cwd, Some(ws.clone()));

        let req_rel = ExecutionRequest::new("ls", "crates/sub", Duration::from_secs(5));
        let (spec2, _) = prepare_execution_spec_and_policy(&req_rel, &policy, &env).unwrap();
        assert_eq!(spec2.cwd, Some(ws.join("crates/sub")));

        let req_abs = ExecutionRequest::new("ls", "/var/custom", Duration::from_secs(5));
        let (spec3, _) = prepare_execution_spec_and_policy(&req_abs, &policy, &env).unwrap();
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

        let (spec, _) = prepare_execution_spec_and_policy(&req, &policy, &env).unwrap();
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
        let (spec, effective) = prepare_execution_spec_and_policy(&req, &policy, &env).unwrap();

        assert_eq!(spec.mounts, vec![mount]);

        assert_eq!(
            effective.persistent_read,
            vec![ResolvedPath::from_canonical(&ext_read)]
        );
        assert_eq!(
            effective.persistent_read_write,
            vec![ResolvedPath::from_canonical(&ext_write)]
        );

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

    #[test]
    fn test_prepare_spec_protects_paths_and_opaque_roots_against_mounts() {
        let ws_temp = tempfile::tempdir().unwrap();
        let ws = ws_temp.path().canonicalize().unwrap();
        let env = ExecutionEnvironment::new(&ws, &ws).unwrap();

        let external_temp = tempfile::tempdir().unwrap();
        let secret_file = external_temp.path().join("secret.key");
        std::fs::write(&secret_file, "SENSITIVE_SECRET").unwrap();
        let secret_file_canon = secret_file.canonicalize().unwrap();

        let opaque_dir = external_temp.path().join("opaque_runtime");
        std::fs::create_dir(&opaque_dir).unwrap();
        let opaque_dir_canon = opaque_dir.canonicalize().unwrap();

        let policy = ExecutionPolicy::dev_default()
            .with_protected_paths(vec![secret_file_canon.clone()])
            .with_opaque_runtime_roots(vec![opaque_dir_canon.clone()]);

        let req = ExecutionRequest::new("cat", &ws, Duration::from_secs(5));
        let (spec, effective) = prepare_execution_spec_and_policy(&req, &policy, &env).unwrap();

        let protected = effective.protected_paths.read().unwrap();
        assert!(protected.contains(&secret_file_canon));
        assert!(protected.contains(&opaque_dir_canon));
        drop(protected);

        let parent_mount = MountSpec::read_only(external_temp.path(), "/opt/leak");
        assert!(
            crate::execution::sandbox::validate_mounts(&[parent_mount], &effective).is_err(),
            "mounting parent directory of a protected path must be rejected"
        );

        let exact_mount = MountSpec::read_only(&secret_file, "/opt/leak_exact");
        assert!(
            crate::execution::sandbox::validate_mounts(&[exact_mount], &effective).is_err(),
            "mounting a protected file directly must be rejected"
        );

        let opaque_mount = MountSpec::read_only(&opaque_dir, "/opt/leak_opaque");
        assert!(
            crate::execution::sandbox::validate_mounts(&[opaque_mount], &effective).is_err(),
            "mounting an opaque runtime root must be rejected"
        );

        assert_eq!(spec.protected_paths, vec![secret_file_canon]);
        assert_eq!(spec.opaque_runtime_roots, vec![opaque_dir_canon]);
    }

    #[test]
    fn test_prepare_spec_remaps_rust_toolchain_env_vars() {
        let ws_temp = tempfile::tempdir().unwrap();
        let ws = ws_temp.path().canonicalize().unwrap();
        let mut env = ExecutionEnvironment::new(&ws, &ws).unwrap();
        env.set_var("CARGO_HOME", "/home/user/.cargo");
        env.set_var("RUSTUP_HOME", "/home/user/.rustup");

        let policy_unmounted = ExecutionPolicy::dev_default();
        let req = ExecutionRequest::new("cargo build", &ws, Duration::from_secs(10));
        let (spec_unmounted, _) =
            prepare_execution_spec_and_policy(&req, &policy_unmounted, &env).unwrap();

        assert_eq!(
            spec_unmounted.env.get("CARGO_HOME"),
            Some(&"/tmp/home/.cargo".to_string())
        );
        assert_eq!(
            spec_unmounted.env.get("RUSTUP_HOME"),
            Some(&"/tmp/home/.rustup".to_string())
        );

        let cache_mount = MountSpec::read_only("/home/user/.cargo", "/opt/orbit-cache/cargo");
        let policy_mounted = ExecutionPolicy::dev_default().with_mounts(vec![cache_mount]);
        let (spec_mounted, _) =
            prepare_execution_spec_and_policy(&req, &policy_mounted, &env).unwrap();

        assert_eq!(
            spec_mounted.env.get("CARGO_HOME"),
            Some(&"/opt/orbit-cache/cargo".to_string())
        );
        assert_eq!(
            spec_mounted.env.get("RUSTUP_HOME"),
            Some(&"/tmp/home/.rustup".to_string())
        );
    }

    #[test]
    fn test_prepare_spec_rejects_symlink_to_protected_file_in_external_read() {
        let ws_temp = tempfile::tempdir().unwrap();
        let ws = ws_temp.path().canonicalize().unwrap();
        let env = ExecutionEnvironment::new(&ws, &ws).unwrap();

        let secret = ws.join("secret.env");
        std::fs::write(&secret, "PASSWORD=123").unwrap();

        let outside_temp = tempfile::tempdir().unwrap();
        let symlink_path = outside_temp.path().join("external_symlink");
        std::os::unix::fs::symlink(&secret, &symlink_path).unwrap();

        let policy = ExecutionPolicy::dev_default()
            .with_protected_paths(vec![secret.clone()])
            .with_external_read(vec![symlink_path]);

        let req = ExecutionRequest::new("cat external_symlink", &ws, Duration::from_secs(5));
        let result = prepare_execution_spec_and_policy(&req, &policy, &env);

        assert!(
            result.is_err(),
            "external authorization pointing to a protected file via symlink must be rejected"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("overlaps protected path"),
            "unexpected error message: {err}"
        );
    }
}
