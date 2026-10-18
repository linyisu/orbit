//! Stable host metadata is separate from account credential records.
use crate::{AuthError, AuthStore, Result, store::private_file};
use std::io::{Read, Write};

/// Persist one host identifier outside the credential namespace under a process-shared lock.
pub(super) async fn identifier(store: &AuthStore) -> Result<String> {
    let hosts = AuthStore::new(store.directory().join("hosts"))?;
    let _lock = hosts.lock("chatgpt").await?;
    let path = hosts.directory().join("chatgpt.id");
    match private_file(&path, false) {
        Ok(file) => {
            let mut value = String::new();
            file.take(128)
                .read_to_string(&mut value)?;
            let uuid = value
                .strip_prefix("urn:uuid:")
                .ok_or(AuthError::Storage)?;
            if uuid.len() != 36
                || !uuid
                    .bytes()
                    .enumerate()
                    .all(|(index, byte)| {
                        if [8, 13, 18, 23].contains(&index) {
                            byte == b'-'
                        } else {
                            byte.is_ascii_hexdigit()
                        }
                    })
            {
                return Err(AuthError::Storage);
            }
            return Ok(value);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut bytes = [0; 16];
    getrandom::fill(&mut bytes)
        .map_err(|_| AuthError::Configuration("secure randomness unavailable"))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let value = format!(
        "urn:uuid:{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    );
    #[cfg(unix)]
    let mut temporary = tempfile::NamedTempFile::new_in(hosts.directory())?;
    #[cfg(windows)]
    let mut temporary =
        orbit_runtime::filesystem::private::temporary_file(hosts.directory(), "host-")?;
    temporary.write_all(value.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| AuthError::Io(error.error))?;
    Ok(value)
}
