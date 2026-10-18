use std::os::unix::fs::MetadataExt;
use std::{
    fs::File,
    io,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use rustix::fs::{Mode, OFlags, openat};
use thiserror::Error;

use crate::{
    capability::NetworkPolicy,
    filesystem::path_security::ResolvedPath,
    policy::{PolicyEngine, WorkspacePermissions},
};

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("workspace root must be a directory")]
    NotDirectory,
    #[error("use a workspace-relative path without '..'")]
    InvalidPath,
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("only regular files can be read")]
    NotFile,
    #[error("cannot open workspace file (symlinks are not followed): {0}")]
    Io(#[from] io::Error),
}

fn open_canonical_parent_stepwise(canonical_parent: &Path) -> Result<File, WorkspaceError> {
    if !canonical_parent.is_absolute() {
        return Err(WorkspaceError::InvalidPath);
    }

    let mut current = File::open("/")?;

    for component in canonical_parent.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                let next_fd = openat(
                    &current,
                    name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(|e| {
                    if io::Error::from(e).kind() == io::ErrorKind::NotADirectory {
                        WorkspaceError::NotDirectory
                    } else {
                        WorkspaceError::Io(e.into())
                    }
                })?;

                current = File::from(next_fd);
            }

            _ => return Err(WorkspaceError::InvalidPath),
        }
    }

    Ok(current)
}

#[derive(Clone)]
pub struct Workspace {
    root: Arc<File>,
    path: PathBuf,
    policy: PolicyEngine,
}

impl Workspace {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, WorkspaceError> {
        let path = root.as_ref().canonicalize()?;
        let root = File::open(&path)?;

        if !root.metadata()?.is_dir() {
            return Err(WorkspaceError::NotDirectory);
        }

        let resolved = ResolvedPath::resolve(&path, None).map_err(WorkspaceError::Io)?;
        let policy = PolicyEngine::new(resolved);

        Ok(Self {
            root: Arc::new(root),
            path,
            policy,
        })
    }

    pub fn root(&self) -> &Path {
        &self.path
    }

    pub fn policy(&self) -> &PolicyEngine {
        &self.policy
    }

    pub fn clear_session_grants(&self) {
        self.policy.clear_session_grants();
    }

    pub fn set_policy(&mut self, policy: PolicyEngine) {
        self.policy = policy;
    }

    pub fn configure_policy(
        &mut self,
        permissions: WorkspacePermissions,
        network: NetworkPolicy,
    ) -> Result<(), WorkspaceError> {
        let resolved = ResolvedPath::resolve(&self.path, None).map_err(WorkspaceError::Io)?;
        let policy = PolicyEngine::try_with_permissions(resolved, permissions, network)
            .map_err(|e| WorkspaceError::PermissionDenied(e.to_string()))?;

        let mut policy = policy;
        policy.set_approval_policy(self.policy.approval_policy().clone());
        self.policy = policy;

        Ok(())
    }

    pub fn checked_relative(&self, path: &Path) -> Result<PathBuf, WorkspaceError> {
        let mut parts = Vec::new();

        for component in path.components() {
            match component {
                Component::Normal(name) => {
                    parts.push(name);
                }

                Component::CurDir => {}
                _ => return Err(WorkspaceError::InvalidPath),
            }
        }

        if parts.is_empty() {
            return Err(WorkspaceError::InvalidPath);
        }

        let relative = parts.iter().collect::<PathBuf>();

        Ok(relative)
    }

    /// Resolve links through directory descriptors, with no canonicalize/open gap.
    /// Link targets may traverse upward only within the workspace root.
    pub fn write_parent(&self, path: &Path) -> Result<(File, std::ffi::OsString), WorkspaceError> {
        self.resolve_parent(path)
    }

    fn resolve_parent(&self, path: &Path) -> Result<(File, std::ffi::OsString), WorkspaceError> {
        use std::collections::VecDeque;
        use std::os::unix::ffi::OsStringExt;
        let relative = self.checked_relative(path)?;
        let mut pending: VecDeque<std::ffi::OsString> = relative
            .iter()
            .map(|n| n.to_owned())
            .collect();
        let mut stack = vec![self.root.try_clone()?];
        let mut resolved = PathBuf::new();
        let mut links = 0;
        while let Some(name) = pending.pop_front() {
            if name == "." {
                continue;
            }
            if name == ".." {
                if stack.len() == 1 {
                    return Err(WorkspaceError::InvalidPath);
                }
                stack.pop();
                resolved.pop();
                continue;
            }

            let candidate = resolved.join(&name);
            self.checked_relative(&candidate)?;
            let directory = stack.last().unwrap();
            match rustix::fs::readlinkat(directory, &name, Vec::new()) {
                Ok(target) => {
                    links += 1;
                    if links > 32 {
                        return Err(WorkspaceError::Io(io::Error::other(
                            "symlink loop or link depth exceeds 32",
                        )));
                    }

                    let target = PathBuf::from(std::ffi::OsString::from_vec(target.into_bytes()));
                    let target = if target.is_absolute() {
                        let relative_target = target
                            .strip_prefix(&self.path)
                            .map_err(|_| WorkspaceError::InvalidPath)?
                            .to_path_buf();
                        stack.truncate(1);
                        resolved.clear();
                        relative_target
                    } else {
                        target
                    };
                    let mut components: VecDeque<_> = target
                        .components()
                        .filter_map(|c| match c {
                            Component::Normal(n) => Some(n.to_owned()),
                            Component::ParentDir => Some(std::ffi::OsString::from("..")),
                            Component::CurDir => None,
                            _ => Some(std::ffi::OsString::from("..")),
                        })
                        .collect();

                    components.append(&mut pending);
                    pending = components;
                }

                Err(rustix::io::Errno::INVAL | rustix::io::Errno::NOENT) => {
                    if pending.is_empty() {
                        return Ok((directory.try_clone()?, name));
                    }
                    let next = File::from(
                        openat(
                            directory,
                            &name,
                            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                            Mode::empty(),
                        )
                        .map_err(io::Error::from)?,
                    );

                    stack.push(next);
                    resolved.push(name);
                }

                Err(error) => return Err(WorkspaceError::Io(error.into())),
            }
        }

        Ok((
            stack.last().unwrap().try_clone()?,
            std::ffi::OsString::from("."),
        ))
    }

    pub fn open_directory(&self, path: &Path) -> Result<File, WorkspaceError> {
        let directory = if path.as_os_str().is_empty() {
            self.root.try_clone()?
        } else {
            let (parent, name) = self.resolve_parent(path)?;

            File::from(
                openat(
                    &parent,
                    name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map_err(|e| {
                    if io::Error::from(e).kind() == io::ErrorKind::NotADirectory {
                        WorkspaceError::NotDirectory
                    } else {
                        WorkspaceError::Io(e.into())
                    }
                })?,
            )
        };

        Ok(directory)
    }

    pub fn authorize_write_parent(
        &self,
        path: &Path,
    ) -> Result<(File, std::ffi::OsString), WorkspaceError> {
        match self.write_parent(path) {
            Ok(res) => Ok(res),
            Err(WorkspaceError::NotFile) => Err(WorkspaceError::NotFile),
            Err(WorkspaceError::NotDirectory) => Err(WorkspaceError::NotDirectory),
            Err(WorkspaceError::Io(err)) => Err(WorkspaceError::Io(err)),
            Err(WorkspaceError::PermissionDenied(err)) => {
                Err(WorkspaceError::PermissionDenied(err))
            }

            Err(WorkspaceError::InvalidPath) => {
                let resolved =
                    ResolvedPath::resolve(path, Some(&self.path)).map_err(WorkspaceError::Io)?;
                let canonical = resolved.canonical();

                let cap = crate::capability::FilesystemCapability {
                    environment: crate::capability::EnvironmentId::Local,
                    path: canonical.to_path_buf(),
                    access: crate::capability::FilesystemAccess::ReadWrite,
                };

                match self.policy.check_filesystem(&cap) {
                    crate::policy::PolicyDecision::Granted => {}
                    crate::policy::PolicyDecision::NeedsApproval(_) => {
                        return Err(WorkspaceError::PermissionDenied(format!(
                            "write permission denied: '{}' is outside the workspace. Call \
                                RequestAccess to request access.",
                            path.display()
                        )));
                    }

                    crate::policy::PolicyDecision::Denied(reason) => {
                        return Err(WorkspaceError::PermissionDenied(reason));
                    }
                }

                let parent_path = canonical
                    .parent()
                    .ok_or(WorkspaceError::InvalidPath)?;
                let file_name = canonical
                    .file_name()
                    .ok_or(WorkspaceError::InvalidPath)?
                    .to_os_string();

                let parent_file = open_canonical_parent_stepwise(parent_path)?;
                let parent_meta = parent_file.metadata()?;

                if !parent_meta.is_dir() {
                    return Err(WorkspaceError::NotDirectory);
                }

                Ok((parent_file, file_name))
            }
        }
    }

    pub fn authorize_directory(&self, path: &Path) -> Result<File, WorkspaceError> {
        match self.open_directory(path) {
            Ok(file) => Ok(file),
            Err(WorkspaceError::NotFile) => Err(WorkspaceError::NotFile),
            Err(WorkspaceError::NotDirectory) => Err(WorkspaceError::NotDirectory),
            Err(WorkspaceError::Io(err)) => Err(WorkspaceError::Io(err)),
            Err(WorkspaceError::PermissionDenied(err)) => {
                Err(WorkspaceError::PermissionDenied(err))
            }

            Err(WorkspaceError::InvalidPath) => {
                let resolved =
                    ResolvedPath::resolve(path, Some(&self.path)).map_err(WorkspaceError::Io)?;
                let canonical = resolved.canonical().to_path_buf();

                if !canonical.metadata()?.is_dir() {
                    return Err(WorkspaceError::NotDirectory);
                }

                let cap = crate::capability::FilesystemCapability {
                    environment: crate::capability::EnvironmentId::Local,
                    path: canonical.clone(),
                    access: crate::capability::FilesystemAccess::Read,
                };

                match self.policy.check_filesystem(&cap) {
                    crate::policy::PolicyDecision::Granted => {}
                    crate::policy::PolicyDecision::NeedsApproval(_) => {
                        return Err(WorkspaceError::PermissionDenied(format!(
                            "directory read permission denied: '{}' is outside the workspace. Call \
                                RequestAccess to request access.",
                            path.display()
                        )));
                    }

                    crate::policy::PolicyDecision::Denied(reason) => {
                        return Err(WorkspaceError::PermissionDenied(reason));
                    }
                }

                let dir = open_canonical_parent_stepwise(&canonical)?;
                let meta = dir.metadata()?;

                if !meta.is_dir() {
                    return Err(WorkspaceError::NotDirectory);
                }

                Ok(dir)
            }
        }
    }

    pub fn authorize_directory_entries(
        &self,
        path: &Path,
    ) -> Result<Vec<std::ffi::OsString>, WorkspaceError> {
        let directory = self.authorize_directory(path)?;
        Self::read_directory_entries(&directory)
    }

    pub fn open_granted_write_parent(
        &self,
        path: &Path,
    ) -> Result<(File, std::ffi::OsString), WorkspaceError> {
        self.authorize_write_parent(path)
    }

    pub fn open_granted_directory(&self, path: &Path) -> Result<File, WorkspaceError> {
        self.authorize_directory(path)
    }

    pub fn open_granted_directory_entries(
        &self,
        path: &Path,
    ) -> Result<Vec<std::ffi::OsString>, WorkspaceError> {
        self.authorize_directory_entries(path)
    }

    pub fn read_directory_entries(
        directory: &File,
    ) -> Result<Vec<std::ffi::OsString>, WorkspaceError> {
        use std::os::unix::ffi::OsStrExt;

        let entries = rustix::fs::Dir::read_from(directory).map_err(io::Error::from)?;
        let mut names = Vec::new();

        for entry in entries {
            let entry = entry.map_err(io::Error::from)?;
            let name = std::ffi::OsStr::from_bytes(entry.file_name().to_bytes());

            if name != "." && name != ".." {
                names.push(name.to_owned());
            }

            if names.len() > 10_000 {
                return Err(io::Error::other("directory exceeds 10000 entries").into());
            }
        }

        names.sort();

        Ok(names)
    }

    pub fn directory_entries(
        &self,
        path: &Path,
    ) -> Result<Vec<std::ffi::OsString>, WorkspaceError> {
        let directory = self.open_directory(path)?;
        Self::read_directory_entries(&directory)
    }

    pub fn open_read(&self, path: &Path) -> Result<File, WorkspaceError> {
        let (parent, name) = self.resolve_parent(path)?;
        let file = File::from(
            openat(
                &parent,
                &name,
                OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
                Mode::empty(),
            )
            .map_err(io::Error::from)?,
        );
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(WorkspaceError::NotFile);
        }
        Ok(file)
    }

    pub fn authorize_read(&self, path: &Path) -> Result<File, WorkspaceError> {
        match self.open_read(path) {
            Ok(file) => Ok(file),
            Err(WorkspaceError::NotFile) => Err(WorkspaceError::NotFile),
            Err(WorkspaceError::NotDirectory) => Err(WorkspaceError::NotDirectory),
            Err(WorkspaceError::Io(err)) => Err(WorkspaceError::Io(err)),
            Err(WorkspaceError::PermissionDenied(err)) => {
                Err(WorkspaceError::PermissionDenied(err))
            }

            Err(WorkspaceError::InvalidPath) => {
                let resolved =
                    ResolvedPath::resolve(path, Some(&self.path)).map_err(WorkspaceError::Io)?;
                let canonical = resolved.canonical().to_path_buf();

                let cap = crate::capability::FilesystemCapability {
                    environment: crate::capability::EnvironmentId::Local,
                    path: canonical.clone(),
                    access: crate::capability::FilesystemAccess::Read,
                };

                match self.policy.check_filesystem(&cap) {
                    crate::policy::PolicyDecision::Granted => {}
                    crate::policy::PolicyDecision::NeedsApproval(_) => {
                        return Err(WorkspaceError::PermissionDenied(format!(
                            "read permission denied: '{}' is outside the workspace. Call \
                                RequestAccess to request access.",
                            path.display()
                        )));
                    }

                    crate::policy::PolicyDecision::Denied(reason) => {
                        return Err(WorkspaceError::PermissionDenied(reason));
                    }
                }

                let parent_path = canonical
                    .parent()
                    .ok_or(WorkspaceError::InvalidPath)?;
                let file_name = canonical
                    .file_name()
                    .ok_or(WorkspaceError::InvalidPath)?;

                let parent_file = open_canonical_parent_stepwise(parent_path)?;
                let parent_meta = parent_file.metadata()?;

                if !parent_meta.is_dir() {
                    return Err(WorkspaceError::NotDirectory);
                }

                let fd = openat(
                    &parent_file,
                    file_name,
                    OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
                    Mode::empty(),
                )
                .map_err(io::Error::from)?;
                let file = File::from(fd);
                let metadata = file.metadata()?;

                if !metadata.is_file() {
                    return Err(WorkspaceError::NotFile);
                }

                let canon_meta = canonical.metadata()?;

                if metadata.dev() != canon_meta.dev() || metadata.ino() != canon_meta.ino() {
                    return Err(WorkspaceError::Io(io::Error::other(
                        "file inode changed during open",
                    )));
                }

                Ok(file)
            }
        }
    }

    pub fn open_granted_read(&self, path: &Path) -> Result<File, WorkspaceError> {
        self.authorize_read(path)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn yolo_reads_and_writes_external_paths_without_recording_grants() {
        use crate::policy::ApprovalMode;

        let root = tempdir().unwrap();
        let project = root.path().join("project");
        std::fs::create_dir(&project).unwrap();
        std::fs::create_dir(project.join(".git")).unwrap();
        for name in [".env", ".git/config", "config.toml"] {
            std::fs::write(project.join(name), "synthetic content").unwrap();
        }
        let external = root.path().join("external.txt");
        std::fs::write(&external, "outside").unwrap();
        let workspace = Workspace::new(&project).unwrap();
        let clone = workspace.clone();
        assert!(
            workspace
                .authorize_read(&external)
                .is_err()
        );
        for name in [".env", ".git/config", "config.toml"] {
            assert!(
                workspace
                    .open_read(Path::new(name))
                    .is_ok()
            );
            assert!(
                workspace
                    .authorize_write_parent(Path::new(name))
                    .is_ok()
            );
        }

        let policy = workspace.policy().approval_policy();
        policy.set_mode(ApprovalMode::Yolo);
        assert!(clone.authorize_read(&external).is_ok());
        assert!(
            clone
                .authorize_write_parent(&root.path().join("new.txt"))
                .is_ok()
        );
        assert!(
            clone
                .authorize_directory(root.path())
                .is_ok()
        );
        assert!(
            clone
                .policy()
                .session_grants()
                .read()
                .unwrap()
                .grants()
                .is_empty()
        );
        policy.set_mode(ApprovalMode::Ask);
        assert!(clone.authorize_read(&external).is_err());
        assert!(
            clone
                .authorize_write_parent(&root.path().join("new.txt"))
                .is_err()
        );
    }

    #[test]
    fn session_grants_are_ephemeral_and_clearable() {
        use crate::{
            capability::{EnvironmentId, FilesystemCapability},
            policy::{FilesystemGrant, PolicyDecision},
        };
        let ws_temp = tempfile::tempdir().unwrap();
        let ext_temp = tempfile::tempdir().unwrap();
        let persistent_temp = tempfile::tempdir().unwrap();

        let mut workspace = Workspace::new(ws_temp.path()).unwrap();

        let permissions = WorkspacePermissions {
            read_templates: Vec::new(),
            write_templates: Vec::new(),
            read: vec![persistent_temp.path().to_path_buf()],
            read_write: vec![],
        };

        workspace
            .configure_policy(permissions, NetworkPolicy::Allow)
            .unwrap();

        let external_file = ext_temp.path().join("data.csv");
        std::fs::write(&external_file, "1,2,3\n").unwrap();
        let cap = FilesystemCapability::local_read(&external_file);

        assert!(matches!(
            workspace
                .policy()
                .check_filesystem(&cap),
            PolicyDecision::NeedsApproval(_)
        ));

        let resolved_ext = ResolvedPath::resolve(&external_file, None).unwrap();
        workspace
            .policy()
            .record_grant(FilesystemGrant::read_only(
                EnvironmentId::Local,
                resolved_ext,
            ));

        assert_eq!(
            workspace
                .policy()
                .check_filesystem(&cap),
            PolicyDecision::Granted
        );

        workspace
            .policy()
            .clear_session_grants();

        assert!(matches!(
            workspace
                .policy()
                .check_filesystem(&cap),
            PolicyDecision::NeedsApproval(_)
        ));

        let persistent_file = persistent_temp.path().join("model.bin");
        std::fs::write(&persistent_file, "weights").unwrap();
        let persistent_cap = FilesystemCapability::local_read(&persistent_file);

        assert_eq!(
            workspace
                .policy()
                .check_filesystem(&persistent_cap),
            PolicyDecision::Granted
        );

        let ws_cap = FilesystemCapability::local_read_write(ws_temp.path().join("src/lib.rs"));

        assert_eq!(
            workspace
                .policy()
                .check_filesystem(&ws_cap),
            PolicyDecision::Granted
        );

        let fresh_workspace = Workspace::new(ws_temp.path()).unwrap();

        assert!(matches!(
            fresh_workspace
                .policy()
                .check_filesystem(&cap),
            PolicyDecision::NeedsApproval(_)
        ));
        assert_eq!(
            fresh_workspace
                .policy()
                .session_grants()
                .read()
                .unwrap()
                .grants()
                .len(),
            0
        );
    }
}
