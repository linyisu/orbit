use std::{collections::BTreeMap, path::PathBuf};

use crate::{
    capability::NetworkPolicy,
    execution::{process::MountSpec, resources::ResourceLimits},
};

/// Resolved mount, network and resource boundaries for one command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionPolicy {
    pub read_only_workspace: bool,
    pub network: NetworkPolicy,
    pub mounts: Vec<MountSpec>,
    pub external_read: Vec<PathBuf>,
    pub external_read_write: Vec<PathBuf>,
    pub protected_paths: Vec<PathBuf>,
    pub opaque_runtime_roots: Vec<PathBuf>,
    pub resources: ResourceLimits,
    pub rust_toolchain: Option<PathBuf>,
    pub env_overrides: BTreeMap<String, String>,
    pub unmask_git: bool,
    pub allow_git_config_write: bool,
    pub read_templates: Vec<PathBuf>,
    pub write_templates: Vec<PathBuf>,
}

impl ExecutionPolicy {
    /// Read-write workspace policy; external writes still require grants.
    pub fn dev_default() -> Self {
        Self {
            read_only_workspace: false,
            network: NetworkPolicy::Deny,
            mounts: Vec::new(),
            external_read: Vec::new(),
            external_read_write: Vec::new(),
            protected_paths: Vec::new(),
            opaque_runtime_roots: Vec::new(),
            resources: ResourceLimits::default(),
            rust_toolchain: None,
            env_overrides: BTreeMap::new(),
            unmask_git: false,
            allow_git_config_write: false,
            read_templates: Vec::new(),
            write_templates: Vec::new(),
        }
    }

    /// Read-only workspace policy.
    pub fn read_only() -> Self {
        Self {
            read_only_workspace: true,
            network: NetworkPolicy::Deny,
            mounts: Vec::new(),
            external_read: Vec::new(),
            external_read_write: Vec::new(),
            protected_paths: Vec::new(),
            opaque_runtime_roots: Vec::new(),
            resources: ResourceLimits::default(),
            rust_toolchain: None,
            env_overrides: BTreeMap::new(),
            unmask_git: false,
            allow_git_config_write: false,
            read_templates: Vec::new(),
            write_templates: Vec::new(),
        }
    }

    pub fn with_unmask_git(mut self, unmask_git: bool) -> Self {
        self.unmask_git = unmask_git;
        self
    }

    pub fn with_read_write(mut self, rw: bool) -> Self {
        self.read_only_workspace = !rw;
        self
    }

    pub fn with_network(mut self, network: NetworkPolicy) -> Self {
        self.network = network;
        self
    }

    pub fn with_mounts(mut self, mounts: Vec<MountSpec>) -> Self {
        self.mounts = mounts;
        self
    }

    pub fn with_external_read(mut self, paths: Vec<PathBuf>) -> Self {
        self.external_read = paths;
        self
    }

    pub fn with_external_read_write(mut self, paths: Vec<PathBuf>) -> Self {
        self.external_read_write = paths;
        self
    }

    pub fn with_protected_paths(mut self, paths: Vec<PathBuf>) -> Self {
        self.protected_paths = paths;
        self
    }

    pub fn with_opaque_runtime_roots(mut self, roots: Vec<PathBuf>) -> Self {
        self.opaque_runtime_roots = roots;
        self
    }

    pub fn with_resources(mut self, resources: ResourceLimits) -> Self {
        self.resources = resources;
        self
    }

    pub fn with_rust_toolchain(mut self, toolchain: Option<PathBuf>) -> Self {
        self.rust_toolchain = toolchain;
        self
    }

    pub fn with_env_overrides(mut self, env: BTreeMap<String, String>) -> Self {
        self.env_overrides = env;
        self
    }
}

impl Default for ExecutionPolicy {
    fn default() -> Self {
        Self::dev_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_policy_defaults() {
        let dev = ExecutionPolicy::dev_default();
        assert!(!dev.read_only_workspace);
        assert_eq!(dev.network, NetworkPolicy::Deny);

        let ro = ExecutionPolicy::read_only();
        assert!(ro.read_only_workspace);
    }

    #[test]
    fn execution_policy_builders() {
        let mut env = BTreeMap::new();
        env.insert("CARGO_INCREMENTAL".to_string(), "0".to_string());

        let policy = ExecutionPolicy::dev_default()
            .with_network(NetworkPolicy::Allow)
            .with_read_write(false)
            .with_protected_paths(vec![PathBuf::from("/etc/secret")])
            .with_opaque_runtime_roots(vec![PathBuf::from("/var/cache")])
            .with_external_read(vec![PathBuf::from("/usr/include")])
            .with_external_read_write(vec![PathBuf::from("/var/log")])
            .with_env_overrides(env.clone());

        assert!(policy.read_only_workspace);
        assert_eq!(policy.network, NetworkPolicy::Allow);
        assert_eq!(policy.protected_paths, vec![PathBuf::from("/etc/secret")]);
        assert_eq!(
            policy.opaque_runtime_roots,
            vec![PathBuf::from("/var/cache")]
        );
        assert_eq!(policy.external_read, vec![PathBuf::from("/usr/include")]);
        assert_eq!(policy.external_read_write, vec![PathBuf::from("/var/log")]);
        assert_eq!(policy.env_overrides, env);
    }
}
