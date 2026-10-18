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

    pub fn merge(&mut self, other: &Self) {
        self.read |= other.read || other.write;
        self.write |= other.write;

        if self.write {
            self.read = true;
        }
    }
}

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

    /// Merge rights for the same path and remove redundant child grants.
    pub fn grant(&mut self, new_grant: FilesystemGrant) {
        let grant_to_check = if let Some(existing) = self.filesystem.iter_mut().find(|g| {
            g.environment == new_grant.environment
                && g.path.canonical() == new_grant.path.canonical()
        }) {
            existing.merge(&new_grant);
            existing.clone()
        } else {
            let is_redundant = self.filesystem.iter().any(|existing| {
                existing.environment == new_grant.environment
                    && new_grant
                        .path
                        .is_subpath_of(&existing.path)
                    && (!new_grant.read || existing.read)
                    && (!new_grant.write || existing.write)
            });

            if is_redundant {
                return;
            }

            self.filesystem.push(new_grant.clone());

            new_grant
        };

        self.filesystem.retain(|existing| {
            if existing.environment != grant_to_check.environment
                || existing.path.canonical() == grant_to_check.path.canonical()
            {
                return true;
            }

            let is_child_redundant = existing
                .path
                .is_subpath_of(&grant_to_check.path)
                && (!existing.read || grant_to_check.read)
                && (!existing.write || grant_to_check.write);

            !is_child_redundant
        });
    }

    pub fn clear(&mut self) {
        self.filesystem.clear();
    }

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

/// Shared session approval mode observed by tools and filesystem access.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ApprovalMode {
    #[default]
    Ask,
    Auto,
    Yolo,
}

/// Clones observe mode changes without rebuilding a running tool registry.
#[derive(Clone, Debug, Default)]
pub struct ApprovalPolicy(Arc<RwLock<ApprovalMode>>);

impl ApprovalPolicy {
    pub fn mode(&self) -> ApprovalMode {
        *self
            .0
            .read()
            .expect("approval policy lock poisoned")
    }

    pub fn set_mode(&self, mode: ApprovalMode) {
        *self
            .0
            .write()
            .expect("approval policy lock poisoned") = mode;
    }
}

/// Default workspace read/write and network access; external paths require grants.
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

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct WorkspacePermissions {
    /// Legacy alias for explicitly granted read paths.
    #[serde(default)]
    pub read_templates: Vec<PathBuf>,
    /// Legacy alias for explicitly granted read/write paths.
    #[serde(default)]
    pub write_templates: Vec<PathBuf>,
    #[serde(default)]
    pub read: Vec<PathBuf>,
    #[serde(default)]
    pub read_write: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct EffectivePolicy {
    pub base: BasePolicy,
    pub persistent_read: Vec<ResolvedPath>,
    pub persistent_read_write: Vec<ResolvedPath>,
    pub session_grants: Arc<RwLock<SessionGrantStore>>,
    pub approval: ApprovalPolicy,
}

impl EffectivePolicy {
    pub fn new(base: BasePolicy) -> Self {
        Self {
            base,
            persistent_read: Vec::new(),
            persistent_read_write: Vec::new(),
            session_grants: Arc::new(RwLock::new(SessionGrantStore::new())),
            approval: ApprovalPolicy::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PolicyDecision {
    Granted,
    NeedsApproval(Vec<FilesystemCapability>),
    Denied(String),
}

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

        let validate_path =
            |p: &std::path::Path, resolved: &ResolvedPath| -> Result<(), RuntimeError> {
                let canon = resolved.canonical();

                if !canon.exists() {
                    return Err(failed(format!(
                        "persistent permission path '{}' does not exist",
                        p.display()
                    )));
                }

                Ok(())
            };

        let mut persistent_read = Vec::new();

        for p in permissions
            .read
            .into_iter()
            .chain(permissions.read_templates)
        {
            let resolved = ResolvedPath::resolve(&p, Some(base_dir)).map_err(|e| {
                failed(format!(
                    "invalid persistent read path '{}': {e}",
                    p.display()
                ))
            })?;

            validate_path(&p, &resolved)?;
            if !resolved.is_subpath_of(&workspace) {
                persistent_read.push(resolved);
            }
        }

        let mut persistent_read_write = Vec::new();

        for p in permissions
            .read_write
            .into_iter()
            .chain(permissions.write_templates)
        {
            let resolved = ResolvedPath::resolve(&p, Some(base_dir)).map_err(|e| {
                failed(format!(
                    "invalid persistent read_write path '{}': {e}",
                    p.display()
                ))
            })?;

            validate_path(&p, &resolved)?;
            if !resolved.is_subpath_of(&workspace) {
                persistent_read_write.push(resolved);
            }
        }

        normalize_resolved_paths(&mut persistent_read_write);

        normalize_resolved_paths(&mut persistent_read);

        for rw in &persistent_read_write {
            for ro in &persistent_read {
                let rw_c = rw.canonical();
                let ro_c = ro.canonical();

                if rw_c == ro_c || rw_c.starts_with(ro_c) || ro_c.starts_with(rw_c) {
                    return Err(failed(format!(
                        "persistent permissions conflict: read_write path '{}' overlaps with read \
                            path '{}'; read and read_write ranges cannot overlap",
                        rw_c.display(),
                        ro_c.display()
                    )));
                }
            }
        }

        Ok(Self {
            policy: Arc::new(EffectivePolicy {
                base: BasePolicy { workspace, network },
                persistent_read,
                persistent_read_write,
                session_grants: Arc::new(RwLock::new(SessionGrantStore::new())),
                approval: ApprovalPolicy::default(),
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

    pub fn approval_policy(&self) -> &ApprovalPolicy {
        &self.policy.approval
    }

    pub fn set_approval_policy(&mut self, approval: ApprovalPolicy) {
        Arc::make_mut(&mut self.policy).approval = approval;
    }

    pub fn record_grant(&self, grant: FilesystemGrant) {
        self.policy
            .session_grants
            .write()
            .expect("session grant store lock poisoned")
            .grant(grant);
    }

    pub fn clear_session_grants(&self) {
        self.policy
            .session_grants
            .write()
            .expect("session grant store lock poisoned")
            .clear();
    }

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
            if self.policy.approval.mode() == ApprovalMode::Yolo
                || resolved.is_subpath_of(&self.policy.base.workspace)
            {
                return PolicyDecision::Granted;
            }

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

        let session_allowed = self
            .policy
            .session_grants
            .read()
            .expect("session grant store lock poisoned")
            .check(&capability.environment, &resolved, capability.access);

        if session_allowed {
            return PolicyDecision::Granted;
        }

        PolicyDecision::NeedsApproval(vec![capability.clone()])
    }
}

fn normalize_resolved_paths(paths: &mut Vec<ResolvedPath>) {
    paths.sort_by_key(|p| p.canonical().components().count());
    let mut normalized: Vec<ResolvedPath> = Vec::new();

    for p in paths.drain(..) {
        let is_covered = normalized.iter().any(|existing| {
            p.canonical()
                .starts_with(existing.canonical())
        });

        if !is_covered {
            normalized.push(p);
        }
    }

    *paths = normalized;
}

#[cfg(test)]
mod tests;
