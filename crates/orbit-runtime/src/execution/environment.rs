use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

use crate::error::{RuntimeError, failed};

/// Real working directory and target user environment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionEnvironment {
    pub workspace_root: PathBuf,
    pub cwd: PathBuf,
    pub env_vars: BTreeMap<String, String>,
}

impl ExecutionEnvironment {
    /// Canonicalize paths and inherit the target shell environment.
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

        let env_vars = super::shell_environment::snapshot();

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
