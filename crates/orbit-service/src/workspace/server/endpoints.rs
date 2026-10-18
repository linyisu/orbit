//! Database identity owns the endpoint; protocol negotiation happens on connection.
use super::*;

// These released versions included the wire version in their endpoint key.
// Keep their addresses as aliases so already-open terminals can reconnect.
#[cfg(unix)]
const LEGACY_VERSIONS: [u32; 2] = [7, 8];

fn address(root: &Path, identity: impl serde::Serialize) -> Result<PathBuf> {
    let key = Sha256::digest(serde_json::to_vec(&identity)?)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok(root.join(format!("{}.sock", &key[..24])))
}

pub(super) fn shared(root: &Path, database: &Path) -> Result<PathBuf> {
    address(root, ("shared-service-v2", database))
}

pub(super) fn legacy(root: &Path, database: &Path) -> Result<Vec<(u32, PathBuf)>> {
    #[cfg(windows)]
    {
        let _ = (root, database);
        Ok(Vec::new())
    }
    #[cfg(unix)]
    LEGACY_VERSIONS
        .into_iter()
        .map(|version| {
            Ok((
                version,
                address(root, ("shared-service-v1", database, version))?,
            ))
        })
        .collect()
}

pub(super) fn lock_legacy(root: &Path, database: &Path) -> Result<Vec<fs::File>> {
    legacy(root, database)?
        .into_iter()
        .map(|(_, socket)| {
            let file = lifecycle::private_file(&socket.with_extension("lock"))?;
            file.try_lock()
                .context("旧版工作区服务仍在退出，等待交接后重试")?;
            Ok(file)
        })
        .collect()
}

pub(super) fn publish_aliases(root: &Path, database: &Path, socket: &Path) -> Result<()> {
    #[cfg(windows)]
    let _ = socket;
    for (_, alias) in legacy(root, database)? {
        match fs::remove_file(&alias) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(socket, alias)?;
    }
    Ok(())
}
