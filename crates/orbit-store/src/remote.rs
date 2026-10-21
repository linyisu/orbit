//! Private immutable device grants. Removal revokes the grant without touching sessions.

use anyhow::{Context, Result, ensure};
use orbit_protocol::remote::Permissions;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
pub struct Grant {
    pub workspace: PathBuf,
    pub database: PathBuf,
    pub permissions: Permissions,
    pub secret_hash: [u8; 32],
}

/// Create a private state directory; reject existing directories with broader access.
pub fn directory(path: &Path) -> Result<()> {
    match std::fs::DirBuilder::new()
        .mode(0o700)
        .create(path)
    {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(
        metadata.is_dir() && metadata.permissions().mode() & 0o077 == 0,
        "remote state must be a private directory"
    );
    Ok(())
}

/// Persist a new secret or grant without replacing an existing identity.
pub fn create(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::File::open(
        path.parent()
            .context("state file has no parent")?,
    )?
    .sync_all()?;
    Ok(())
}

pub fn device_path(directory: &Path, device: &str) -> Result<PathBuf> {
    ensure!(
        device.len() == 64
            && device
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit()),
        "invalid device identity"
    );
    Ok(directory.join(format!("device-{device}.json")))
}

pub fn digest(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

/// Every request reloads its grant so a removed registration fails closed.
pub fn authenticate(directory: &Path, device: &str, secret: &str) -> Result<Grant> {
    let path = device_path(directory, device)?;
    let file = orbit_runtime::filesystem::open_regular_file(&path)?;
    ensure!(
        file.metadata()?.len() <= 16 * 1_024,
        "invalid device registration"
    );
    let grant: Grant = serde_json::from_reader(file)?;
    let actual = digest(secret);
    let difference = actual
        .iter()
        .zip(grant.secret_hash)
        .fold(0u8, |difference, (a, b)| difference | (a ^ b));
    ensure!(difference == 0, "device authentication failed");
    Ok(grant)
}

pub fn revoke(directory: &Path, device: &str) -> Result<()> {
    std::fs::remove_file(device_path(directory, device)?)?;
    std::fs::File::open(directory)?.sync_all()?;
    Ok(())
}
