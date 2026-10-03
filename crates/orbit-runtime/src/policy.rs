use std::{
    path::PathBuf,
    sync::{Arc, RwLock},
};

use serde::{Deserialize, Serialize};

use crate::{
    capability::{EnvironmentId, FilesystemAccess, FilesystemCapability, NetworkPolicy},
    error::{RuntimeError, failed},
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
    /// Exact environment template paths explicitly trusted by the user.
    #[serde(default)]
    pub read_templates: Vec<PathBuf>,
    #[serde(default)]
    pub write_templates: Vec<PathBuf>,
    #[serde(default)]
    pub read: Vec<PathBuf>,
    #[serde(default)]
    pub read_write: Vec<PathBuf>,
}

/// Combined effective policy calculated from BasePolicy + Persistent Workspace Permissions + Session Grants + Protected Paths.
#[derive(Clone, Debug)]
pub struct EffectivePolicy {
    pub base: BasePolicy,
    pub read_templates: Vec<PathBuf>,
    pub write_templates: Vec<PathBuf>,
    pub persistent_read: Vec<ResolvedPath>,
    pub persistent_read_write: Vec<ResolvedPath>,
    pub session_grants: Arc<RwLock<SessionGrantStore>>,
    pub protected_paths: Arc<RwLock<Vec<PathBuf>>>,
    pub opaque_runtime_roots: Arc<RwLock<Vec<PathBuf>>>,
}

impl EffectivePolicy {
    pub fn new(base: BasePolicy) -> Self {
        Self {
            base,
            read_templates: Vec::new(),
            write_templates: Vec::new(),
            persistent_read: Vec::new(),
            persistent_read_write: Vec::new(),
            session_grants: Arc::new(RwLock::new(SessionGrantStore::new())),
            protected_paths: Arc::new(RwLock::new(Vec::new())),
            opaque_runtime_roots: Arc::new(RwLock::new(Vec::new())),
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
#[derive(Clone, Debug)]
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

    pub fn try_with_permissions(
        workspace: ResolvedPath,
        permissions: WorkspacePermissions,
        network: NetworkPolicy,
    ) -> Result<Self, RuntimeError> {
        let base_dir = workspace.canonical();

        let validate_path = |p: &std::path::Path,
                             resolved: &ResolvedPath|
         -> Result<(), RuntimeError> {
            let canon = resolved.canonical();

            if !canon.exists() {
                return Err(failed(format!(
                    "persistent permission path '{}' does not exist",
                    p.display()
                )));
            }
            if crate::execution::process::is_reserved_system_path(canon) {
                return Err(failed(format!(
                    "persistent permission path '{}' is a reserved system path",
                    p.display()
                )));
            }
            if base_dir.starts_with(canon) {
                return Err(failed(format!(
                    "persistent permission path '{}' is an ancestor of the workspace",
                    p.display()
                )));
            }
            if canon.starts_with(base_dir) {
                return Err(failed(format!(
                    "persistent permission path '{}' is inside the workspace; persistent permissions are for external paths",
                    p.display()
                )));
            }

            Ok(())
        };

        let validate_templates = |paths: Vec<PathBuf>| -> Result<Vec<PathBuf>, RuntimeError> {
            paths.into_iter().map(|path| {
                let candidate = if path.is_absolute() { path } else { base_dir.join(path) };
                if std::fs::symlink_metadata(&candidate).map_err(failed)?.file_type().is_symlink() { return Err(failed("template authorization requires an ordinary file, not a symlink")); }
                let canonical = std::fs::canonicalize(&candidate).map_err(failed)?;
                let rel = canonical.strip_prefix(base_dir).map_err(|_| failed("template must be inside workspace"))?;
                let name = canonical.file_name().and_then(|s| s.to_str()).unwrap_or("");
                if !name.starts_with(".env.") || !canonical.is_file() || rel.parent().is_some_and(|p| p.components().any(|c| crate::filesystem::path_security::is_protected_workspace_name(&c.as_os_str().to_string_lossy()))) {
                    return Err(failed("template authorization requires an exact ordinary .env.* file inside workspace"));
                }
                Ok(canonical)
            }).collect()
        };
        let read_templates = validate_templates(permissions.read_templates)?;
        let write_templates = validate_templates(permissions.write_templates)?;

        let mut persistent_read = Vec::new();

        for p in permissions.read {
            let resolved = ResolvedPath::resolve(&p, Some(base_dir)).map_err(|e| {
                failed(format!(
                    "invalid persistent read path '{}': {e}",
                    p.display()
                ))
            })?;
            validate_path(&p, &resolved)?;
            persistent_read.push(resolved);
        }

        let mut persistent_read_write = Vec::new();

        for p in permissions.read_write {
            let resolved = ResolvedPath::resolve(&p, Some(base_dir)).map_err(|e| {
                failed(format!(
                    "invalid persistent read_write path '{}': {e}",
                    p.display()
                ))
            })?;
            validate_path(&p, &resolved)?;
            persistent_read_write.push(resolved);
        }

        // Normalize persistent_read_write (deduplicate and keep broader ancestor)
        normalize_resolved_paths(&mut persistent_read_write);

        // Normalize persistent_read (deduplicate and keep broader ancestor)
        normalize_resolved_paths(&mut persistent_read);

        // Check overlap between read_write and read
        for rw in &persistent_read_write {
            for ro in &persistent_read {
                let rw_c = rw.canonical();
                let ro_c = ro.canonical();

                if rw_c == ro_c || rw_c.starts_with(ro_c) || ro_c.starts_with(rw_c) {
                    return Err(failed(format!(
                        "persistent permissions conflict: read_write path '{}' overlaps with read path '{}'; read and read_write ranges cannot overlap",
                        rw_c.display(),
                        ro_c.display()
                    )));
                }
            }
        }

        Ok(Self {
            policy: Arc::new(EffectivePolicy {
                base: BasePolicy { workspace, network },
                read_templates,
                write_templates,
                persistent_read,
                persistent_read_write,
                session_grants: Arc::new(RwLock::new(SessionGrantStore::new())),
                protected_paths: Arc::new(RwLock::new(Vec::new())),
                opaque_runtime_roots: Arc::new(RwLock::new(Vec::new())),
            }),
        })
    }

    pub fn with_permissions(
        workspace: ResolvedPath,
        permissions: WorkspacePermissions,
        network: NetworkPolicy,
    ) -> Self {
        Self::try_with_permissions(workspace, permissions, network)
            .expect("invalid persistent workspace permissions")
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

    pub fn add_protected_path(&self, path: PathBuf) {
        let mut guard = self
            .policy
            .protected_paths
            .write()
            .expect("protected_paths lock poisoned");

        if !guard.contains(&path) {
            guard.push(path);
        }
    }

    pub fn protected_paths(&self) -> Vec<PathBuf> {
        self.policy
            .protected_paths
            .read()
            .expect("protected_paths lock poisoned")
            .clone()
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

        if capability.environment.is_local() {
            let canon = resolved.canonical();
            let ws_canon = self.policy.base.workspace.canonical();

            // Reject ancestor directories of the workspace (e.g. workspace is /home/user/project, capability is /home/user)
            if ws_canon.starts_with(canon) && ws_canon != canon {
                return PolicyDecision::Denied(format!(
                    "path '{}' is an ancestor of the workspace and cannot be granted as an external capability",
                    canon.display()
                ));
            }

            // Reject reserved runtime/system paths
            if crate::execution::process::is_reserved_system_path(canon) {
                return PolicyDecision::Denied(format!(
                    "path '{}' is a reserved runtime path and cannot be granted as external capability",
                    canon.display()
                ));
            }

            // Check against protected paths
            let protected_guard = self
                .policy
                .protected_paths
                .read()
                .expect("protected_paths lock poisoned");

            for p in protected_guard.iter() {
                if canon == p || canon.starts_with(p) {
                    return PolicyDecision::Denied(format!(
                        "path '{}' is inside protected path '{}' and cannot be granted",
                        canon.display(),
                        p.display()
                    ));
                }
                if p.starts_with(canon) {
                    return PolicyDecision::Denied(format!(
                        "path '{}' contains protected path '{}' and cannot be granted as capability",
                        canon.display(),
                        p.display()
                    ));
                }
            }

            // Inside workspace check
            if resolved.is_subpath_of(&self.policy.base.workspace) {
                if let Ok(rel) = canon.strip_prefix(ws_canon) {
                    for component in rel.components() {
                        let name = component.as_os_str().to_string_lossy();

                        if crate::filesystem::path_security::is_protected_workspace_name(&name) {
                            let template_read = self
                                .policy
                                .read_templates
                                .iter()
                                .chain(self.policy.write_templates.iter())
                                .any(|p| p == canon);
                            let template_write =
                                self.policy.write_templates.iter().any(|p| p == canon);
                            if template_write
                                || (template_read && !capability.access.allows_write())
                            {
                                continue;
                            }
                            return PolicyDecision::Denied(format!(
                                "path '{}' is protected metadata and cannot be granted",
                                canon.display()
                            ));
                        }
                    }
                }

                return PolicyDecision::Granted;
            }

            // Persistent workspace permissions
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

        // Runtime session grants
        let session_allowed = self
            .policy
            .session_grants
            .read()
            .expect("session grant store lock poisoned")
            .check(&capability.environment, &resolved, capability.access);

        if session_allowed {
            return PolicyDecision::Granted;
        }

        // Otherwise, requires interactive user approval
        PolicyDecision::NeedsApproval(vec![capability.clone()])
    }
}

fn normalize_resolved_paths(paths: &mut Vec<ResolvedPath>) {
    paths.sort_by_key(|p| p.canonical().components().count());
    let mut normalized: Vec<ResolvedPath> = Vec::new();

    for p in paths.drain(..) {
        let is_covered = normalized
            .iter()
            .any(|existing| p.canonical().starts_with(existing.canonical()));

        if !is_covered {
            normalized.push(p);
        }
    }

    *paths = normalized;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

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

    #[test]
    fn rejects_ancestor_of_workspace_as_capability() {
        let tmp = tempfile::tempdir().unwrap();
        let proj = tmp.path().join("proj");
        std::fs::create_dir_all(&proj).unwrap();

        let workspace = ResolvedPath::resolve(&proj, None).unwrap();
        let engine = PolicyEngine::new(workspace);

        // Requesting the parent / ancestor directory of workspace
        let cap = FilesystemCapability::local_read(tmp.path());

        match engine.check_filesystem(&cap) {
            PolicyDecision::Denied(reason) => {
                assert!(reason.contains("ancestor of the workspace"));
            }
            other => panic!("expected Denied, got {other:?}"),
        }
    }

    #[test]
    fn rejects_protected_path_and_its_ancestor_and_descendant() {
        let tmp_ws = tempfile::tempdir().unwrap();
        let tmp_ext = tempfile::tempdir().unwrap();
        let secret = tmp_ext.path().join("nested").join("orbit.db");
        std::fs::create_dir_all(secret.parent().unwrap()).unwrap();
        std::fs::write(&secret, "database").unwrap();

        let workspace = ResolvedPath::resolve(tmp_ws.path(), None).unwrap();
        let engine = PolicyEngine::new(workspace);
        engine.add_protected_path(secret.clone());

        // 1. Requesting the protected file directly is denied
        let cap_direct = FilesystemCapability::local_read(&secret);

        match engine.check_filesystem(&cap_direct) {
            PolicyDecision::Denied(reason) => {
                assert!(reason.contains("inside protected path"));
            }
            other => panic!("expected Denied, got {other:?}"),
        }

        // 2. Requesting the parent directory containing the protected file is denied
        let cap_parent = FilesystemCapability::local_read(tmp_ext.path());

        match engine.check_filesystem(&cap_parent) {
            PolicyDecision::Denied(reason) => {
                assert!(reason.contains("contains protected path"));
            }
            other => panic!("expected Denied, got {other:?}"),
        }
    }

    #[test]
    fn rejects_reserved_runtime_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = ResolvedPath::resolve(tmp.path(), None).unwrap();
        let engine = PolicyEngine::new(workspace);

        for reserved in [
            "/workspace/sub",
            "/scratch/file",
            "/orbit-command.sh",
            "/orbit-source/file",
            "/opt/orbit-rust/bin",
            "/tmp/home/user",
            "/etc/shadow",
            "/proc/cpuinfo",
            "/dev/null",
        ] {
            let cap = FilesystemCapability::local_read(Path::new(reserved));

            assert!(
                matches!(engine.check_filesystem(&cap), PolicyDecision::Denied(_)),
                "expected {reserved} to be denied"
            );
        }
    }

    #[test]
    fn persistent_permissions_upfront_validation_and_overlap_rejection() {
        let tmp_ws = tempfile::tempdir().unwrap();
        let tmp_ext = tempfile::tempdir().unwrap();
        let ext_dir = tmp_ext.path().join("data");
        std::fs::create_dir_all(&ext_dir).unwrap();

        let ws = ResolvedPath::resolve(tmp_ws.path(), None).unwrap();

        // 1. Non-existent path rejected
        let perms_nonexistent = WorkspacePermissions {
            read_templates: Vec::new(),
            write_templates: Vec::new(),
            read: vec![tmp_ext.path().join("nonexistent")],
            read_write: Vec::new(),
        };

        assert!(
            PolicyEngine::try_with_permissions(ws.clone(), perms_nonexistent, NetworkPolicy::Allow)
                .is_err()
        );

        // 2. Workspace ancestor rejected
        let perms_ancestor = WorkspacePermissions {
            read_templates: Vec::new(),
            write_templates: Vec::new(),
            read: vec![tmp_ws.path().parent().unwrap().to_path_buf()],
            read_write: Vec::new(),
        };

        assert!(
            PolicyEngine::try_with_permissions(ws.clone(), perms_ancestor, NetworkPolicy::Allow)
                .is_err()
        );

        // 3. Inside workspace rejected
        let perms_inside = WorkspacePermissions {
            read_templates: Vec::new(),
            write_templates: Vec::new(),
            read: vec![tmp_ws.path().join("src")],
            read_write: Vec::new(),
        };

        assert!(
            PolicyEngine::try_with_permissions(ws.clone(), perms_inside, NetworkPolicy::Allow)
                .is_err()
        );

        // 4. Overlapping read and read_write rejected
        let sub_dir = ext_dir.join("secret");
        std::fs::create_dir_all(&sub_dir).unwrap();
        let perms_overlap = WorkspacePermissions {
            read_templates: Vec::new(),
            write_templates: Vec::new(),
            read_write: vec![ext_dir.clone()],
            read: vec![sub_dir.clone()],
        };
        let err =
            PolicyEngine::try_with_permissions(ws.clone(), perms_overlap, NetworkPolicy::Allow)
                .unwrap_err()
                .to_string();

        assert!(err.contains("overlaps with read path"));

        // 5. Redundant paths normalized
        let sub_rw = ext_dir.join("sub_rw");
        std::fs::create_dir_all(&sub_rw).unwrap();
        let perms_redundant = WorkspacePermissions {
            read_templates: Vec::new(),
            write_templates: Vec::new(),
            read_write: vec![ext_dir.clone(), sub_rw],
            read: Vec::new(),
        };
        let engine =
            PolicyEngine::try_with_permissions(ws, perms_redundant, NetworkPolicy::Allow).unwrap();

        assert_eq!(engine.effective_policy().persistent_read_write.len(), 1);
    }
}
