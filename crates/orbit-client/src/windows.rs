//! Client window layout and reconnect metadata. Session history stays on its target.
//! OS leases detect crashed clients; one registry lock serializes joins and exits.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::transport::{Connection, Target};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedTab {
    /// Persisted session identity, or None for a tab that has not sent its first message.
    pub id: Option<String>,
    /// Stable view identity; legacy records may have no key until first restored.
    #[serde(default)]
    pub key: String,
    #[serde(default)]
    pub draft: Draft,
    pub title: String,
    pub target: Target,
    pub workspace: String,
    pub config: PathBuf,
    pub auth: PathBuf,
    pub database: Option<PathBuf>,
    pub allow_mcp_start: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
}

impl SavedTab {
    pub fn from_connection(connection: &Connection, id: String, title: String) -> Option<Self> {
        let local = connection
            .local_configuration
            .as_ref()?;
        Some(Self {
            id: Some(id),
            key: String::new(),
            draft: Draft::default(),
            title,
            target: connection.target.clone(),
            workspace: connection.workspace.clone(),
            config: local.config.clone(),
            auth: local.auth.clone(),
            database: connection.database.clone(),
            allow_mcp_start: false,
            server: None,
        })
    }

    /// Session IDs are scoped to a transport and data source, not globally unique.
    pub fn same_session(&self, other: &Self) -> bool {
        self.id.is_some() && self.id == other.id && self.source().matches(&other.source())
    }

    fn source(&self) -> super::transport::DataSource<'_> {
        super::transport::DataSource {
            target: self.target.clone(),
            server: self.server.as_deref(),
            config: &self.config,
            auth: Some(&self.auth),
            database: self.database.as_deref(),
            workspace: Path::new(&self.workspace),
        }
    }
}

/// Unsent composer material. Assets are copied into the profile before a snapshot is committed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Draft {
    pub text: String,
    pub attachments: Vec<DraftAttachment>,
    #[serde(skip)]
    pub leases: Vec<orbit_model::types::Attachment>,
}

impl Draft {
    pub fn is_empty(&self) -> bool {
        self.text.is_empty() && self.attachments.is_empty()
    }

    /// Rebuild local attachment handles without importing, sending or reading file content.
    pub fn attachment_handles(&self) -> Vec<orbit_model::types::Attachment> {
        self.attachments
            .iter()
            .map(|asset| {
                if crate::attachments::is_image_path(&asset.path) {
                    orbit_model::types::Attachment::Image {
                        name: asset.name.clone(),
                        mime_type: match asset
                            .path
                            .extension()
                            .and_then(|value| value.to_str())
                            .unwrap_or("")
                            .to_ascii_lowercase()
                            .as_str()
                        {
                            "jpg" | "jpeg" => "image/jpeg",
                            "gif" => "image/gif",
                            "webp" => "image/webp",
                            _ => "image/png",
                        }
                        .into(),
                        path: asset.path.clone(),
                        lease: None,
                    }
                } else {
                    orbit_model::types::Attachment::File {
                        name: asset.name.clone(),
                        path: asset.path.clone(),
                        text: None,
                        lease: None,
                    }
                }
            })
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DraftAttachment {
    pub name: String,
    pub path: PathBuf,
}

/// Compatibility name for callers predating shared desktop restoration.
pub type TuiWindow = ClientWindow;

/// Configuration and database overrides identify a local client launch profile;
/// workspace directories and transports belong to its individual tabs.
pub struct Profile {
    pub config: PathBuf,
    pub auth: PathBuf,
    pub database: Option<PathBuf>,
}

impl Profile {
    pub fn from_connection(connection: &Connection) -> Result<Self> {
        let local = connection
            .local_configuration
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("本地连接配置缺失"))?;
        Ok(Self {
            config: local.config.clone(),
            auth: local.auth.clone(),
            database: connection.database.clone(),
        })
    }

    fn legacy_directory(&self) -> PathBuf {
        self.auth
            .parent()
            .unwrap_or(&self.auth)
            .join("windows")
    }

    pub fn directory(&self) -> PathBuf {
        use sha2::{Digest, Sha256};
        let identity = serde_json::to_vec(&(&self.config, &self.auth, &self.database))
            .expect("serializable profile paths");
        let key: String = Sha256::digest(identity)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        self.legacy_directory()
            .join("profiles")
            .join(key)
    }
}

/// A pre-shared client snapshot. Its modification time only participates in first migration.
pub struct LegacyTabs {
    pub source: PathBuf,
    pub tabs: Vec<SavedTab>,
    pub active: Option<usize>,
}

#[cfg(unix)]
fn private_file(path: &Path) -> Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options
        .create(true)
        .truncate(false)
        .read(true)
        .write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

#[cfg(windows)]
fn private_file(path: &Path) -> Result<File> {
    Ok(orbit_runtime::filesystem::private::open_file(
        path, true, false,
    )?)
}

#[derive(Default, Serialize, Deserialize)]
struct Registry {
    #[serde(default)]
    revision: u64,
    #[serde(default)]
    imports: BTreeSet<PathBuf>,
    #[serde(default)]
    imported_at: Option<std::time::SystemTime>,
    previous: Vec<SavedTab>,
    #[serde(default)]
    previous_active: Option<usize>,
    windows: BTreeMap<String, Window>,
}

#[derive(Default, Serialize, Deserialize)]
struct Window {
    #[serde(default)]
    revision: u64,
    tabs: Vec<SavedTab>,
    #[serde(default)]
    active: Option<usize>,
    closed: bool,
}

/// A leased client window. All frontends share one restore generation per launch profile.
pub struct ClientWindow {
    directory: PathBuf,
    id: String,
    lease: Option<File>,
    previous: Vec<SavedTab>,
    active: Option<usize>,
}

fn read_registry(directory: &Path, legacy: &[SavedTab]) -> Result<Registry> {
    let path = directory.join("state.json");
    match std::fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Registry {
            previous: legacy.to_vec(),
            ..Default::default()
        }),

        Err(e) => Err(e.into()),
    }
}

fn save(directory: &Path, registry: &Registry) -> Result<()> {
    let mut file = private_file(&directory.join("state.tmp"))?;
    file.set_len(0)?;
    file.write_all(&serde_json::to_vec(registry)?)?;
    file.sync_all()?;
    std::fs::rename(directory.join("state.tmp"), directory.join("state.json"))?;
    Ok(())
}

fn registry_lock(directory: &Path) -> Result<File> {
    #[cfg(windows)]
    orbit_runtime::filesystem::private::create_directory_all(directory)?;
    #[cfg(unix)]
    {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(directory)?;
    }
    let lock = private_file(&directory.join("registry.lock"))?;
    lock.lock()?;
    Ok(lock)
}

fn reap(directory: &Path, registry: &mut Registry) -> Result<()> {
    for (id, window) in &mut registry.windows {
        if window.closed {
            continue;
        }
        let probe = private_file(&directory.join(id))?;
        match probe.try_lock() {
            Ok(()) => window.closed = true,
            Err(std::fs::TryLockError::WouldBlock) => {}
            Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
        }
    }

    if !registry.windows.is_empty()
        && registry
            .windows
            .values()
            .all(|window| window.closed)
    {
        (registry.previous, registry.previous_active) = merged_windows(registry);
        for id in registry.windows.keys() {
            let _ = std::fs::remove_file(directory.join(id));
        }
        registry.windows.clear();
    }

    Ok(())
}

/// Union current window snapshots in save order without dropping independent drafts.
fn merged_windows(registry: &Registry) -> (Vec<SavedTab>, Option<usize>) {
    let mut windows: Vec<_> = registry.windows.iter().collect();
    windows.sort_by_key(|(id, window)| (window.revision, *id));
    let mut tabs: Vec<SavedTab> = Vec::new();
    let mut active = None;
    for (id, window) in windows {
        for (index, tab) in window.tabs.iter().enumerate() {
            let existing = tabs.iter().position(|other| {
                (tab.same_session(other) || (!tab.key.is_empty() && tab.key == other.key))
                    && tab.draft == other.draft
            });
            let restored = existing.unwrap_or_else(|| {
                let mut tab = tab.clone();
                if !tab.key.is_empty()
                    && tabs
                        .iter()
                        .any(|other| other.key == tab.key)
                {
                    tab.key = format!("{id}:{}", tab.key);
                }
                tabs.push(tab);
                tabs.len() - 1
            });
            if window.active == Some(index) {
                active = Some(restored);
            }
        }
    }
    (tabs, active)
}

impl ClientWindow {
    pub async fn open(directory: PathBuf) -> Result<Self> {
        Self::open_with_previous(directory, Vec::new()).await
    }

    /// Join a profile using the same local restore sources as the desktop client.
    pub async fn for_connection(
        client: &mut super::transport::Client,
        connection: &Connection,
    ) -> Result<Self> {
        let profile = Profile::from_connection(connection)?;
        Self::open_for_connection(profile.legacy_directory(), client, connection).await
    }

    pub async fn open_for_connection(
        directory: PathBuf,
        client: &mut super::transport::Client,
        connection: &Connection,
    ) -> Result<Self> {
        let profile = Profile::from_connection(connection)?;
        let mut previous = Vec::new();
        if !profile
            .directory()
            .join("state.json")
            .exists()
            && !directory.join("state.json").exists()
            && matches!(connection.target, Target::Local)
            && let super::protocol::Response::Sessions(sessions) = client
                .request(super::protocol::Request::RestoreSessions)
                .await?
        {
            for session in sessions {
                let mut origin = connection.clone();
                origin.workspace = session.workspace;
                if let Some(tab) = SavedTab::from_connection(&origin, session.id, session.title) {
                    previous.push(tab);
                }
            }
        }
        tokio::task::spawn_blocking(move || {
            let desktop = legacy_desktop(&desktop_state_path(None)?, &profile)?;
            Self::open_profile_seeded(&profile, desktop, previous)
        })
        .await?
    }

    async fn open_with_previous(directory: PathBuf, legacy: Vec<SavedTab>) -> Result<Self> {
        tokio::task::spawn_blocking(move || Self::open_registry(directory, legacy, Vec::new()))
            .await?
    }

    /// Open the shared profile without network access. Legacy imports are considered once;
    /// the newest saved source wins only before a live generation starts.
    pub fn open_profile(profile: &Profile, legacy: Option<LegacyTabs>) -> Result<Self> {
        Self::open_profile_seeded(profile, legacy, Vec::new())
    }

    fn open_profile_seeded(
        profile: &Profile,
        legacy: Option<LegacyTabs>,
        seed: Vec<SavedTab>,
    ) -> Result<Self> {
        let legacy_directory = profile.legacy_directory();
        let mut imports = Vec::new();
        if legacy_directory
            .join("state.json")
            .exists()
        {
            let mut registry = read_registry(&legacy_directory, &[])?;
            reap(&legacy_directory, &mut registry)?;
            imports.push(LegacyTabs {
                source: legacy_directory.join("state.json"),
                active: registry.previous_active,
                tabs: registry
                    .previous
                    .into_iter()
                    .filter(|tab| tab.config == profile.config && tab.database == profile.database)
                    .collect(),
            });
        }
        imports.extend(legacy);
        Self::open_registry(profile.directory(), seed, imports)
    }

    fn open_registry(
        directory: PathBuf,
        legacy: Vec<SavedTab>,
        mut imports: Vec<LegacyTabs>,
    ) -> Result<Self> {
        let _lock = registry_lock(&directory)?;
        let mut modified = std::fs::metadata(directory.join("state.json"))
            .and_then(|metadata| metadata.modified())
            .ok();
        let mut registry = read_registry(&directory, &legacy)?;
        if let Some(imported) = registry.imported_at {
            modified = Some(imported);
        }
        reap(&directory, &mut registry)?;
        imports.sort_by_key(|import| {
            std::fs::metadata(&import.source)
                .and_then(|metadata| metadata.modified())
                .ok()
        });
        for import in imports {
            if registry
                .imports
                .insert(import.source.clone())
            {
                let imported = std::fs::metadata(&import.source)
                    .and_then(|metadata| metadata.modified())
                    .ok();
                if registry.windows.is_empty() && imported.is_some() && imported >= modified {
                    registry.previous = import.tabs;
                    registry.previous_active = import.active;
                    registry.imported_at = imported;
                    modified = imported;
                }
            }
        }
        static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let id = format!(
            "{:032x}-{}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos(),
            std::process::id(),
            SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let lease = private_file(&directory.join(&id))?;
        lease.lock()?;
        let (previous, active) = if registry.windows.is_empty() {
            (registry.previous.clone(), registry.previous_active)
        } else {
            merged_windows(&registry)
        };
        registry.windows.insert(
            id.clone(),
            Window {
                tabs: previous.clone(),
                active,
                closed: false,
                revision: registry.revision,
            },
        );
        save(&directory, &registry)?;
        Ok(Self {
            directory,
            id,
            lease: Some(lease),
            previous,
            active,
        })
    }
}

impl ClientWindow {
    /// Restore candidates captured at join time, including snapshots from other live windows.
    pub fn previous(&self) -> &[SavedTab] {
        &self.previous
    }

    pub fn active(&self) -> Option<usize> {
        self.active
    }

    pub async fn update(&self, tabs: Vec<SavedTab>) -> Result<()> {
        let directory = self.directory.clone();
        let id = self.id.clone();
        tokio::task::spawn_blocking(move || Self::save_tabs(&directory, &id, tabs, None)).await?
    }

    /// Persist this window from a caller-owned background writer, without starting a runtime.
    pub fn update_blocking(&self, tabs: Vec<SavedTab>, active: Option<usize>) -> Result<()> {
        Self::save_tabs(&self.directory, &self.id, tabs, active)
    }

    pub async fn update_active(&self, tabs: Vec<SavedTab>, active: Option<usize>) -> Result<()> {
        let directory = self.directory.clone();
        let id = self.id.clone();
        tokio::task::spawn_blocking(move || Self::save_tabs(&directory, &id, tabs, active)).await?
    }

    fn save_tabs(
        directory: &Path,
        id: &str,
        mut tabs: Vec<SavedTab>,
        active: Option<usize>,
    ) -> Result<()> {
        let _lock = registry_lock(directory)?;
        for tab in &mut tabs {
            save_assets(directory, &mut tab.draft)?;
        }
        let mut registry = read_registry(directory, &[])?;
        registry.imported_at = None;
        registry.revision = registry.revision.saturating_add(1);
        if let Some(window) = registry.windows.get_mut(id) {
            window.revision = registry.revision;
            window.active = active.filter(|index| *index < tabs.len());
            window.tabs = tabs;
        }
        reap(directory, &mut registry)?;
        save(directory, &registry)?;
        let _ = collect_assets(directory, &registry);
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        if self.lease.is_none() {
            return Ok(());
        }
        let _lock = registry_lock(&self.directory)?;
        let mut registry = read_registry(&self.directory, &[])?;
        if let Some(window) = registry.windows.get_mut(&self.id) {
            window.closed = true;
        }
        self.lease.take();
        reap(&self.directory, &mut registry)?;
        save(&self.directory, &registry)?;
        let _ = collect_assets(&self.directory, &registry);
        Ok(())
    }

    pub async fn close(mut self) -> Result<()> {
        tokio::task::spawn_blocking(move || self.finish()).await?
    }
}

impl Drop for ClientWindow {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

/// Persist exact local assets; no directory grant or message submission is involved.
fn save_assets(directory: &Path, draft: &mut Draft) -> Result<()> {
    let assets = orbit_runtime::assets::AssetDirectory::for_database(&directory.join("drafts.db"));
    for asset in &mut draft.attachments {
        if asset.path.starts_with(assets.path()) {
            continue;
        }
        let upload = crate::attachments::file_upload(&asset.path)?;
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD.decode(upload.data)?;
        let extension = asset
            .path
            .extension()
            .and_then(|value| value.to_str())
            .filter(|value| {
                value.len() <= orbit_runtime::assets::AssetDirectory::MAX_EXTENSION_BYTES
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric())
            })
            .unwrap_or("bin");
        let (destination, _lease) = assets.import(&bytes, extension)?;
        asset.path = destination;
    }
    draft.leases.clear();
    Ok(())
}

/// Collect only unreferenced profile assets after the registry has committed.
fn collect_assets(directory: &Path, registry: &Registry) -> Result<()> {
    let assets = orbit_runtime::assets::AssetDirectory::for_database(&directory.join("drafts.db"));
    if !assets.path().exists() {
        return Ok(());
    }
    let retained: BTreeSet<_> = registry
        .previous
        .iter()
        .chain(
            registry
                .windows
                .values()
                .flat_map(|window| &window.tabs),
        )
        .flat_map(|tab| &tab.draft.attachments)
        .filter_map(|asset| assets.file_name(&asset.path))
        .collect();
    let lock = assets.lock()?;
    for (name, _) in lock.files()? {
        if !retained.contains(&name) {
            lock.remove_if_unused(&name)?;
        }
    }
    lock.prune_pending()
}

/// Locate view metadata for the desktop without making it a session restore source.
pub fn desktop_state_path(preferences: Option<&Path>) -> Result<PathBuf> {
    legacy::desktop_state_path(preferences)
}

mod legacy;
use legacy::legacy_desktop;
#[cfg(test)]
mod tests;
