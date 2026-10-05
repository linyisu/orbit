use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use crate::{
    error::{RuntimeError, failed},
    execution::process::MountSpec,
};

/// Sanitized host environment projected onto the sandbox mounts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionEnvironment {
    pub workspace_root: PathBuf,
    pub cwd: PathBuf,
    pub env_vars: BTreeMap<String, String>,
}

impl ExecutionEnvironment {
    /// Canonicalize paths and inherit only the approved environment keys.
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

    pub fn set_var(&mut self, key: impl Into<String>, val: impl Into<String>) {
        let key = key.into();

        if !is_sensitive_key(&key) {
            self.env_vars.insert(key, val.into());
        }
    }

    /// Map authorized host paths to sandbox paths; use local storage for unmounted paths.
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

/// Reject provider credentials and Orbit secrets from subprocess environments.
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

/// Remap Cargo, Rustup and home paths against authorized mounts and grants.
/// Unmounted paths fall back to `container_home`; an absent RUSTUP_HOME stays absent.
pub fn remap_path_environment(
    env: &mut BTreeMap<String, String>,
    workspace_root: &Path,
    mounts: &[MountSpec],
    external_grants: &[PathBuf],
    container_home: &Path,
) {
    for (key, fallback, initialize_when_absent) in [
        ("CARGO_HOME", container_home.join(".cargo"), true),
        ("RUSTUP_HOME", container_home.join(".rustup"), false),
        ("HOME", container_home.to_path_buf(), true),
    ] {
        let value = env.get(key);
        if value.is_none() && !initialize_when_absent {
            continue;
        }

        let remapped = value
            .and_then(|value| {
                remap_single_path(
                    Path::new(value),
                    workspace_root,
                    mounts,
                    external_grants,
                    container_home,
                )
            })
            .unwrap_or(fallback);

        env.insert(key.to_owned(), remapped.to_string_lossy().into_owned());
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

        env.set_var("OPENAI_API_KEY", "secret-value");
        assert!(!env.env_vars.contains_key("OPENAI_API_KEY"));

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

        assert_eq!(
            env.get("CARGO_HOME"),
            Some(&"/opt/orbit-cache/cargo".to_string())
        );

        assert_eq!(
            env.get("RUSTUP_HOME"),
            Some(&"/tmp/home/.rustup".to_string())
        );

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

        assert_eq!(env.get("CARGO_HOME"), Some(&"/opt/review".to_string()));

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

        let remapped =
            remap_single_path(&symlink_cache, &ws, &[], &external_grants, container_home);
        assert_eq!(remapped, Some(real_cache_canon));
    }
}
