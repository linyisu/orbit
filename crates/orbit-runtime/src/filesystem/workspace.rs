use std::os::unix::fs::MetadataExt;
use std::{
    collections::HashSet,
    fs::File,
    io,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use rustix::fs::{Mode, OFlags, openat};
use thiserror::Error;

use crate::{
    capability::NetworkPolicy,
    filesystem::inodes::collect_dir_inodes,
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
    opaque_runtime_roots: Vec<PathBuf>,
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
            opaque_runtime_roots: Vec::new(),
            policy,
        })
    }

    pub fn root(&self) -> &Path {
        &self.path
    }

    pub fn protected(&self) -> &[PathBuf] {
        &self.protected
    }

    pub fn opaque_runtime_roots(&self) -> &[PathBuf] {
        &self.opaque_runtime_roots
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

        for p in &self.protected {
            policy.add_protected_path(p.clone());
        }

        let effective = policy.effective_policy();
        for template in effective
            .read_templates
            .iter()
            .chain(effective.write_templates.iter())
        {
            if self
                .protected
                .iter()
                .any(|p| template.starts_with(p) || p.starts_with(template))
            {
                return Err(WorkspaceError::ProtectedPath);
            }
        }

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

        self.policy = policy;

        Ok(())
    }

    fn resolve_and_validate_protected_path(&self, path: &Path) -> Result<PathBuf, WorkspaceError> {
        let requested = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.path.join(path)
        };
        let mut ancestor = requested.as_path();
        let mut missing = Vec::new();

        // Runtime directories may have several absent parents on first launch.
        // Resolve the existing prefix without creating directories or hiding I/O
        // failures; a dangling symlink must not be mistaken for a missing path.
        let mut resolved = loop {
            match ancestor.canonicalize() {
                Ok(path) => break path,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    match std::fs::symlink_metadata(ancestor) {
                        Ok(_) => return Err(error.into()),
                        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    }

                    missing.push(
                        ancestor
                            .file_name()
                            .ok_or(WorkspaceError::InvalidPath)?,
                    );
                    ancestor = ancestor
                        .parent()
                        .ok_or(WorkspaceError::InvalidPath)?;
                }

                Err(error) => return Err(error.into()),
            }
        };

        for component in missing.into_iter().rev() {
            resolved.push(component);
        }

        let effective = self.policy.effective_policy();
        if effective
            .read_templates
            .iter()
            .chain(effective.write_templates.iter())
            .any(|p| p.starts_with(&resolved) || resolved.starts_with(p))
        {
            return Err(WorkspaceError::ProtectedPath);
        }

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

        Ok(resolved)
    }

    pub fn protect(&mut self, path: impl AsRef<Path>) -> Result<(), WorkspaceError> {
        let resolved = self.resolve_and_validate_protected_path(path.as_ref())?;
        self.protected.push(resolved.clone());
        self.policy.add_protected_path(resolved);
        Ok(())
    }

    /// Protects an internal runtime storage directory (e.g. orbit.workspaces, command-output).
    ///
    /// Opaque runtime roots cannot be accessed through file tools or capability grants.
    /// Unlike regular sensitive directories, external roots are not scanned until a
    /// hardlink could alias a descendant. Native shell commands use OS permissions.
    pub fn protect_opaque_runtime_root(
        &mut self,
        path: impl AsRef<Path>,
    ) -> Result<(), WorkspaceError> {
        let resolved = self.resolve_and_validate_protected_path(path.as_ref())?;
        self.protected.push(resolved.clone());
        self.opaque_runtime_roots
            .push(resolved.clone());
        self.policy.add_protected_path(resolved);
        Ok(())
    }

    pub fn checked_relative(&self, path: &Path) -> Result<PathBuf, WorkspaceError> {
        let mut parts = Vec::new();

        for component in path.components() {
            match component {
                Component::Normal(name) => {
                    let name_text = name.to_string_lossy();

                    if super::path_security::is_protected_workspace_name(&name_text)
                        && !self
                            .policy
                            .effective_policy()
                            .read_templates
                            .iter()
                            .chain(
                                self.policy
                                    .effective_policy()
                                    .write_templates
                                    .iter(),
                            )
                            .any(|p| {
                                p == &self
                                    .path
                                    .join(parts.iter().collect::<PathBuf>())
                                    .join(name)
                            })
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

        if self.protected.iter().any(|protected| {
            self.path
                .join(&relative)
                .starts_with(protected)
        }) {
            return Err(WorkspaceError::ProtectedPath);
        }

        Ok(relative)
    }

    /// Resolve links through directory descriptors, with no canonicalize/open gap.
    /// Link targets may traverse upward only within the workspace root.
    pub fn write_parent(&self, path: &Path) -> Result<(File, std::ffi::OsString), WorkspaceError> {
        self.resolve_parent(path, true)
    }

    fn resolve_parent(
        &self,
        path: &Path,
        write: bool,
    ) -> Result<(File, std::ffi::OsString), WorkspaceError> {
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
            let effective = self.policy.effective_policy();
            let absolute = self.path.join(&candidate);
            if write
                && effective
                    .read_templates
                    .contains(&absolute)
                && !effective
                    .write_templates
                    .contains(&absolute)
            {
                return Err(WorkspaceError::ProtectedPath);
            }

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

                    self.check_protected_metadata(&next.metadata()?)?;
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
            let (parent, name) = self.resolve_parent(path, false)?;

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
        let mut protected_inodes = HashSet::new();

        for protected in &self.protected {
            let other = match std::fs::symlink_metadata(protected) {
                Ok(other) => other,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => return Err(WorkspaceError::Io(error)),
            };

            if metadata.dev() == other.dev() && metadata.ino() == other.ino() {
                return Err(WorkspaceError::ProtectedPath);
            }

            if metadata.is_file()
                && metadata.nlink() > 1
                && other.is_dir()
                && other.dev() == metadata.dev()
            {
                collect_dir_inodes(protected, &mut protected_inodes)
                    .map_err(|error| WorkspaceError::PermissionDenied(error.to_string()))?;
            }
        }

        if protected_inodes.contains(&(metadata.dev(), metadata.ino())) {
            return Err(WorkspaceError::ProtectedPath);
        }
        if metadata.is_file()
            && metadata.nlink() > 1
            && super::inodes::is_reserved_workspace_inode_with_templates(
                &self.path,
                (metadata.dev(), metadata.ino()),
                metadata.nlink(),
                &self
                    .policy
                    .effective_policy()
                    .read_templates,
                &self
                    .policy
                    .effective_policy()
                    .write_templates,
            )
            .map_err(|error| WorkspaceError::PermissionDenied(error.to_string()))?
        {
            return Err(WorkspaceError::ProtectedPath);
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

                let name_text = file_name.to_string_lossy();

                if super::path_security::is_protected_workspace_name(&name_text) {
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
        let (parent, name) = self.resolve_parent(path, false)?;
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
        self.check_protected_metadata(&metadata)?;
        Ok(file)
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
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn missing_protected_paths_resolve_existing_symlinks_and_reject_dangling_ones() {
        let root = tempdir().unwrap();
        let storage = tempdir().unwrap();
        std::os::unix::fs::symlink(storage.path(), root.path().join("alias")).unwrap();
        std::os::unix::fs::symlink(root.path().join("absent"), root.path().join("dangling"))
            .unwrap();
        std::fs::write(root.path().join("file"), "content").unwrap();
        let mut workspace = Workspace::new(root.path()).unwrap();

        workspace
            .protect(Path::new("alias/config/orbit/auth"))
            .unwrap();
        workspace
            .protect_opaque_runtime_root(
                root.path()
                    .join("missing/runtime/archive"),
            )
            .unwrap();

        assert!(
            workspace
                .protected()
                .contains(&storage.path().join("config/orbit/auth"))
        );
        assert!(
            workspace
                .protect(Path::new("dangling/auth"))
                .is_err()
        );
        assert!(
            workspace
                .protect(Path::new("file/auth"))
                .is_err()
        );
        assert!(!root.path().join("missing").exists());
    }

    #[test]
    fn protected_directory_descendant_hardlinks_cannot_be_read() {
        let ws_dir = tempdir().unwrap();
        let storage = tempdir().unwrap();
        let opaque = storage.path().join("orbit.workspaces");
        std::fs::create_dir(&opaque).unwrap();
        let private_file = opaque.join("secret.txt");
        std::fs::write(&private_file, "private runtime data").unwrap();
        std::fs::hard_link(&private_file, ws_dir.path().join("alias.txt")).unwrap();

        let mut workspace = Workspace::new(ws_dir.path()).unwrap();
        workspace
            .protect_opaque_runtime_root(&opaque)
            .unwrap();

        assert!(matches!(
            workspace.open_read(Path::new("alias.txt")),
            Err(WorkspaceError::ProtectedPath)
        ));
    }

    #[test]
    fn reserved_workspace_file_hardlinks_cannot_be_read() {
        let ws_dir = tempdir().unwrap();
        let nested = ws_dir.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        let private_file = nested.join(".env.local");
        std::fs::write(&private_file, "private workspace data").unwrap();
        std::fs::hard_link(&private_file, ws_dir.path().join("alias.txt")).unwrap();

        let workspace = Workspace::new(ws_dir.path()).unwrap();

        assert!(matches!(
            workspace.open_read(Path::new("alias.txt")),
            Err(WorkspaceError::ProtectedPath)
        ));
    }

    #[test]
    fn test_persistent_permission_conflicts_with_protected_path() {
        let ws_dir = tempdir().unwrap();
        let outside_dir = tempdir().unwrap();
        let db_file = outside_dir.path().join("orbit.db");
        std::fs::write(&db_file, "data").unwrap();

        let mut ws = Workspace::new(ws_dir.path()).unwrap();
        ws.protect(&db_file).unwrap();

        let perms = WorkspacePermissions {
            read_templates: Vec::new(),
            write_templates: Vec::new(),
            read: vec![outside_dir.path().to_path_buf()],
            read_write: Vec::new(),
        };
        let err = ws
            .configure_policy(perms, NetworkPolicy::Allow)
            .unwrap_err()
            .to_string();

        assert!(err.contains("conflicts with protected path"));

        let other_outside = tempdir().unwrap();
        let mut ws2 = Workspace::new(ws_dir.path()).unwrap();
        let perms2 = WorkspacePermissions {
            read_templates: Vec::new(),
            write_templates: Vec::new(),
            read: vec![other_outside.path().to_path_buf()],
            read_write: Vec::new(),
        };

        ws2.configure_policy(perms2, NetworkPolicy::Allow)
            .unwrap();

        let sub_secret = other_outside.path().join("secret.db");
        let err2 = ws2
            .protect(&sub_secret)
            .unwrap_err()
            .to_string();

        assert!(err2.contains("conflicts with persistent permission"));
    }

    #[test]
    fn test_configure_policy_is_transactional() {
        let ws_dir = tempdir().unwrap();
        let valid_dir = tempdir().unwrap();
        let invalid_dir = tempdir().unwrap();
        let secret_file = invalid_dir.path().join("secret.key");
        std::fs::write(&secret_file, "12345").unwrap();

        let mut ws = Workspace::new(ws_dir.path()).unwrap();
        ws.protect(&secret_file).unwrap();

        let initial_perms = WorkspacePermissions {
            read_templates: Vec::new(),
            write_templates: Vec::new(),
            read: vec![valid_dir.path().to_path_buf()],
            read_write: Vec::new(),
        };

        ws.configure_policy(initial_perms, NetworkPolicy::Deny)
            .unwrap();
        let valid_canonical = valid_dir.path().canonicalize().unwrap();

        assert_eq!(
            ws.policy()
                .effective_policy()
                .persistent_read[0]
                .canonical(),
            valid_canonical
        );

        let bad_perms = WorkspacePermissions {
            read_templates: Vec::new(),
            write_templates: Vec::new(),
            read: vec![invalid_dir.path().to_path_buf()],
            read_write: Vec::new(),
        };
        let err = ws
            .configure_policy(bad_perms, NetworkPolicy::Allow)
            .unwrap_err();

        assert!(
            err.to_string()
                .contains("conflicts with protected path")
        );

        let current_policy = ws.policy().effective_policy();

        assert_eq!(current_policy.persistent_read.len(), 1);
        assert_eq!(
            current_policy.persistent_read[0].canonical(),
            valid_canonical
        );
        assert_eq!(current_policy.base.network, NetworkPolicy::Deny);
    }
}
