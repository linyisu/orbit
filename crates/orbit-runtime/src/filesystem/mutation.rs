//! Handle-relative primitives for approved atomic file transactions.

use std::{
    ffi::OsStr,
    fs::File,
    io,
    path::{Component, Path},
};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
#[cfg(unix)]
use unix as native;
#[cfg(windows)]
use windows as native;

/// Stable native identity of an opened file or directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileIdentity(native::Identity);

impl FileIdentity {
    pub fn read(file: &File) -> io::Result<Self> {
        native::identity(file).map(Self)
    }

    /// Encode the complete native identity for binding an ephemeral transaction receipt.
    pub fn bytes(&self) -> Vec<u8> {
        native::identity_bytes(&self.0)
    }
}

/// A change stamp and permission snapshot captured before approval.
#[derive(Debug, PartialEq, Eq)]
pub struct FileState {
    version: super::FileVersion,
    permissions: native::Permissions,
}

impl FileState {
    /// Capture attributes required to detect changes and preserve replacement permissions.
    /// Native attributes that cannot safely be preserved are rejected before mutation.
    pub fn read(file: &File) -> io::Result<Self> {
        Ok(Self {
            version: super::FileVersion::read(file)?,
            permissions: native::permissions(file)?,
        })
    }

    /// Apply the approved permission snapshot to an unpublished replacement file.
    pub fn apply_permissions(&self, file: &File) -> io::Result<()> {
        native::apply_permissions(file, &self.permissions)
    }
}

pub enum Replacement {
    Create,
    Replace,
}

fn leaf(name: &OsStr) -> io::Result<()> {
    let path = Path::new(name);
    let mut components = path.components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected one file name",
        ));
    }
    #[cfg(windows)]
    super::path_security::validate_windows_path(path)?;
    Ok(())
}

/// Open one directory entry without following links under an authorized directory handle.
pub fn open_directory(parent: &File, name: &OsStr) -> io::Result<File> {
    leaf(name)?;
    super::access::open_directory(parent, Path::new(name))
}

/// Open an absolute directory by traversing each component without following links.
/// This preserves path identity checks; it does not grant workspace access.
pub fn open_directory_path(path: &Path) -> Result<File, super::WorkspaceError> {
    #[cfg(windows)]
    super::path_security::validate_windows_path(path)?;
    super::workspace::open_canonical_parent_stepwise(path)
}

/// Open one regular file without following links under an authorized directory handle.
pub fn open_regular(parent: &File, name: &OsStr) -> io::Result<File> {
    leaf(name)?;
    let file = super::access::open_read(parent, Path::new(name))?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected a regular file",
        ));
    }
    Ok(file)
}

/// Create one directory entry without traversing a mutable pathname.
pub fn create_directory(parent: &File, name: &OsStr) -> io::Result<()> {
    leaf(name)?;
    native::create_directory(parent, name)
}

/// Create a private regular file exclusively under an already authorized directory handle.
pub fn create_private(parent: &File, name: &OsStr) -> io::Result<File> {
    leaf(name)?;
    native::create_private(parent, name)
}

/// Remove a single directory entry without following a final link.
pub fn remove_file(parent: &File, name: &OsStr) -> io::Result<()> {
    leaf(name)?;
    native::remove_file(parent, name)
}

/// Atomically publish a same-directory replacement; Create refuses an existing destination.
/// Callers must recheck their approval snapshot immediately before this commit point.
pub fn publish(
    parent: &File,
    file: &File,
    source: &OsStr,
    target: &OsStr,
    mode: Replacement,
) -> io::Result<()> {
    leaf(source)?;
    leaf(target)?;
    native::publish(parent, file, source, target, mode)
}

/// Flush directory updates after committing, reporting any failure separately from the commit.
pub fn sync_directory(parent: &File) -> io::Result<()> {
    native::sync_directory(parent)
}

/// Discard a still-unpublished replacement using its original creation handle.
/// This may clear read-only attributes on Windows solely to clean up this temporary object.
/// Never call after a successful publish, even if the subsequent directory flush failed.
pub fn discard_unpublished(parent: &File, name: &OsStr, file: &File) -> io::Result<()> {
    leaf(name)?;
    native::discard_unpublished(parent, name, file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn publishes_atomically_without_overwriting_a_create_collision() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = super::super::Workspace::new(directory.path()).unwrap();
        let parent = workspace
            .open_directory(Path::new(""))
            .unwrap();
        std::fs::write(directory.path().join("target"), b"before").unwrap();
        let original = File::open(directory.path().join("target")).unwrap();
        let before = FileState::read(&original).unwrap();
        let mut file = create_private(&parent, OsStr::new("pending")).unwrap();
        file.write_all(b"after").unwrap();
        assert!(
            publish(
                &parent,
                &file,
                OsStr::new("pending"),
                OsStr::new("target"),
                Replacement::Create
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(directory.path().join("target")).unwrap(),
            b"before"
        );
        before.apply_permissions(&file).unwrap();
        file.sync_all().unwrap();
        publish(
            &parent,
            &file,
            OsStr::new("pending"),
            OsStr::new("target"),
            Replacement::Replace,
        )
        .unwrap();
        sync_directory(&parent).unwrap();
        let replacement = File::open(directory.path().join("target")).unwrap();
        assert_eq!(
            native::permissions(&original).unwrap(),
            native::permissions(&replacement).unwrap()
        );
        assert_eq!(
            std::fs::read(directory.path().join("target")).unwrap(),
            b"after"
        );
        assert!(
            !directory
                .path()
                .join("pending")
                .exists()
        );
    }

    #[test]
    fn creation_is_exclusive_and_stays_under_the_parent_handle() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = super::super::Workspace::new(directory.path()).unwrap();
        let parent = workspace
            .open_directory(Path::new(""))
            .unwrap();
        assert!(create_private(&parent, OsStr::new("../outside")).is_err());
        let file = create_private(&parent, OsStr::new("pending")).unwrap();
        assert_eq!(
            create_private(&parent, OsStr::new("pending"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        #[cfg(windows)]
        super::super::private::open_file(&directory.path().join("pending"), false, false).unwrap();
        discard_unpublished(&parent, OsStr::new("pending"), &file).unwrap();
        drop(file);
        assert!(
            !directory
                .path()
                .join("pending")
                .exists()
        );
    }

    #[cfg(windows)]
    #[test]
    fn unpublished_readonly_replacements_are_cleaned_up_using_the_original_handle() {
        let directory = tempfile::tempdir().unwrap();
        let workspace = super::super::Workspace::new(directory.path()).unwrap();
        let parent = workspace
            .open_directory(Path::new(""))
            .unwrap();
        let file = create_private(&parent, OsStr::new("pending")).unwrap();
        let mut permissions = file.metadata().unwrap().permissions();
        permissions.set_readonly(true);
        file.set_permissions(permissions)
            .unwrap();
        discard_unpublished(&parent, OsStr::new("pending"), &file).unwrap();
        drop(file);
        assert!(
            !directory
                .path()
                .join("pending")
                .exists()
        );
    }
}
