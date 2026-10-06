use std::{
    io::Write,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::PathBuf,
};

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};

pub(super) fn executable(bundle: Option<&[u8]>) -> Result<PathBuf> {
    if let Some(bytes) = bundle {
        let cache = std::env::var_os("XDG_CACHE_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
            .context("cannot locate the local server cache")?;
        ensure!(cache.is_absolute(), "server cache must be an absolute path");
        let directory = cache.join("orbit/servers");
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&directory)?;
        let metadata = std::fs::symlink_metadata(&directory)?;
        ensure!(
            metadata.is_dir()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.mode() & 0o022 == 0,
            "local server cache must be owned by this user and not writable by others"
        );
        let digest = Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let executable = directory.join(format!("orbit-server-{digest}"));
        if !executable.try_exists()? {
            let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
            temporary.write_all(bytes)?;
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
        let metadata = std::fs::symlink_metadata(&executable)?;
        ensure!(
            metadata.is_file()
                && metadata.uid() == unsafe { libc::geteuid() }
                && metadata.mode() & 0o022 == 0
                && metadata.mode() & 0o100 != 0
                && metadata.len() == bytes.len() as u64,
            "invalid cached workspace server"
        );
        return Ok(executable);
    }
    let sibling = std::env::current_exe()?
        .parent()
        .context("client executable has no parent directory")?
        .join("orbit-workspace-server");
    ensure!(
        sibling.is_file(),
        "workspace server is missing; install orbit-server beside the client"
    );
    Ok(sibling)
}
