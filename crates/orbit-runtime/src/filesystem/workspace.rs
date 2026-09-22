use std::{
    fs::File,
    io,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use std::os::unix::fs::MetadataExt;

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
    #[error("access to this configuration or metadata file is blocked")]
    ProtectedPath,
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
    protected: Vec<PathBuf>,
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
            protected: Vec::new(),
            policy,
        })
    }

    pub fn root(&self) -> &Path {
        &self.path
    }

    pub fn protected(&self) -> &[PathBuf] {
        &self.protected
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
        self.policy = policy;
        for p in &self.protected {
            self.policy.add_protected_path(p.clone());
        }
        let effective = self.policy.effective_policy();
        for p in &self.protected {
            for grant in effective
                .persistent_read
                .iter()
                .chain(effective.persistent_read_write.iter())
            {
                let canon = grant.canonical();
                if canon.starts_with(p) || p.starts_with(canon) {
                    return Err(WorkspaceError::PermissionDenied(format!(
                        "persistent permission '{}' conflicts with protected path '{}'",
                        grant.requested().display(),
                        p.display()
                    )));
                }
            }
        }
        Ok(())
    }

    pub fn protect(&mut self, path: impl AsRef<Path>) -> Result<(), WorkspaceError> {
        let path = path.as_ref();
        let resolved = match path.canonicalize() {
            Ok(path) => path,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let parent = path.parent().ok_or(WorkspaceError::InvalidPath)?;
                let parent_canon = if parent.as_os_str().is_empty() {
                    self.path.clone()
                } else if parent.exists() {
                    parent.canonicalize()?
                } else {
                    return Err(WorkspaceError::InvalidPath);
                };
                parent_canon.join(path.file_name().ok_or(WorkspaceError::InvalidPath)?)
            }
            Err(error) => return Err(error.into()),
        };

        let effective = self.policy.effective_policy();
        for grant in effective
            .persistent_read
            .iter()
            .chain(effective.persistent_read_write.iter())
        {
            let canon = grant.canonical();
            if canon.starts_with(&resolved) || resolved.starts_with(canon) {
                return Err(WorkspaceError::PermissionDenied(format!(
                    "protected path '{}' conflicts with persistent permission '{}'",
                    resolved.display(),
                    grant.requested().display()
                )));
            }
        }

        self.protected.push(resolved.clone());
        self.policy.add_protected_path(resolved);
        Ok(())
    }

    pub fn checked_relative(&self, path: &Path) -> Result<PathBuf, WorkspaceError> {
        let mut parts = Vec::new();
        for component in path.components() {
            match component {
                Component::Normal(name) => {
                    let name_text = name.to_string_lossy();
                    if matches!(name_text.as_ref(), ".git" | ".env")
                        || name_text.starts_with(".env.")
                        || name_text.starts_with(".orbit-write-")
                    {
                        return Err(WorkspaceError::ProtectedPath);
                    }
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
        if self
            .protected
            .iter()
            .any(|protected| self.path.join(&relative).starts_with(protected))
        {
            return Err(WorkspaceError::ProtectedPath);
        }

        Ok(relative)
    }

    pub fn write_parent(&self, path: &Path) -> Result<(File, std::ffi::OsString), WorkspaceError> {
        let relative = self.checked_relative(path)?;
        let mut directory = self.root.try_clone()?;
        for name in relative.parent().ok_or(WorkspaceError::InvalidPath)?.iter() {
            directory = File::from(
                openat(
                    &directory,
                    name,
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                    Mode::empty(),
                )
                .map_err(io::Error::from)?,
            );
        }
        Ok((
            directory,
            relative
                .file_name()
                .ok_or(WorkspaceError::InvalidPath)?
                .to_owned(),
        ))
    }

    pub fn open_directory(&self, path: &Path) -> Result<File, WorkspaceError> {
        let directory = if path.as_os_str().is_empty() {
            self.root.try_clone()?
        } else {
            let (parent, name) = self.write_parent(path)?;
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

    fn check_protected_metadata(&self, metadata: &std::fs::Metadata) -> Result<(), WorkspaceError> {
        use std::os::unix::fs::MetadataExt;
        for protected in &self.protected {
            if let Ok(other) = protected.metadata()
                && metadata.dev() == other.dev()
                && metadata.ino() == other.ino()
            {
                return Err(WorkspaceError::ProtectedPath);
            }
        }
        Ok(())
    }

    fn check_protected_path(&self, canonical: &Path) -> Result<(), WorkspaceError> {
        for protected in &self.protected {
            if canonical.starts_with(protected) {
                return Err(WorkspaceError::ProtectedPath);
            }
        }
        Ok(())
    }

    pub fn authorize_write_parent(
        &self,
        path: &Path,
    ) -> Result<(File, std::ffi::OsString), WorkspaceError> {
        match self.write_parent(path) {
            Ok(res) => Ok(res),
            Err(WorkspaceError::ProtectedPath) => Err(WorkspaceError::ProtectedPath),
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
                self.check_protected_path(canonical)?;

                let cap = crate::capability::FilesystemCapability {
                    environment: crate::capability::EnvironmentId::Local,
                    path: canonical.to_path_buf(),
                    access: crate::capability::FilesystemAccess::ReadWrite,
                };
                match self.policy.check_filesystem(&cap) {
                    crate::policy::PolicyDecision::Granted => {}
                    crate::policy::PolicyDecision::NeedsApproval(_) => {
                        return Err(WorkspaceError::PermissionDenied(format!(
                            "write permission denied: '{}' is outside the workspace. Call request_capability to request access.",
                            path.display()
                        )));
                    }
                    crate::policy::PolicyDecision::Denied(reason) => {
                        return Err(WorkspaceError::PermissionDenied(reason));
                    }
                }

                let parent_path = canonical.parent().ok_or(WorkspaceError::InvalidPath)?;
                let file_name = canonical
                    .file_name()
                    .ok_or(WorkspaceError::InvalidPath)?
                    .to_os_string();

                let name_text = file_name.to_string_lossy();
                if matches!(name_text.as_ref(), ".git" | ".env")
                    || name_text.starts_with(".env.")
                    || name_text.starts_with(".orbit-write-")
                {
                    return Err(WorkspaceError::ProtectedPath);
                }

                let parent_file = open_canonical_parent_stepwise(parent_path)?;
                let parent_meta = parent_file.metadata()?;
                if !parent_meta.is_dir() {
                    return Err(WorkspaceError::NotDirectory);
                }
                self.check_protected_metadata(&parent_meta)?;

                Ok((parent_file, file_name))
            }
        }
    }

    pub fn authorize_directory(&self, path: &Path) -> Result<File, WorkspaceError> {
        match self.open_directory(path) {
            Ok(file) => Ok(file),
            Err(WorkspaceError::ProtectedPath) => Err(WorkspaceError::ProtectedPath),
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
                self.check_protected_path(&canonical)?;

                let cap = crate::capability::FilesystemCapability {
                    environment: crate::capability::EnvironmentId::Local,
                    path: canonical.clone(),
                    access: crate::capability::FilesystemAccess::Read,
                };
                match self.policy.check_filesystem(&cap) {
                    crate::policy::PolicyDecision::Granted => {}
                    crate::policy::PolicyDecision::NeedsApproval(_) => {
                        return Err(WorkspaceError::PermissionDenied(format!(
                            "directory read permission denied: '{}' is outside the workspace. Call request_capability to request access.",
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
                self.check_protected_metadata(&meta)?;
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
        let relative = self.checked_relative(path)?;
        let parts: Vec<_> = relative.iter().collect();
        // Each component is opened relative to an already-open directory, never via a
        // canonicalize-then-open path that could follow a swapped symlink.
        let mut directory = self.root.try_clone()?;
        for (index, name) in parts.iter().enumerate() {
            let last = index + 1 == parts.len();
            let mut flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;
            if !last {
                flags |= OFlags::DIRECTORY;
            }
            let fd = openat(&directory, *name, flags, Mode::empty()).map_err(io::Error::from)?;
            let file = File::from(fd);
            if last {
                let metadata = file.metadata()?;
                if !metadata.is_file() {
                    return Err(WorkspaceError::NotFile);
                }
                for protected in &self.protected {
                    if let Ok(other) = protected.metadata() {
                        use std::os::unix::fs::MetadataExt;
                        if metadata.dev() == other.dev() && metadata.ino() == other.ino() {
                            return Err(WorkspaceError::ProtectedPath);
                        }
                    }
                }
                return Ok(file);
            }
            directory = file;
        }
        Err(WorkspaceError::InvalidPath)
    }

    pub fn authorize_read(&self, path: &Path) -> Result<File, WorkspaceError> {
        match self.open_read(path) {
            Ok(file) => Ok(file),
            Err(WorkspaceError::ProtectedPath) => Err(WorkspaceError::ProtectedPath),
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
                self.check_protected_path(&canonical)?;

                let cap = crate::capability::FilesystemCapability {
                    environment: crate::capability::EnvironmentId::Local,
                    path: canonical.clone(),
                    access: crate::capability::FilesystemAccess::Read,
                };
                match self.policy.check_filesystem(&cap) {
                    crate::policy::PolicyDecision::Granted => {}
                    crate::policy::PolicyDecision::NeedsApproval(_) => {
                        return Err(WorkspaceError::PermissionDenied(format!(
                            "read permission denied: '{}' is outside the workspace. Call request_capability to request access.",
                            path.display()
                        )));
                    }
                    crate::policy::PolicyDecision::Denied(reason) => {
                        return Err(WorkspaceError::PermissionDenied(reason));
                    }
                }

                let parent_path = canonical.parent().ok_or(WorkspaceError::InvalidPath)?;
                let file_name = canonical.file_name().ok_or(WorkspaceError::InvalidPath)?;

                let parent_file = open_canonical_parent_stepwise(parent_path)?;
                let parent_meta = parent_file.metadata()?;
                if !parent_meta.is_dir() {
                    return Err(WorkspaceError::NotDirectory);
                }
                self.check_protected_metadata(&parent_meta)?;

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

                self.check_protected_metadata(&metadata)?;
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
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_persistent_permission_conflicts_with_protected_path() {
        let ws_dir = tempdir().unwrap();
        let outside_dir = tempdir().unwrap();
        let db_file = outside_dir.path().join("orbit.db");
        std::fs::write(&db_file, "data").unwrap();

        let mut ws = Workspace::new(ws_dir.path()).unwrap();
        ws.protect(&db_file).unwrap();

        // 1. configure_policy with permissions covering db_file fails upfront
        let perms = WorkspacePermissions {
            read: vec![outside_dir.path().to_path_buf()],
            read_write: Vec::new(),
        };
        let err = ws
            .configure_policy(perms, NetworkPolicy::Allow)
            .unwrap_err()
            .to_string();
        assert!(err.contains("conflicts with protected path"));

        // 2. protect() fails if persistent permission is already configured
        let other_outside = tempdir().unwrap();
        let mut ws2 = Workspace::new(ws_dir.path()).unwrap();
        let perms2 = WorkspacePermissions {
            read: vec![other_outside.path().to_path_buf()],
            read_write: Vec::new(),
        };
        ws2.configure_policy(perms2, NetworkPolicy::Allow).unwrap();

        let sub_secret = other_outside.path().join("secret.db");
        let err2 = ws2.protect(&sub_secret).unwrap_err().to_string();
        assert!(err2.contains("conflicts with persistent permission"));
    }
}
