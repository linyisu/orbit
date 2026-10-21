#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};

pub(super) struct Bundle {
    bytes: &'static [u8],
    digest: std::sync::OnceLock<String>,
}

impl Bundle {
    pub fn new(bytes: &'static [u8]) -> Self {
        Self {
            bytes,
            digest: Default::default(),
        }
    }

    pub fn bytes(&self) -> &'static [u8] {
        self.bytes
    }

    fn digest(&self) -> &str {
        self.digest.get_or_init(|| {
            Sha256::digest(self.bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect()
        })
    }
}

pub(super) fn executable(bundle: Option<&Bundle>) -> Result<PathBuf> {
    if let Some(bundle) = bundle {
        #[cfg(windows)]
        let cache = std::env::var_os("LOCALAPPDATA")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .context("cannot locate the local server cache")?;
        #[cfg(unix)]
        let cache = std::env::var_os("XDG_CACHE_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
            .context("cannot locate the local server cache")?;
        ensure!(cache.is_absolute(), "server cache must be an absolute path");
        let directory = cache.join("orbit/servers");
        return install(bundle, &directory)
            .with_context(|| format!("无法准备本机工作区服务缓存 {}", directory.display()));
    }
    let sibling = std::env::current_exe()?
        .parent()
        .context("client executable has no parent directory")?
        .join(format!(
            "orbit-workspace-server{}",
            std::env::consts::EXE_SUFFIX
        ));
    ensure!(
        sibling.is_file(),
        "workspace server is missing; install orbit-server beside the client"
    );
    Ok(sibling)
}

fn install(bundle: &Bundle, directory: &Path) -> Result<PathBuf> {
    let bytes = bundle.bytes;
    #[cfg(windows)]
    orbit_runtime::filesystem::private::create_directory_all(directory)?;
    #[cfg(unix)]
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)?;
    #[cfg(unix)]
    let metadata = std::fs::symlink_metadata(directory)?;
    #[cfg(unix)]
    ensure!(
        metadata.is_dir()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o022 == 0,
        "local server cache must be owned by this user and not writable by others"
    );
    let registry = cache_lock(&directory.join("registry.lock"))?;
    registry.lock()?;
    let digest = bundle.digest();
    let executable = directory.join(format!(
        "orbit-server-{digest}{}",
        std::env::consts::EXE_SUFFIX
    ));
    if !executable.try_exists()? {
        #[cfg(unix)]
        let mut temporary = tempfile::NamedTempFile::new_in(directory)?;
        #[cfg(windows)]
        let mut temporary =
            orbit_runtime::filesystem::private::temporary_file(directory, ".server-")?;
        temporary.write_all(bytes)?;
        #[cfg(unix)]
        temporary
            .as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o700))?;
        temporary.as_file().sync_all()?;
        match temporary.persist_noclobber(&executable) {
            Ok(_) => {}
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.error.into()),
        }
    }
    #[cfg(windows)]
    let file = orbit_runtime::filesystem::private::open_file(&executable, false, false)?;
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(&executable)?
    };
    let metadata = file.metadata()?;
    #[cfg(unix)]
    ensure!(
        metadata.is_file()
            && metadata.uid() == unsafe { libc::geteuid() }
            && metadata.mode() & 0o022 == 0
            && metadata.mode() & 0o100 != 0
            && metadata.len() == bytes.len() as u64,
        "invalid cached workspace server"
    );
    ensure!(
        metadata.is_file() && metadata.len() == bytes.len() as u64,
        "invalid cached workspace server"
    );
    let mut reader = std::io::BufReader::new(file);
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1_024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    let actual: String = hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    ensure!(
        actual == digest,
        "cached workspace server checksum mismatch"
    );
    // Keep a shared OS lease for every executable used by this client. A crashed
    // client releases its leases automatically; another process may then prune it.
    static LEASES: std::sync::Mutex<Vec<(PathBuf, std::fs::File)>> =
        std::sync::Mutex::new(Vec::new());
    let mut leases = LEASES
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if !leases
        .iter()
        .any(|(path, _)| path == &executable)
    {
        let lease = cache_lock(&executable.with_extension("lease"))?;
        lease.lock_shared()?;
        leases.push((executable.clone(), lease));
    }
    // Cleanup is opportunistic: Windows also refuses removal of running images.
    // Failure must not turn a verified, usable service into a startup failure.
    let _ = prune(directory, &executable);
    Ok(executable)
}

fn cache_lock(path: &Path) -> Result<std::fs::File> {
    #[cfg(windows)]
    let file = orbit_runtime::filesystem::private::open_file(path, true, false)?;
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)?;
        let metadata = file.metadata()?;
        // SAFETY: geteuid has no arguments or memory preconditions.
        let current_user = unsafe { libc::geteuid() };
        ensure!(
            metadata.is_file()
                && metadata.uid() == current_user
                && metadata.nlink() == 1
                && metadata.mode() & 0o077 == 0,
            "invalid server cache lock"
        );
        file
    };
    Ok(file)
}

fn prune(directory: &Path, current: &Path) -> Result<()> {
    let mut candidates = Vec::new();
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(digest) = name.to_str().and_then(|name| {
            name.strip_prefix("orbit-server-")?
                .strip_suffix(std::env::consts::EXE_SUFFIX)
        }) else {
            continue;
        };
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            continue;
        }
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path)?;
        if path != current && metadata.is_file() && !metadata.file_type().is_symlink() {
            candidates.push((metadata.modified()?, path));
        }
    }
    candidates.sort_by(|left, right| right.cmp(left));
    // Retain the current image, all leased images and one previous inactive image.
    let mut retained_previous = false;
    for (_, path) in candidates {
        let lease = cache_lock(&path.with_extension("lease"))?;
        if lease.try_lock().is_err() {
            continue;
        }
        if !retained_previous {
            retained_previous = true;
            continue;
        }
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_pruning_preserves_current_leased_and_previous_images() {
        let root = tempfile::tempdir().unwrap();
        let path = |index| {
            root.path().join(format!(
                "orbit-server-{index:064x}{}",
                std::env::consts::EXE_SUFFIX
            ))
        };
        let files = (0..5).map(path).collect::<Vec<_>>();
        for file in &files {
            std::fs::write(file, b"generated executable").unwrap();
        }
        let lease = cache_lock(&files[0].with_extension("lease")).unwrap();
        lease.lock_shared().unwrap();
        std::fs::write(root.path().join("unrelated.exe"), b"keep").unwrap();
        prune(root.path(), &files[1]).unwrap();
        assert!(files[0].exists());
        assert!(files[1].exists());
        assert_eq!(
            files
                .iter()
                .filter(|path| path.exists())
                .count(),
            3
        );
        assert!(
            root.path()
                .join("unrelated.exe")
                .exists()
        );
        // Parallel subprocess tests can briefly inherit this lease between fork and exec.
        lease.unlock().unwrap();
        drop(lease);
        prune(root.path(), &files[1]).unwrap();
        assert_eq!(
            files
                .iter()
                .filter(|path| path.exists())
                .count(),
            2
        );
    }

    #[test]
    fn cached_server_is_verified_even_when_tampering_preserves_length() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("servers");
        let bundle = Bundle::new(b"synthetic server");
        let path = install(&bundle, &cache).unwrap();
        assert_eq!(install(&bundle, &cache).unwrap(), path);
        std::fs::write(&path, b"synthetic tamper").unwrap();
        assert!(
            install(&bundle, &cache)
                .unwrap_err()
                .to_string()
                .contains("checksum")
        );
    }

    #[test]
    fn cached_server_does_not_follow_file_aliases() {
        let root = tempfile::tempdir().unwrap();
        let cache = root.path().join("servers");
        let bundle = Bundle::new(b"synthetic server");
        let path = install(&bundle, &cache).unwrap();
        let outside = root.path().join("outside");
        std::fs::rename(&path, &outside).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &path).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(&outside, &path).unwrap();
        assert!(install(&bundle, &cache).is_err());
        assert_eq!(std::fs::read(outside).unwrap(), bundle.bytes());
    }
}
