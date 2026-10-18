//! Handle-based file versions for paginated reads and conversion caches.

use std::{fs::File, io};

/// An observed file identity and change stamp, not a content hash or a filesystem lock.
/// Capture from the same open handle before and after reading; a mismatch invalidates the read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileVersion(String);

impl FileVersion {
    /// Query identity, length and native write/change timestamps without reopening a path.
    /// Unsupported native identity queries fail rather than falling back to a pathname.
    pub fn read(file: &File) -> io::Result<Self> {
        read(file).map(Self)
    }

    /// Observe a cache dependency by path, following links as ordinary metadata queries do.
    /// Requires attribute access only, so an executable need not also be readable as data.
    pub fn for_path(path: &std::path::Path) -> io::Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self(from_metadata(&std::fs::metadata(path)?)))
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::FileSystem::FILE_READ_ATTRIBUTES;
            let file = std::fs::OpenOptions::new()
                .access_mode(FILE_READ_ATTRIBUTES)
                .open(path)?;
            Self::read(&file)
        }
    }

    /// Return the opaque continuation token. Existing Unix tokens retain their encoding.
    pub fn token(&self) -> &str {
        &self.0
    }
}

#[cfg(unix)]
fn read(file: &File) -> io::Result<String> {
    Ok(from_metadata(&file.metadata()?))
}

#[cfg(unix)]
fn from_metadata(metadata: &std::fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;

    format!(
        "{}:{}:{}:{}:{}:{}:{}",
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec()
    )
}

#[cfg(windows)]
fn read(file: &File) -> io::Result<String> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_BASIC_INFO, FileBasicInfo, GetFileInformationByHandleEx,
    };

    let (volume, identity) = super::private::identity(file)?;
    let mut basic = FILE_BASIC_INFO::default();
    // SAFETY: The file owns a live handle and basic is a correctly sized writable buffer.
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileBasicInfo,
            (&mut basic as *mut FILE_BASIC_INFO).cast(),
            std::mem::size_of::<FILE_BASIC_INFO>() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(format!(
        "windows:{volume}:{identity:02x?}:{}:{}:{}",
        file.metadata()?.len(),
        basic.LastWriteTime,
        basic.ChangeTime
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn versions_distinguish_replacements_and_length_changes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source");
        std::fs::write(&path, b"first").unwrap();
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let original = FileVersion::read(&file).unwrap();
        assert_eq!(original, FileVersion::read(&file).unwrap());
        file.write_all(b"longer source")
            .unwrap();
        file.sync_all().unwrap();
        assert_ne!(original, FileVersion::read(&file).unwrap());

        // Keep the old handle alive so its file identity cannot be recycled.
        std::fs::rename(&path, directory.path().join("previous")).unwrap();
        std::fs::write(&path, b"first").unwrap();
        let replacement = File::open(path).unwrap();
        assert_ne!(original, FileVersion::read(&replacement).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn existing_unix_cursor_encoding_is_preserved() {
        use std::os::unix::fs::MetadataExt;

        let file = tempfile::tempfile().unwrap();
        let metadata = file.metadata().unwrap();
        assert_eq!(
            FileVersion::read(&file)
                .unwrap()
                .token(),
            format!(
                "{}:{}:{}:{}:{}:{}:{}",
                metadata.dev(),
                metadata.ino(),
                metadata.len(),
                metadata.mtime(),
                metadata.mtime_nsec(),
                metadata.ctime(),
                metadata.ctime_nsec()
            )
        );
    }
}
