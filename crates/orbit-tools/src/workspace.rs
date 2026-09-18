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

    pub(crate) fn checked_relative(&self, path: &Path) -> Result<PathBuf, WorkspaceError> {
        let mut parts = Vec::new();
        for component in path.components() {
            match component {
                Component::Normal(name) => {
                    let name_text = name.to_string_lossy();
                    if matches!(name_text.as_ref(), ".git" | ".env" | "config.toml")
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

    pub(crate) fn write_parent(
        &self,
        path: &Path,
    ) -> Result<(File, std::ffi::OsString), WorkspaceError> {
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

    pub(crate) fn open_directory(&self, path: &Path) -> Result<File, WorkspaceError> {
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
                .map_err(io::Error::from)?,
            )
        };
        Ok(directory)
    }

    pub(crate) fn directory_entries(
        &self,
        path: &Path,
    ) -> Result<Vec<std::ffi::OsString>, WorkspaceError> {
        use std::os::unix::ffi::OsStrExt;
        let directory = self.open_directory(path)?;
        let entries = rustix::fs::Dir::read_from(&directory).map_err(io::Error::from)?;
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
}
