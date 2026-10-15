use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};

use url::Url;

use crate::{
    AuthError, AuthTarget, CredentialInfo, CredentialKind, RequestAuth, Result,
    credential::{Credential, Secret, endpoint_scope, now},
};

/// A dedicated credential directory; never the session database or project config.
#[derive(Clone, Debug)]
pub struct AuthStore {
    directory: PathBuf,
}

impl AuthStore {
    pub fn new(directory: impl Into<PathBuf>) -> Result<Self> {
        let directory = directory.into();
        if !directory.is_absolute() {
            return Err(AuthError::Configuration(
                "credential directory must be absolute",
            ));
        }

        Ok(Self { directory })
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub async fn snapshot(&self, name: &str) -> Result<crate::CredentialSnapshot> {
        let _lock = self.lock(name).await?;
        Ok(crate::CredentialSnapshot(self.load(name)?))
    }

    pub async fn import_snapshot(
        &self,
        name: &str,
        snapshot: &crate::CredentialSnapshot,
    ) -> Result<()> {
        if snapshot.0.version != 1 || snapshot.0.access.0.is_empty() {
            return Err(AuthError::Storage);
        }
        self.save(name, &snapshot.0).await
    }

    pub async fn set_api_key(&self, name: &str, key: String, base_url: &str) -> Result<()> {
        self.save(name, &api_key_credential(key, base_url)?)
            .await
    }

    /// Replace a key under its lock and retain the old credential if the caller's commit fails.
    /// The commit is synchronous so cancellation cannot interrupt the two writes. This is not
    /// a crash-atomic transaction across the credential store and the caller's storage.
    pub async fn commit_api_key<T, E>(
        &self,
        name: &str,
        key: String,
        base_url: &str,
        commit: impl FnOnce() -> std::result::Result<T, E>,
    ) -> std::result::Result<T, crate::CredentialCommitError<E>> {
        use crate::CredentialCommitError;
        let credential =
            api_key_credential(key, base_url).map_err(CredentialCommitError::Authentication)?;
        let _lock = self
            .lock(name)
            .await
            .map_err(CredentialCommitError::Authentication)?;
        let previous = match self.load(name) {
            Ok(value) => Some(value),
            Err(AuthError::Missing) => None,
            Err(error) => return Err(CredentialCommitError::Authentication(error)),
        };
        if let Err(error) = self.write(name, &credential) {
            let rollback = self
                .restore(name, previous.as_ref())
                .err();
            return Err(CredentialCommitError::Write { error, rollback });
        }
        match commit() {
            Ok(value) => Ok(value),
            Err(error) => Err(CredentialCommitError::Commit {
                error,
                rollback: self
                    .restore(name, previous.as_ref())
                    .err(),
            }),
        }
    }

    fn restore(&self, name: &str, previous: Option<&Credential>) -> Result<()> {
        if let Some(previous) = previous {
            return self.write(name, previous);
        }
        match fs::remove_file(self.path(name)) {
            Ok(()) => self.sync_directory(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub async fn remove(&self, name: &str) -> Result<bool> {
        let _lock = self.lock(name).await?;
        match fs::remove_file(self.path(name)) {
            Ok(()) => {
                self.sync_directory()?;
                Ok(true)
            }

            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }

    pub fn list(&self) -> Result<Vec<CredentialInfo>> {
        if !self.directory.try_exists()? {
            return Ok(Vec::new());
        }
        self.validate_directory()?;
        let mut entries = Vec::new();

        for entry in fs::read_dir(&self.directory)? {
            let path = entry?.path();
            if path
                .extension()
                .is_none_or(|extension| extension != "json")
            {
                continue;
            }
            let name = path
                .file_stem()
                .and_then(|name| name.to_str())
                .ok_or(AuthError::Storage)?;
            validate_name(name)?;
            match self.load(name) {
                Ok(credential) => entries.push(credential.info(name.to_owned())),
                Err(AuthError::Missing) => continue,
                Err(error) => return Err(error),
            }
        }

        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(entries)
    }

    /// The file lock covers refresh and persistence, including across processes.
    /// Dropping this future releases the lock and cancels the HTTP request.
    pub async fn resolve(
        &self,
        name: &str,
        endpoint: &Url,
        target: AuthTarget,
    ) -> Result<RequestAuth> {
        let _lock = self.lock(name).await?;
        let mut credential = self.load(name)?;
        let auth = credential.request_auth(endpoint, target)?;

        if credential.kind != CredentialKind::ApiKey
            && credential
                .expires_at
                .is_none_or(|expires| expires <= now().saturating_add(60))
        {
            credential = match credential.kind {
                CredentialKind::AnthropicOAuth => crate::anthropic::refresh(&credential).await?,
                CredentialKind::AntigravityOAuth => {
                    crate::antigravity::refresh(&credential).await?
                }
                CredentialKind::GoogleOAuth => crate::google::refresh(&credential).await?,
                CredentialKind::ChatgptOAuth => crate::chatgpt::refresh(&credential).await?,
                CredentialKind::ApiKey => unreachable!(),
            };
            self.write(name, &credential)?;
            return credential.request_auth(endpoint, target);
        }

        Ok(auth)
    }

    pub(crate) async fn save(&self, name: &str, credential: &Credential) -> Result<()> {
        let _lock = self.lock(name).await?;
        self.write(name, credential)
    }

    pub async fn resolve_usage(&self, name: &str) -> Result<Option<crate::UsageAuthorization>> {
        let snapshot = self.snapshot(name).await?;
        let credential = &snapshot.0;
        let Some((provider, target)) =
            crate::UsageProvider::select(credential.kind, &credential.endpoint)?
        else {
            return Ok(None);
        };
        let auth = self
            .resolve(name, &endpoint_scope(&credential.endpoint)?, target)
            .await?;
        if auth.kind != credential.kind {
            return Err(AuthError::Scope);
        }
        Ok(Some(crate::UsageAuthorization {
            provider,
            account: credential
                .account
                .clone()
                .unwrap_or_else(|| name.into()),
            auth,
        }))
    }

    fn path(&self, name: &str) -> PathBuf {
        self.directory
            .join(format!("{name}.json"))
    }

    async fn lock(&self, name: &str) -> Result<File> {
        validate_name(name)?;
        self.create_directory()?;
        let file = private_file(
            &self
                .directory
                .join(format!("{name}.lock")),
            true,
        )?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(45);

        loop {
            match file.try_lock() {
                Ok(()) => return Ok(file),
                Err(std::fs::TryLockError::WouldBlock) => {
                    if tokio::time::Instant::now() >= deadline {
                        return Err(AuthError::Timeout);
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }

                Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
            }
        }
    }

    fn load(&self, name: &str) -> Result<Credential> {
        let file = match private_file(&self.path(name), false) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(AuthError::Missing);
            }
            Err(error) => return Err(error.into()),
        };
        let mut content = Vec::new();
        file.take(65_537)
            .read_to_end(&mut content)?;
        if content.len() > 65_536 {
            return Err(AuthError::Storage);
        }
        let credential: Credential =
            serde_json::from_slice(&content).map_err(|_| AuthError::Storage)?;
        if credential.version != 1 || credential.access.0.is_empty() {
            return Err(AuthError::Storage);
        }

        Ok(credential)
    }

    fn write(&self, name: &str, credential: &Credential) -> Result<()> {
        let mut temporary = tempfile::NamedTempFile::new_in(&self.directory)?;
        serde_json::to_writer(&mut temporary, credential).map_err(|_| AuthError::Storage)?;
        temporary.write_all(b"\n")?;
        temporary.as_file().sync_all()?;
        temporary
            .persist(self.path(name))
            .map_err(|error| error.error)?;
        self.sync_directory()
    }

    fn create_directory(&self) -> Result<()> {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&self.directory)?;
        self.validate_directory()
    }

    fn validate_directory(&self) -> Result<()> {
        let metadata = fs::symlink_metadata(&self.directory)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(AuthError::Configuration(
                "credential directory must not be a symlink",
            ));
        }

        validate_owner(&metadata)?;
        Ok(())
    }

    fn sync_directory(&self) -> Result<()> {
        #[cfg(unix)]
        File::open(&self.directory)?.sync_all()?;
        Ok(())
    }
}

fn api_key_credential(key: String, base_url: &str) -> Result<Credential> {
    let endpoint = endpoint_scope(base_url)?;
    if key.trim().is_empty() || key.len() > 16_384 || key.chars().any(char::is_control) {
        return Err(AuthError::Configuration(
            "API key must contain 1–16384 bytes and no control characters",
        ));
    }
    Ok(Credential {
        version: 1,
        kind: CredentialKind::ApiKey,
        endpoint: endpoint.to_string(),
        access: Secret(key),
        refresh: None,
        expires_at: None,
        account: None,
        account_id: None,
        project_id: None,
    })
}

fn private_file(path: &Path, create: bool) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(create)
        .create(create)
        .truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::other(
            "credential path must be a regular file",
        ));
    }

    validate_owner(&metadata)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(std::io::Error::other(
                "credential path must not be hard linked",
            ));
        }
    }
    Ok(file)
}

fn validate_owner(metadata: &fs::Metadata) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        // getuid has no preconditions and does not modify process state.
        if metadata.uid() != unsafe { libc::getuid() } || metadata.mode() & 0o077 != 0 {
            return Err(std::io::Error::other(
                "credential storage must be user-owned and private (directory 0700, file 0600)",
            ));
        }
    }

    #[cfg(not(unix))]
    let _ = metadata;
    Ok(())
}

pub(crate) fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_'))
    {
        return Err(AuthError::Configuration(
            "credential name must contain 1–64 ASCII letters, digits, '-' or '_'",
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn keys_are_scoped_rotated_and_removed_without_exposing_secrets() {
        let root = tempfile::tempdir().unwrap();
        let store = AuthStore::new(root.path().join("auth")).unwrap();
        let endpoint = Url::parse("https://example.com/v1/messages").unwrap();
        store
            .set_api_key("test", "first-secret".into(), "https://example.com/v1")
            .await
            .unwrap();

        let auth = store
            .resolve("test", &endpoint, AuthTarget::Anthropic)
            .await
            .unwrap();

        assert_eq!(auth.headers["x-api-key"], "first-secret");
        assert!(!format!("{auth:?}").contains("first-secret"));
        assert!(
            !serde_json::to_string(&store.list().unwrap())
                .unwrap()
                .contains("first-secret")
        );
        for url in [
            "https://other.example/v1/messages",
            "http://example.com/v1/messages",
            "https://example.com/v10/messages",
        ] {
            assert!(matches!(
                store
                    .resolve("test", &Url::parse(url).unwrap(), AuthTarget::Openai)
                    .await,
                Err(AuthError::Scope)
            ));
        }

        store
            .set_api_key("test", "second-secret".into(), "https://example.com/v1")
            .await
            .unwrap();
        let updated = store
            .resolve("test", &endpoint, AuthTarget::Openai)
            .await
            .unwrap();

        assert_eq!(updated.headers["authorization"], "Bearer second-secret");
        assert!(store.remove("test").await.unwrap());
        assert!(matches!(
            store
                .resolve("test", &endpoint, AuthTarget::Openai)
                .await,
            Err(AuthError::Missing)
        ));
        assert!(!store.remove("test").await.unwrap());
    }

    #[tokio::test]
    async fn independent_handles_serialize_updates_and_cancelled_waits_release_resources() {
        let root = tempfile::tempdir().unwrap();
        let store = AuthStore::new(root.path().join("auth")).unwrap();
        let held = store.lock("test").await.unwrap();
        let second = AuthStore::new(store.directory()).unwrap();

        let blocked = tokio::time::timeout(
            Duration::from_millis(70),
            second.set_api_key("test", "secret".into(), "https://example.com/v1"),
        )
        .await;

        assert!(blocked.is_err());
        assert!(store.list().unwrap().is_empty());
        drop(held);

        second
            .set_api_key("test", "secret".into(), "https://example.com/v1")
            .await
            .unwrap();

        assert_eq!(store.list().unwrap().len(), 1);
        assert!(
            store
                .set_api_key("../escape", "secret".into(), "https://example.com")
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn failed_external_commit_restores_existing_key_and_removes_uncommitted_key() {
        let root = tempfile::tempdir().unwrap();
        let store = AuthStore::new(root.path().join("auth")).unwrap();
        store
            .set_api_key(
                "existing",
                "original-key".into(),
                "https://sample.invalid/v1",
            )
            .await
            .unwrap();
        for name in ["existing", "new"] {
            let result = store
                .commit_api_key(
                    name,
                    "replacement-key".into(),
                    "https://sample.invalid/v1",
                    || Err::<(), _>(std::io::Error::other("synthetic commit conflict")),
                )
                .await;
            assert!(matches!(
                result,
                Err(crate::CredentialCommitError::Commit { rollback: None, .. })
            ));
        }
        assert_eq!(store.load("existing").unwrap().access.0, "original-key");
        assert!(matches!(store.load("new"), Err(AuthError::Missing)));
        let value = store
            .commit_api_key(
                "existing",
                "committed-key".into(),
                "https://sample.invalid/v1",
                || Ok::<_, std::io::Error>(42),
            )
            .await
            .unwrap();
        assert_eq!(value, 42);
        assert_eq!(store.load("existing").unwrap().access.0, "committed-key");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn credentials_reject_public_permissions_and_linked_files() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let root = tempfile::tempdir().unwrap();
        let store = AuthStore::new(root.path().join("auth")).unwrap();
        store
            .set_api_key("test", "secret".into(), "https://example.com")
            .await
            .unwrap();
        let path = store.path("test");

        assert_eq!(
            fs::metadata(store.directory())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(store.load("test").is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        fs::hard_link(&path, root.path().join("linked")).unwrap();
        assert!(store.load("test").is_err());
        fs::remove_file(&path).unwrap();
        symlink(root.path().join("linked"), &path).unwrap();
        assert!(store.load("test").is_err());
    }
}
