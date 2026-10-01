use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use crate::error::{RuntimeError, failed};

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
}
