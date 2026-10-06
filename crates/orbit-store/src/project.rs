//! Project identity is stored only in the central database. Never writes markers.
use std::{fs, path::Path, time::UNIX_EPOCH};

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub path: Option<String>,
}

#[derive(Debug)]
pub(crate) struct Directory {
    pub path: String,
    pub identity: Option<String>,
    pub strong: bool,
}

/// A Linux mount can expose the same directory with a different device
/// number. The inode *and* filesystem birth time remain stable in that case.
/// Only use this weaker match together with a previously recorded path.
pub(crate) fn same_directory_identity(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }

    fn linux_inode_birth(identity: &str) -> Option<(&str, &str)> {
        let mut fields = identity.split(':');

        if fields.next()? != "linux" {
            return None;
        }

        let _device = fields.next()?;
        let inode = fields.next()?;
        let birth = fields.next()?;

        if inode.is_empty() || birth.is_empty() || fields.next().is_some() {
            return None;
        }

        Some((inode, birth))
    }

    matches!(
        (linux_inode_birth(left), linux_inode_birth(right)),
        (Some(a), Some(b)) if a == b
    )
}

impl Directory {
    pub fn read(path: &Path) -> Result<Self> {
        let path = path.canonicalize()?;
        let metadata = fs::metadata(&path)?;
        ensure!(metadata.is_dir(), "project path is not a directory");

        // Reject lossy names rather than letting different paths collapse together.
        let path_text = path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("project path must be UTF-8"))?
            .to_owned();
        let object = object_id(&path, &metadata);
        let birth = metadata
            .created()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|t| t.as_nanos());
        #[cfg(target_os = "linux")]
        let birth = birth.or_else(|| linux_birth_time(&path, &metadata));
        let strong = object.is_some() && birth.is_some();
        let identity = object.map(|id| {
            format!(
                "{}:{id}:{}",
                std::env::consts::OS,
                birth.map_or_else(|| "unknown".into(), |b| b.to_string())
            )
        });

        Ok(Self {
            path: path_text,
            identity,
            strong,
        })
    }
}

// musl's standard metadata may omit birth time even when the filesystem supplies it.
#[cfg(target_os = "linux")]
fn linux_birth_time(path: &Path, metadata: &fs::Metadata) -> Option<u128> {
    use rustix::fs::{AtFlags, CWD, StatxFlags, statx};
    use std::os::unix::fs::MetadataExt;

    let stat = statx(
        CWD,
        path,
        AtFlags::empty(),
        StatxFlags::BTIME | StatxFlags::INO,
    )
    .ok()?;
    if stat.stx_mask & StatxFlags::BTIME.bits() == 0 || stat.stx_ino != metadata.ino() {
        return None;
    }
    let seconds = u128::try_from(stat.stx_btime.tv_sec).ok()?;
    Some(seconds * 1_000_000_000 + u128::from(stat.stx_btime.tv_nsec))
}

#[cfg(unix)]
fn object_id(_: &Path, metadata: &fs::Metadata) -> Option<String> {
    use std::os::unix::fs::MetadataExt;

    Some(format!("{}:{}", metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
fn object_id(path: &Path, _: &fs::Metadata) -> Option<String> {
    use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};

    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx,
    };

    let file = fs::OpenOptions::new()
        .access_mode(0)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .ok()?;
    let mut info = std::mem::MaybeUninit::<FILE_ID_INFO>::uninit();
    // SAFETY: the owned handle stays live; the buffer is correctly sized and
    // initialized by Windows only on a successful return.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            info.as_mut_ptr().cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    };

    if ok == 0 {
        return None;
    }

    let info = unsafe { info.assume_init() };

    Some(format!(
        "{}:{:032x}",
        info.VolumeSerialNumber,
        u128::from_le_bytes(info.FileId.Identifier)
    ))
}

#[cfg(not(any(unix, windows)))]
fn object_id(_: &Path, _: &fs::Metadata) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::same_directory_identity;

    #[cfg(target_os = "linux")]
    #[test]
    fn kernel_birth_time_matches_native_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let metadata = std::fs::metadata(directory.path()).unwrap();
        let birth = metadata
            .created()
            .unwrap()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        assert_eq!(
            super::linux_birth_time(directory.path(), &metadata),
            Some(birth)
        );
    }

    #[test]
    fn linux_mount_device_change_does_not_change_directory_identity() {
        assert!(same_directory_identity(
            "linux:66308:11940401:1789431744",
            "linux:66307:11940401:1789431744"
        ));
        assert!(!same_directory_identity(
            "linux:66308:11940401:1789431744",
            "linux:66307:11940402:1789431744"
        ));
        assert!(!same_directory_identity(
            "linux:66308:11940401:1789431744",
            "linux:66307:11940401:1789431745"
        ));
    }
}
