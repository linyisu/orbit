use std::{
    fs::File,
    io,
    path::{Component, Path, PathBuf},
    sync::Arc,
};

use rustix::fs::{Mode, OFlags, openat};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("workspace root must be a directory")]
    NotDirectory,
    #[error("use a workspace-relative path without '..'")]
    InvalidPath,
    #[error("access to this configuration or metadata file is blocked")]
    ProtectedPath,
    #[error("only regular files can be read")]
    NotFile,
    #[error("cannot open workspace file (symlinks are not followed): {0}")]
    Io(#[from] io::Error),
}

#[derive(Clone)]
pub struct Workspace {
    root: Arc<File>,
    path: PathBuf,
    protected: Vec<PathBuf>,
}

impl Workspace {
    pub fn new(root: impl AsRef<Path>) -> Result<Self, WorkspaceError> {
        let path = root.as_ref().canonicalize()?;
        let root = File::open(&path)?;
        if !root.metadata()?.is_dir() {
            return Err(WorkspaceError::NotDirectory);
        }

        Ok(Self {
            root: Arc::new(root),
            path,
            protected: Vec::new(),
        })
    }

    pub fn root(&self) -> &Path {
        &self.path
    }

    pub fn protect(&mut self, path: impl AsRef<Path>) -> Result<(), WorkspaceError> {
        let path = path.as_ref();
        let resolved = match path.canonicalize() {
            Ok(path) => path,
            Err(error) if error.kind() == io::ErrorKind::NotFound => path
                .parent()
                .ok_or(WorkspaceError::InvalidPath)?
                .canonicalize()?
                .join(path.file_name().ok_or(WorkspaceError::InvalidPath)?),
            Err(error) => return Err(error.into()),
        };
        self.protected.push(resolved);
        Ok(())
    }

    pub fn open_read(&self, path: &Path) -> Result<File, WorkspaceError> {
        let mut parts = Vec::new();
        for component in path.components() {
            match component {
                Component::Normal(name) => {
                    let name_text = name.to_string_lossy();
                    if matches!(
                        name_text.as_ref(),
                        ".git" | ".zcode" | ".codex" | ".env" | "config.toml"
                    ) || name_text.starts_with(".env.")
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
            .any(|protected| *protected == self.path.join(&relative))
        {
            return Err(WorkspaceError::ProtectedPath);
        }

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
}
