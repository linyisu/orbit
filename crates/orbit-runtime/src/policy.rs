use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
};

use serde::{Deserialize, Serialize};

use crate::{
    capability::{EnvironmentId, FilesystemAccess, FilesystemCapability, NetworkPolicy},
    filesystem::ResolvedPath,
};

/// A granted filesystem permission for a specific resolved path in an environment.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FilesystemGrant {
    pub environment: EnvironmentId,
    pub path: ResolvedPath,
    pub read: bool,
    pub write: bool,
}

impl FilesystemGrant {
    pub fn new(environment: EnvironmentId, path: ResolvedPath, read: bool, write: bool) -> Self {
        Self {
            environment,
            path,
            read: read || write,
            write,
        }
    }

    pub fn read_only(environment: EnvironmentId, path: ResolvedPath) -> Self {
        Self::new(environment, path, true, false)
    }

    pub fn read_write(environment: EnvironmentId, path: ResolvedPath) -> Self {
        Self::new(environment, path, true, true)
    }

    /// Merge another grant into this grant, combining access rights (R + W -> RW).
    pub fn merge(&mut self, other: &Self) {
        self.read |= other.read || other.write;
        self.write |= other.write;
        if self.write {
            self.read = true;
        }
    }
}

/// In-memory storage for interactive capability grants valid only for the current Runtime Session.
/// When the runtime session ends, dropping this store invalidates all temporary grants.
#[derive(Clone, Debug, Default)]
pub struct SessionGrantStore {
    filesystem: Vec<FilesystemGrant>,
}

impl SessionGrantStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn grants(&self) -> &[FilesystemGrant] {
        &self.filesystem
    }

    /// Add a grant to the session store, merging permissions for the same path
    /// and normalizing redundant child grants.
    pub fn grant(&mut self, new_grant: FilesystemGrant) {
        // 1. Check if an exact grant for the same environment and path already exists
        let grant_to_check = if let Some(existing) = self.filesystem.iter_mut().find(|g| {
            g.environment == new_grant.environment
                && g.path.canonical() == new_grant.path.canonical()
        }) {
            existing.merge(&new_grant);
            existing.clone()
        } else {
            // 2. If an existing grant already covers this path with equal or greater permissions, skip redundant
            let is_redundant = self.filesystem.iter().any(|existing| {
                existing.environment == new_grant.environment
                    && new_grant.path.is_subpath_of(&existing.path)
                    && (!new_grant.read || existing.read)
                    && (!new_grant.write || existing.write)
            });

            if is_redundant {
                return;
            }

            self.filesystem.push(new_grant.clone());
            new_grant
        };

        // 3. Remove existing subpath grants that are made redundant by the new broader grant
        self.filesystem.retain(|existing| {
            if existing.environment != grant_to_check.environment
                || existing.path.canonical() == grant_to_check.path.canonical()
            {
                return true;
            }
            let is_child_redundant = existing.path.is_subpath_of(&grant_to_check.path)
                && (!existing.read || grant_to_check.read)
                && (!existing.write || grant_to_check.write);

            !is_child_redundant
        });
    }

    pub fn clear(&mut self) {
        self.filesystem.clear();
    }

    /// Check whether the store grants access to the specified resolved path.
    pub fn check(
        &self,
        environment: &EnvironmentId,
        path: &ResolvedPath,
        access: FilesystemAccess,
    ) -> bool {
        self.filesystem.iter().any(|grant| {
            if &grant.environment != environment {
                return false;
            }

            if !path.is_subpath_of(&grant.path) {
                return false;
            }

            match access {
                FilesystemAccess::Read => grant.read,
                FilesystemAccess::ReadWrite => grant.write,
            }
        })
    }
}

/// Baseline policy: workspace has full read/write, outside is denied by default,
/// and network is allowed by default.
#[derive(Clone, Debug)]
pub struct BasePolicy {
    pub workspace: ResolvedPath,
    pub network: NetworkPolicy,
}

impl BasePolicy {
    pub fn new(workspace: ResolvedPath) -> Self {
        Self {
            workspace,
            network: NetworkPolicy::Allow,
        }
    }
}

/// Persistent workspace permissions loaded from project or user configuration (e.g. orbit.toml).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WorkspacePermissions {
    #[serde(default)]
    pub read: Vec<PathBuf>,
    #[serde(default)]
    pub read_write: Vec<PathBuf>,
}

/// Combined effective policy calculated from BasePolicy + Persistent Workspace Permissions + Session Grants.
#[derive(Clone, Debug)]
pub struct EffectivePolicy {
    pub base: BasePolicy,
    pub persistent_read: Vec<ResolvedPath>,
    pub persistent_read_write: Vec<ResolvedPath>,
    pub session_grants: Arc<RwLock<SessionGrantStore>>,
}

impl EffectivePolicy {
    pub fn new(base: BasePolicy) -> Self {
        Self {
            base,
            persistent_read: Vec::new(),
            persistent_read_write: Vec::new(),
            session_grants: Arc::new(RwLock::new(SessionGrantStore::new())),
        }
    }
}

/// Decision made by the policy engine when a capability is evaluated.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PolicyDecision {
    Granted,
    NeedsApproval(Vec<FilesystemCapability>),
    Denied(String),
}

/// Unified capability decision engine. All tools query permissions through this engine.
#[derive(Clone)]
pub struct PolicyEngine {
    policy: Arc<EffectivePolicy>,
}

impl PolicyEngine {
    pub fn new(workspace: ResolvedPath) -> Self {
        Self::with_permissions(
            workspace,
            WorkspacePermissions::default(),
            NetworkPolicy::Allow,
        )
    }

    pub fn with_permissions(
        workspace: ResolvedPath,
        permissions: WorkspacePermissions,
        network: NetworkPolicy,
    ) -> Self {
        let base_dir = workspace.canonical();

        let persistent_read = permissions
            .read
            .into_iter()
            .filter_map(|p| ResolvedPath::resolve(p, Some(base_dir)).ok())
            .collect();

        let persistent_read_write = permissions
            .read_write
            .into_iter()
            .filter_map(|p| ResolvedPath::resolve(p, Some(base_dir)).ok())
            .collect();

        Self {
            policy: Arc::new(EffectivePolicy {
                base: BasePolicy { workspace, network },
                persistent_read,
                persistent_read_write,
                session_grants: Arc::new(RwLock::new(SessionGrantStore::new())),
            }),
        }
    }

    pub fn workspace(&self) -> &ResolvedPath {
        &self.policy.base.workspace
    }

    pub fn effective_policy(&self) -> &EffectivePolicy {
        &self.policy
    }

    pub fn network_policy(&self) -> NetworkPolicy {
        self.policy.base.network
    }

    pub fn session_grants(&self) -> Arc<RwLock<SessionGrantStore>> {
        self.policy.session_grants.clone()
    }

    /// Record a granted capability into the current session store.
    pub fn record_grant(&self, grant: FilesystemGrant) {
        self.policy
            .session_grants
            .write()
            .expect("session grant store lock poisoned")
            .grant(grant);
    }

    /// Clear all temporary runtime session grants.
    pub fn clear_session_grants(&self) {
        self.policy
            .session_grants
            .write()
            .expect("session grant store lock poisoned")
            .clear();
    }

    /// Check whether a filesystem capability request is allowed under the current effective policy.
    pub fn check_filesystem(&self, capability: &FilesystemCapability) -> PolicyDecision {
        let base_dir = if capability.environment.is_local() {
            Some(self.policy.base.workspace.canonical())
        } else {
            None
        };
        let resolved = match ResolvedPath::resolve(&capability.path, base_dir) {
            Ok(p) => p,
            Err(e) => return PolicyDecision::Denied(format!("invalid path: {e}")),
        };

        // 1. Local workspace is always granted RW by BasePolicy
        if capability.environment.is_local() && resolved.is_subpath_of(&self.policy.base.workspace)
        {
            return PolicyDecision::Granted;
        }

        // 2. Reject reserved runtime/system paths for local environment
        if capability.environment.is_local()
            && crate::execution::process::is_reserved_system_path(resolved.canonical())
        {
            return PolicyDecision::Denied(format!(
                "path '{}' is a reserved runtime path and cannot be granted as external capability",
                resolved.canonical().display()
            ));
        }

        // 2. Persistent workspace permissions
        if capability.environment.is_local() {
            let is_persistent_rw = self
                .policy
                .persistent_read_write
                .iter()
                .any(|rw| resolved.is_subpath_of(rw));

            if is_persistent_rw {
                return PolicyDecision::Granted;
            }

            if !capability.access.allows_write() {
                let is_persistent_r = self
                    .policy
                    .persistent_read
                    .iter()
                    .any(|r| resolved.is_subpath_of(r));

                if is_persistent_r {
                    return PolicyDecision::Granted;
                }
            }
        }

        // 3. Runtime session grants
        let session_allowed = self
            .policy
            .session_grants
            .read()
            .expect("session grant store lock poisoned")
            .check(&capability.environment, &resolved, capability.access);

        if session_allowed {
            return PolicyDecision::Granted;
        }

        // 4. Otherwise, requires interactive user approval
        PolicyDecision::NeedsApproval(vec![capability.clone()])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_paths_are_always_granted() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = ResolvedPath::resolve(tmp.path(), None).unwrap();
        let engine = PolicyEngine::new(workspace);

        let inside_read = FilesystemCapability::local_read(tmp.path().join("src/main.rs"));
        assert_eq!(
            engine.check_filesystem(&inside_read),
            PolicyDecision::Granted
        );

        let inside_write = FilesystemCapability::local_write(tmp.path().join("target/output.bin"));
        assert_eq!(
            engine.check_filesystem(&inside_write),
            PolicyDecision::Granted
        );
    }

    #[test]
    fn outside_paths_require_approval_until_granted() {
        let tmp_ws = tempfile::tempdir().unwrap();
        let tmp_external = tempfile::tempdir().unwrap();

        let workspace = ResolvedPath::resolve(tmp_ws.path(), None).unwrap();
        let external_file = tmp_external.path().join("dataset.csv");
        std::fs::write(&external_file, "data").unwrap();

        let engine = PolicyEngine::new(workspace);
        let cap_read = FilesystemCapability::local_read(&external_file);

        // Before grant: NeedsApproval
        assert!(matches!(
            engine.check_filesystem(&cap_read),
            PolicyDecision::NeedsApproval(_)
        ));

        // Grant read access for this session
        let resolved_external = ResolvedPath::resolve(&external_file, None).unwrap();
        engine.record_grant(FilesystemGrant::read_only(
            EnvironmentId::Local,
            resolved_external,
        ));

        // After grant: Granted
        assert_eq!(engine.check_filesystem(&cap_read), PolicyDecision::Granted);

        // Writing to the same file still requires approval
        let cap_write = FilesystemCapability::local_write(&external_file);
        assert!(matches!(
            engine.check_filesystem(&cap_write),
            PolicyDecision::NeedsApproval(_)
        ));
    }

    #[test]
    fn directory_grant_inherits_to_children_and_merges() {
        let tmp_ws = tempfile::tempdir().unwrap();
        let tmp_models = tempfile::tempdir().unwrap();

        let workspace = ResolvedPath::resolve(tmp_ws.path(), None).unwrap();
        let engine = PolicyEngine::new(workspace);

        let resolved_models = ResolvedPath::resolve(tmp_models.path(), None).unwrap();
        engine.record_grant(FilesystemGrant::read_only(
            EnvironmentId::Local,
            resolved_models.clone(),
        ));

        let child_read =
            FilesystemCapability::local_read(tmp_models.path().join("llama").join("weights.bin"));
        assert_eq!(
            engine.check_filesystem(&child_read),
            PolicyDecision::Granted
        );

        // Child write still needs approval
        let child_write =
            FilesystemCapability::local_write(tmp_models.path().join("llama").join("weights.bin"));
        assert!(matches!(
            engine.check_filesystem(&child_write),
            PolicyDecision::NeedsApproval(_)
        ));

        // Now grant write to parent: R + W -> RW
        engine.record_grant(FilesystemGrant::read_write(
            EnvironmentId::Local,
            resolved_models,
        ));

        // Now child write is granted!
        assert_eq!(
            engine.check_filesystem(&child_write),
            PolicyDecision::Granted
        );
    }

    #[test]
    fn session_store_prunes_redundant_subpaths() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = ResolvedPath::resolve(tmp.path(), None).unwrap();
        let child = ResolvedPath::resolve(tmp.path().join("sub"), None).unwrap();

        let mut store = SessionGrantStore::new();

        // 1. Add child grant R
        store.grant(FilesystemGrant::read_only(
            EnvironmentId::Local,
            child.clone(),
        ));
        assert_eq!(store.grants().len(), 1);

        // 2. Add broader parent grant R -> replaces redundant child
        store.grant(FilesystemGrant::read_only(EnvironmentId::Local, parent));
        assert_eq!(store.grants().len(), 1);

        // 3. Adding child R again is ignored as redundant
        store.grant(FilesystemGrant::read_only(EnvironmentId::Local, child));
        assert_eq!(store.grants().len(), 1);
    }
}
