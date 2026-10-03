use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use crate::{
    error::{RuntimeError, failed},
    execution::process::MountSpec,
};

/// ExecutionEnvironment resolves and sanitizes the workspace environment, toolchain paths,
/// and environment variables for subprocess execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionEnvironment {
    pub workspace_root: PathBuf,
    pub cwd: PathBuf,
    pub env_vars: BTreeMap<String, String>,
}

impl ExecutionEnvironment {
    /// Creates a new execution environment for the given workspace and working directory.
    /// Cleans Orbit authentication secrets while retaining essential toolchain and developer variables.
    pub fn new(workspace_root: &Path, cwd: &Path) -> Result<Self, RuntimeError> {
        let canonical_ws = workspace_root.canonicalize().map_err(|e| {
            failed(format!(
                "workspace root '{}' must exist: {e}",
                workspace_root.display()
            ))
        })?;

        let canonical_cwd = if cwd.is_absolute() {
            cwd.to_path_buf()
        } else {
            canonical_ws.join(cwd)
        };

        let canonical_cwd = canonical_cwd.canonicalize().map_err(|e| {
            failed(format!(
                "working directory '{}' must exist: {e}",
                canonical_cwd.display()
            ))
        })?;

        let mut env_vars = BTreeMap::new();
        populate_safe_environment(&mut env_vars);

        Ok(Self {
            workspace_root: canonical_ws,
            cwd: canonical_cwd,
            env_vars,
        })
    }

    /// Sets or overrides an environment variable in the subprocess environment.
    pub fn set_var(&mut self, key: impl Into<String>, val: impl Into<String>) {
        let key = key.into();

        if !is_sensitive_key(&key) {
            self.env_vars.insert(key, val.into());
        }
    }

    /// Remaps path-based environment variables (including CARGO_HOME and RUSTUP_HOME)
    /// against authorized mounts and external capability grants, ensuring that unmounted
    /// host paths do not leak into the sandboxed subprocess where they are inaccessible.
    pub fn resolve_path_environment(
        &mut self,
        mounts: &[MountSpec],
        external_grants: &[PathBuf],
        container_home: &Path,
    ) {
        remap_path_environment(
            &mut self.env_vars,
            &self.workspace_root,
            mounts,
            external_grants,
            container_home,
        );
    }
}

/// Identifies sensitive keys (e.g. AI provider API keys, Orbit secrets) that must never
/// leak into sandboxed subprocesses.
pub fn is_sensitive_key(key: &str) -> bool {
    let upper = key.to_uppercase();

    upper.starts_with("ORBIT_")
        || upper.ends_with("_API_KEY")
        || upper.ends_with("_TOKEN")
        || upper.ends_with("_SECRET")
        || matches!(
            upper.as_str(),
            "OPENAI_API_KEY"
                | "ANTHROPIC_API_KEY"
                | "GEMINI_API_KEY"
                | "DEEPSEEK_API_KEY"
                | "AWS_SECRET_ACCESS_KEY"
                | "GITHUB_TOKEN"
        )
}

fn populate_safe_environment(env: &mut BTreeMap<String, String>) {
    const SAFE_PASSTHROUGH: &[&str] = &[
        "PATH",
        "HOME",
        "USER",
        "LOGNAME",
        "SHELL",
        "TERM",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "CARGO_HOME",
        "RUSTUP_HOME",
        "RUSTUP_TOOLCHAIN",
        "CARGO_TERM_COLOR",
    ];

    for key in SAFE_PASSTHROUGH {
        if let Ok(val) = std::env::var(key) {
            env.insert((*key).to_string(), val);
        }
    }
}

/// Remaps environment variables that contain host filesystem paths (CARGO_HOME, RUSTUP_HOME, HOME)
/// to their corresponding container paths based on authorized mounts and external capability grants.
/// If a host path is not authorized/mounted into the container, it is safely remapped to
/// container-local storage under `container_home` (e.g. /scratch/home or /tmp/home) so subprocesses
/// do not fail on missing host directories.
pub fn remap_path_environment(
    env: &mut BTreeMap<String, String>,
    workspace_root: &Path,
    mounts: &[MountSpec],
    external_grants: &[PathBuf],
    container_home: &Path,
) {
    if let Some(cargo_val) = env.get("CARGO_HOME").cloned() {
        let cargo_path = PathBuf::from(&cargo_val);
        if let Some(remapped) = remap_single_path(
            &cargo_path,
            workspace_root,
            mounts,
            external_grants,
            container_home,
        ) {
            env.insert(
                "CARGO_HOME".to_string(),
                remapped.to_string_lossy().to_string(),
            );
        } else {
            env.insert(
                "CARGO_HOME".to_string(),
                container_home.join(".cargo").to_string_lossy().to_string(),
            );
        }
    } else {
        env.insert(
            "CARGO_HOME".to_string(),
            container_home.join(".cargo").to_string_lossy().to_string(),
        );
    }

    if let Some(rustup_val) = env.get("RUSTUP_HOME").cloned() {
        let rustup_path = PathBuf::from(&rustup_val);
        if let Some(remapped) = remap_single_path(
            &rustup_path,
            workspace_root,
            mounts,
            external_grants,
            container_home,
        ) {
            env.insert(
                "RUSTUP_HOME".to_string(),
                remapped.to_string_lossy().to_string(),
            );
        } else {
            env.insert(
                "RUSTUP_HOME".to_string(),
                container_home.join(".rustup").to_string_lossy().to_string(),
            );
        }
    }

    if let Some(home_val) = env.get("HOME").cloned() {
        let home_path = PathBuf::from(&home_val);
        if let Some(remapped) = remap_single_path(
            &home_path,
            workspace_root,
            mounts,
            external_grants,
            container_home,
        ) {
            env.insert("HOME".to_string(), remapped.to_string_lossy().to_string());
        } else {
            env.insert(
                "HOME".to_string(),
                container_home.to_string_lossy().to_string(),
            );
        }
    } else {
        env.insert(
            "HOME".to_string(),
            container_home.to_string_lossy().to_string(),
        );
    }
}

pub fn remap_single_path(
    path: &Path,
    workspace_root: &Path,
    mounts: &[MountSpec],
    external_grants: &[PathBuf],
    container_home: &Path,
) -> Option<PathBuf> {
    let canon = path.canonicalize().ok();
    let mut candidates = vec![path];
    if let Some(c) = canon.as_deref().filter(|&c| c != path) {
        candidates.push(c);
    }

    for mount in mounts {
        let Ok(target) = mount.normalized_target() else {
            continue;
        };
        let mount_source_canon = mount.source.canonicalize().ok();
        for candidate in &candidates {
            if let Ok(rel) = candidate.strip_prefix(&mount.source) {
                return Some(if rel.as_os_str().is_empty() {
                    target.clone()
                } else {
                    target.join(rel)
                });
            }
            if let Some(rel) = mount_source_canon
                .as_ref()
                .and_then(|sc| candidate.strip_prefix(sc).ok())
            {
                return Some(if rel.as_os_str().is_empty() {
                    target.clone()
                } else {
                    target.join(rel)
                });
            }
        }
        if path == target || path.starts_with(&target) {
            return Some(path.to_path_buf());
        }
    }

    let ws_canon = workspace_root.canonicalize().ok();
    for candidate in &candidates {
        if let Ok(rel) = candidate.strip_prefix(workspace_root) {
            return Some(if rel.as_os_str().is_empty() {
                PathBuf::from("/workspace")
            } else {
                Path::new("/workspace").join(rel)
            });
        }
        if let Some(rel) = ws_canon
            .as_ref()
            .and_then(|wc| candidate.strip_prefix(wc).ok())
        {
            return Some(if rel.as_os_str().is_empty() {
                PathBuf::from("/workspace")
            } else {
                Path::new("/workspace").join(rel)
            });
        }
    }

    for grant in external_grants {
        let grant_canon = grant.canonicalize().unwrap_or_else(|_| grant.clone());
        if let Some(ref c) = canon {
            if let Ok(rel) = c.strip_prefix(&grant_canon) {
                return Some(if rel.as_os_str().is_empty() {
                    grant_canon.clone()
                } else {
                    grant_canon.join(rel)
                });
            }
            if let Ok(rel) = c.strip_prefix(grant) {
                return Some(if rel.as_os_str().is_empty() {
                    grant_canon.clone()
                } else {
                    grant_canon.join(rel)
                });
            }
        }
        if let Ok(rel) = path.strip_prefix(&grant_canon) {
            return Some(if rel.as_os_str().is_empty() {
                grant_canon.clone()
            } else {
                grant_canon.join(rel)
            });
        }
        if let Ok(rel) = path.strip_prefix(grant) {
            return Some(if rel.as_os_str().is_empty() {
                grant_canon.clone()
            } else {
                grant_canon.join(rel)
            });
        }
    }

    if path.starts_with("/workspace")
        || path.starts_with("/scratch")
        || path.starts_with("/opt/orbit-rust")
        || path == container_home
        || path.starts_with(container_home)
    {
        return Some(path.to_path_buf());
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_key_detection() {
        assert!(is_sensitive_key("OPENAI_API_KEY"));
        assert!(is_sensitive_key("ANTHROPIC_API_KEY"));
        assert!(is_sensitive_key("GEMINI_API_KEY"));
        assert!(is_sensitive_key("DEEPSEEK_API_KEY"));
        assert!(is_sensitive_key("ORBIT_SESSION_ID"));
        assert!(is_sensitive_key("MY_APP_SECRET"));
        assert!(is_sensitive_key("AUTH_TOKEN"));

        assert!(!is_sensitive_key("PATH"));
        assert!(!is_sensitive_key("HOME"));
        assert!(!is_sensitive_key("CARGO_TERM_COLOR"));
    }

    #[test]
    fn environment_initialization() {
        let temp = tempfile::tempdir().unwrap();
        let ws = temp.path();
        let mut env = ExecutionEnvironment::new(ws, ws).unwrap();

        assert_eq!(env.workspace_root, ws.canonicalize().unwrap());
        assert_eq!(env.cwd, ws.canonicalize().unwrap());

        // Sensitive variables must not be set
        env.set_var("OPENAI_API_KEY", "secret-value");
        assert!(!env.env_vars.contains_key("OPENAI_API_KEY"));

        // Safe variables can be set
        env.set_var("CUSTOM_VAR", "hello");
        assert_eq!(env.env_vars.get("CUSTOM_VAR"), Some(&"hello".to_string()));
    }

    #[test]
    fn test_remap_path_environment() {
        let temp = tempfile::tempdir().unwrap();
        let ws = temp.path().canonicalize().unwrap();
        let mut env = BTreeMap::new();
        env.insert("CARGO_HOME".to_string(), "/home/user/.cargo".to_string());
        env.insert("RUSTUP_HOME".to_string(), "/home/user/.rustup".to_string());

        let mounts = vec![MountSpec::read_only(
            "/home/user/.cargo",
            "/opt/orbit-cache/cargo",
        )];
        let external_grants = vec![PathBuf::from("/var/shared")];
        let container_home = Path::new("/tmp/home");

        remap_path_environment(&mut env, &ws, &mounts, &external_grants, container_home);

        // Mounted CARGO_HOME should be remapped to mount target
        assert_eq!(
            env.get("CARGO_HOME"),
            Some(&"/opt/orbit-cache/cargo".to_string())
        );

        // Unmounted RUSTUP_HOME should be safely remapped to container home
        assert_eq!(
            env.get("RUSTUP_HOME"),
            Some(&"/tmp/home/.rustup".to_string())
        );

        // HOME should be set to container home
        assert_eq!(env.get("HOME"), Some(&"/tmp/home".to_string()));
    }

    #[test]
    fn test_remap_path_environment_host_tmp_and_opt_mounts() {
        let temp = tempfile::tempdir().unwrap();
        let ws = temp.path().canonicalize().unwrap();

        let host_tmp_cache = temp.path().join("tmp_cargo_cache");
        std::fs::create_dir(&host_tmp_cache).unwrap();
        let host_tmp_canon = host_tmp_cache.canonicalize().unwrap();

        let mut env = BTreeMap::new();
        env.insert(
            "CARGO_HOME".to_string(),
            host_tmp_canon.to_string_lossy().to_string(),
        );

        let mounts = vec![MountSpec::read_only(&host_tmp_canon, "/opt/review")];
        let container_home = Path::new("/tmp/home");

        remap_path_environment(&mut env, &ws, &mounts, &[], container_home);

        // Host /tmp/... cache mounted to /opt/review must remap to /opt/review, NOT remain host /tmp/...
        assert_eq!(env.get("CARGO_HOME"), Some(&"/opt/review".to_string()));

        // An unmounted host /tmp path must NOT be treated as a container path; must fallback to container home
        let outside_temp = tempfile::tempdir().unwrap();
        let unmounted_tmp = outside_temp.path().join("unmounted_cargo");
        std::fs::create_dir(&unmounted_tmp).unwrap();
        let unmounted_canon = unmounted_tmp.canonicalize().unwrap();
        let mut env_unmounted = BTreeMap::new();
        env_unmounted.insert(
            "CARGO_HOME".to_string(),
            unmounted_canon.to_string_lossy().to_string(),
        );
        remap_path_environment(&mut env_unmounted, &ws, &[], &[], container_home);
        assert_eq!(
            env_unmounted.get("CARGO_HOME"),
            Some(&"/tmp/home/.cargo".to_string())
        );
    }

    #[test]
    fn test_remap_single_path_symlink_to_external_grant() {
        let temp = tempfile::tempdir().unwrap();
        let ws = temp.path().join("workspace");
        std::fs::create_dir(&ws).unwrap();

        let real_cache = temp.path().join("real_cargo_cache");
        std::fs::create_dir(&real_cache).unwrap();
        let real_cache_canon = real_cache.canonicalize().unwrap();

        let symlink_cache = temp.path().join("symlink_cargo_cache");
        std::os::unix::fs::symlink(&real_cache, &symlink_cache).unwrap();

        let container_home = Path::new("/tmp/home");
        let external_grants = vec![real_cache_canon.clone()];

        // Passing the host symlink path to remap_single_path must return the canonical path mounted in the container
        let remapped =
            remap_single_path(&symlink_cache, &ws, &[], &external_grants, container_home);
        assert_eq!(remapped, Some(real_cache_canon));
    }
}
