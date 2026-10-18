mod access;
pub mod path_security;
#[cfg(windows)]
pub mod private;
mod version;
pub mod workspace;

pub use path_security::ResolvedPath;
pub use version::FileVersion;
pub use workspace::{Workspace, WorkspaceError};

/// Open an internal regular file without following a final symlink or reparse point.
/// This does not authorize workspace access or validate ownership; callers own those checks.
/// Parent paths must already be trusted. Unix FIFOs are opened nonblocking and then rejected.
pub fn open_regular_file(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        };
        path_security::validate_windows_path(path)?;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "reparse points are not regular internal files",
            ));
        }
    }
    if !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "only regular files can be read",
        ));
    }
    Ok(file)
}

/// Query hard links on the opened object, without a second path lookup.
pub fn hard_link_count(file: &std::fs::File) -> std::io::Result<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(file.metadata()?.nlink())
    }
    #[cfg(windows)]
    {
        Ok(cap_fs_ext::MetadataExt::nlink(
            &cap_std::fs::Metadata::from_file(file)?,
        ))
    }
}

/// Create a private temporary directory before placing any user content inside it.
/// Unix uses mode 0700; Windows sets and verifies explicit ownership and a protected DACL.
pub fn temporary_directory(prefix: &str) -> std::io::Result<tempfile::TempDir> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tempfile::Builder::new()
            .prefix(prefix)
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir()
    }
    #[cfg(windows)]
    {
        private::temporary_directory(&std::env::temp_dir(), prefix)
    }
}

#[cfg(test)]
pub(crate) fn private_test_directory() -> std::io::Result<tempfile::TempDir> {
    #[cfg(unix)]
    {
        tempfile::tempdir()
    }
    #[cfg(windows)]
    {
        private::temporary_directory(&std::env::temp_dir(), "orbit-test-")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_reads_reject_links_and_count_hard_links_on_the_handle() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config");
        std::fs::write(&path, b"configuration").unwrap();
        let file = open_regular_file(&path).unwrap();
        assert_eq!(hard_link_count(&file).unwrap(), 1);
        std::fs::hard_link(&path, directory.path().join("hard-link")).unwrap();
        assert_eq!(hard_link_count(&file).unwrap(), 2);
        let alias = directory.path().join("alias");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&path, &alias).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&path, &alias).unwrap();
        assert!(open_regular_file(&alias).is_err());
        assert!(open_regular_file(directory.path()).is_err());
    }
}
