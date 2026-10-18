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
        return install(bundle, &directory);
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
    Ok(executable)
}

#[cfg(test)]
mod tests {
    use super::*;

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
