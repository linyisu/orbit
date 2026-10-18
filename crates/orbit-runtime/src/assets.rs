//! Immutable content-addressed files. Database ownership belongs to orbit-store;
//! open shared leases bridge imports and commits without racing collection.
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::fs::OpenOptions;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
};

/// Total persisted attachment byte budget for one database-owned asset directory.
pub const MAX_ASSET_DIRECTORY_BYTES: u64 = 512 * 1_024 * 1_024;

#[derive(Clone)]
pub struct AssetDirectory {
    root: PathBuf,
}

#[derive(Debug)]
pub struct AssetLease {
    _file: File,
}

pub struct AssetDirectoryLock {
    directory: AssetDirectory,
    _file: File,
}

impl AssetDirectory {
    /// Maximum ASCII extension length in content-addressed attachment filenames.
    pub const MAX_EXTENSION_BYTES: usize = 16;

    pub fn for_database(database: &Path) -> Self {
        Self {
            root: database.with_extension("attachments"),
        }
    }

    pub fn path(&self) -> &Path {
        &self.root
    }

    pub fn lock(&self) -> Result<AssetDirectoryLock> {
        #[cfg(windows)]
        crate::filesystem::private::create_directory(&self.root)?;
        #[cfg(unix)]
        {
            match std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&self.root)
            {
                Ok(()) => (),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => (),
                Err(error) => return Err(error.into()),
            }
            let metadata = self.root.symlink_metadata()?;
            ensure!(
                metadata.is_dir()
                    && !metadata.file_type().is_symlink()
                    && metadata.uid() == unsafe { libc::geteuid() },
                "invalid attachment asset directory"
            );
            std::fs::set_permissions(&self.root, std::fs::Permissions::from_mode(0o700))?;
        }
        let file = open_private(&self.root.join(".lock"), true)?;
        let started = std::time::Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(std::fs::TryLockError::WouldBlock)
                    if started.elapsed() < std::time::Duration::from_secs(3) =>
                {
                    std::thread::sleep(std::time::Duration::from_millis(5))
                }
                Err(error) => {
                    return Err(anyhow::anyhow!(error))
                        .context("attachment asset directory is busy");
                }
            }
        }
        Ok(AssetDirectoryLock {
            directory: self.clone(),
            _file: file,
        })
    }

    pub fn file_name(&self, path: &Path) -> Option<String> {
        if path.parent() != Some(self.path()) {
            return None;
        }
        let name = path.file_name()?.to_str()?;
        is_asset_name(name).then(|| name.to_owned())
    }

    pub fn import(&self, bytes: &[u8], extension: &str) -> Result<(PathBuf, Arc<AssetLease>)> {
        let digest = hash(bytes);
        let name = format!("{digest}.{extension}");
        ensure!(is_asset_name(&name), "invalid attachment asset extension");
        let lock = self.lock()?;
        lock.prune_pending()?;
        let path = self.root.join(&name);
        let valid = self
            .read(&path, bytes.len())
            .is_ok_and(|existing| existing == bytes);
        if !valid {
            let size: u64 = lock
                .files()?
                .into_iter()
                .map(|(_, size)| size)
                .sum();
            ensure!(
                size.saturating_add(bytes.len() as u64) <= MAX_ASSET_DIRECTORY_BYTES,
                "附件已达到 512 MiB 容量上限；请删除不需要的会话并清理附件后重试，已有附件不会被自动淘汰"
            );
            #[cfg(windows)]
            let mut temporary =
                crate::filesystem::private::temporary_file(&self.root, ".pending-")?;
            #[cfg(unix)]
            let mut temporary = tempfile::Builder::new()
                .prefix(".pending-")
                .tempfile_in(&self.root)?;
            temporary.write_all(bytes)?;
            temporary.as_file().sync_all()?;
            temporary.persist(&path)?;
            #[cfg(unix)]
            File::open(&self.root)?.sync_all()?;
            #[cfg(windows)]
            crate::filesystem::private::sync_directory(&self.root)?;
        }
        let file = open_private(&path, false)?;
        file.lock_shared()?;
        Ok((path, Arc::new(AssetLease { _file: file })))
    }

    pub fn read(&self, path: &Path, max_bytes: usize) -> Result<Vec<u8>> {
        let name = self
            .file_name(path)
            .context("not an attachment asset")?;
        let metadata = self.root.symlink_metadata()?;
        ensure!(
            metadata.is_dir() && !metadata.file_type().is_symlink(),
            "attachment asset directory is not a real directory"
        );
        let file = open_private(path, false)?;
        file.lock_shared()?;
        ensure!(
            file.metadata()?.len() <= max_bytes as u64,
            "image exceeds byte limit"
        );
        let mut bytes = Vec::new();
        file.take(max_bytes as u64 + 1)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() <= max_bytes, "image exceeds byte limit");
        ensure!(
            hash(&bytes) == name.split_once('.').unwrap().0,
            "attachment asset failed integrity check"
        );
        Ok(bytes)
    }
}

fn hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

impl AssetDirectoryLock {
    /// Imports hold this directory lock until rename. Therefore no pending file can belong to a
    /// live writer while we hold it.
    pub fn prune_pending(&self) -> Result<()> {
        for entry in std::fs::read_dir(self.directory.path())? {
            let entry = entry?;
            if !entry
                .file_name()
                .as_encoded_bytes()
                .starts_with(b".pending-")
            {
                continue;
            }
            let metadata = entry.path().symlink_metadata()?;
            if metadata.is_file()
                && !metadata.file_type().is_symlink()
                && is_owned_asset(&entry.path(), &metadata, true)?
            {
                std::fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    }

    pub fn files(&self) -> Result<Vec<(String, u64)>> {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(self.directory.path())? {
            let entry = entry?;
            let Some(name) = entry
                .file_name()
                .to_str()
                .map(str::to_owned)
            else {
                continue;
            };
            let metadata = entry.path().symlink_metadata()?;
            if is_asset_name(&name)
                && metadata.is_file()
                && !metadata.file_type().is_symlink()
                && is_owned_asset(&entry.path(), &metadata, false)?
            {
                files.push((name, metadata.len()));
            }
        }
        Ok(files)
    }

    pub fn remove_if_unused(&self, name: &str) -> Result<bool> {
        ensure!(is_asset_name(name), "invalid asset name");
        let path = self.directory.root.join(name);
        let file = match open_private(&path, false) {
            Ok(file) => file,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(false);
            }
            Err(error) => return Err(error),
        };
        match file.try_lock() {
            Ok(()) => {
                std::fs::remove_file(path)?;
                Ok(true)
            }
            Err(std::fs::TryLockError::WouldBlock) => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
}

pub fn is_asset_name(name: &str) -> bool {
    name.split_once('.')
        .is_some_and(|(hash, extension)| {
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit())
                && !extension.is_empty()
                && extension.len() <= AssetDirectory::MAX_EXTENSION_BYTES
                && extension
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric())
        })
}

#[cfg(windows)]
fn open_private(path: &Path, create: bool) -> Result<File> {
    crate::filesystem::private::open_file(path, create, false).map_err(Into::into)
}

#[cfg(windows)]
fn is_owned_asset(path: &Path, _metadata: &std::fs::Metadata, _private: bool) -> Result<bool> {
    Ok(crate::filesystem::private::open_file(path, false, false).is_ok())
}

#[cfg(unix)]
fn is_owned_asset(_path: &Path, metadata: &std::fs::Metadata, private: bool) -> Result<bool> {
    Ok(metadata.nlink() == 1
        && metadata.uid() == unsafe { libc::geteuid() }
        && (!private || metadata.permissions().mode() & 0o077 == 0))
}

#[cfg(unix)]
fn open_private(path: &Path, create: bool) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(create)
        .create(create)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.nlink() == 1 && metadata.uid() == unsafe { libc::geteuid() },
        "attachment asset is not a private regular file"
    );
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_is_global_deduplication_is_free_and_collection_respects_open_leases() {
        let root = tempfile::tempdir().unwrap();
        let assets = AssetDirectory::for_database(&root.path().join("orbit.db"));
        let (path, lease) = assets
            .import(b"picture", "png")
            .unwrap();
        let (same, duplicate) = assets
            .import(b"picture", "png")
            .unwrap();
        assert_eq!(path, same);
        let lock = assets.lock().unwrap();
        let name = path
            .file_name()
            .unwrap()
            .to_str()
            .unwrap();
        assert!(!lock.remove_if_unused(name).unwrap());
        drop(lease);
        assert!(!lock.remove_if_unused(name).unwrap());
        drop(duplicate);
        assert!(lock.remove_if_unused(name).unwrap());
        let large = assets
            .path()
            .join(format!("{}.png", "a".repeat(64)));
        #[cfg(unix)]
        let large_file = File::create(&large).unwrap();
        #[cfg(windows)]
        let large_file = crate::filesystem::private::open_file(&large, true, true).unwrap();
        large_file
            .set_len(MAX_ASSET_DIRECTORY_BYTES)
            .unwrap();
        drop(lock);
        assert!(
            assets
                .import(b"new picture", "png")
                .unwrap_err()
                .to_string()
                .contains("512 MiB")
        );
        assert!(
            large.exists(),
            "a full directory must not evict referenced history"
        );
    }

    #[test]
    fn collection_never_follows_foreign_symlinks_or_hard_links() {
        #[cfg(unix)]
        use std::os::unix::fs::symlink;
        #[cfg(windows)]
        use std::os::windows::fs::symlink_file as symlink;
        let root = tempfile::tempdir().unwrap();
        let assets = AssetDirectory::for_database(&root.path().join("orbit.db"));
        let lock = assets.lock().unwrap();
        let outside = root.path().join("external");
        std::fs::write(&outside, b"keep").unwrap();
        symlink(
            &outside,
            assets
                .path()
                .join(format!("{}.png", "b".repeat(64))),
        )
        .unwrap();
        std::fs::hard_link(
            &outside,
            assets
                .path()
                .join(format!("{}.png", "c".repeat(64))),
        )
        .unwrap();
        assert!(lock.files().unwrap().is_empty());
        assert!(outside.exists());
    }
}
