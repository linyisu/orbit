//! Client window layout and reconnect metadata. Session history stays on its target.
//! OS leases detect crashed clients; one registry lock serializes joins and exits.
use std::{
    collections::BTreeMap,
    fs::File,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::transport::{Connection, Target};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SavedTab {
    pub id: String,
    pub title: String,
    pub target: Target,
    pub workspace: String,
    pub config: PathBuf,
    pub auth: PathBuf,
    pub database: Option<PathBuf>,
    pub allow_mcp_start: bool,
}

impl SavedTab {
    pub fn from_connection(connection: &Connection, id: String, title: String) -> Option<Self> {
        let local = connection
            .local_configuration
            .as_ref()?;
        Some(Self {
            id,
            title,
            target: connection.target.clone(),
            workspace: connection.workspace.clone(),
            config: local.config.clone(),
            auth: local.auth.clone(),
            database: connection.database.clone(),
            allow_mcp_start: connection.allow_mcp_start,
        })
    }
}

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

#[derive(Default, Serialize, Deserialize)]
struct Registry {
    previous: Vec<SavedTab>,
    windows: BTreeMap<String, Window>,
}

#[derive(Default, Serialize, Deserialize)]
struct Window {
    tabs: Vec<SavedTab>,
    closed: bool,
}

pub struct TuiWindow {
    directory: PathBuf,
    id: String,
    lease: Option<File>,
    previous: Vec<SavedTab>,
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
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(directory)?;
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
        let mut seen = std::collections::HashSet::new();
        registry.previous = registry
            .windows
            .values()
            .flat_map(|window| window.tabs.iter())
            .filter(|tab| seen.insert((tab.target.clone(), tab.id.clone())))
            .cloned()
            .collect();
        for id in registry.windows.keys() {
            let _ = std::fs::remove_file(directory.join(id));
        }
        registry.windows.clear();
    }

    Ok(())
}

impl TuiWindow {
    pub async fn open(directory: PathBuf) -> Result<Self> {
        Self::open_with_previous(directory, Vec::new()).await
    }

    pub async fn open_for_connection(
        directory: PathBuf,
        client: &mut super::transport::Client,
        connection: &Connection,
    ) -> Result<Self> {
        let mut previous = Vec::new();
        if !directory.join("state.json").exists()
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
        Self::open_with_previous(directory, previous).await
    }

    async fn open_with_previous(directory: PathBuf, legacy: Vec<SavedTab>) -> Result<Self> {
        tokio::task::spawn_blocking(move || {
            let _lock = registry_lock(&directory)?;
            let mut registry = read_registry(&directory, &legacy)?;
            reap(&directory, &mut registry)?;
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
            let previous = registry.previous.clone();
            registry
                .windows
                .insert(id.clone(), Window::default());
            save(&directory, &registry)?;
            Ok(TuiWindow {
                directory,
                id,
                lease: Some(lease),
                previous,
            })
        })
        .await?
    }
}

impl TuiWindow {
    pub fn previous(&self) -> &[SavedTab] {
        &self.previous
    }

    pub async fn update(&self, tabs: Vec<SavedTab>) -> Result<()> {
        let directory = self.directory.clone();
        let id = self.id.clone();
        tokio::task::spawn_blocking(move || {
            let _lock = registry_lock(&directory)?;
            let mut registry = read_registry(&directory, &[])?;
            if let Some(window) = registry.windows.get_mut(&id) {
                window.tabs = tabs;
            }
            reap(&directory, &mut registry)?;
            save(&directory, &registry)
        })
        .await?
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
        save(&self.directory, &registry)
    }

    pub async fn close(mut self) -> Result<()> {
        tokio::task::spawn_blocking(move || self.finish()).await?
    }
}

impl Drop for TuiWindow {
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(target: Target, id: &str) -> SavedTab {
        SavedTab {
            id: id.into(),
            title: id.into(),
            target,
            workspace: "/project".into(),
            config: "/config.toml".into(),
            auth: "/auth".into(),
            database: None,
            allow_mcp_start: false,
        }
    }

    #[tokio::test]
    async fn last_window_merges_targets_and_crashed_window_records() {
        let root = tempfile::tempdir().unwrap();
        let first = TuiWindow::open(root.path().into())
            .await
            .unwrap();
        let mut crashed = TuiWindow::open(root.path().into())
            .await
            .unwrap();
        let local = tab(Target::Local, "same-id");
        let ssh = tab(
            Target::Ssh {
                host: "dev".into(),
                options: Default::default(),
            },
            "same-id",
        );
        first
            .update(vec![local.clone()])
            .await
            .unwrap();
        crashed
            .update(vec![ssh.clone(), local.clone()])
            .await
            .unwrap();
        crashed.lease.take();
        drop(crashed);
        first.close().await.unwrap();
        let restored = TuiWindow::open(root.path().into())
            .await
            .unwrap();
        assert_eq!(restored.previous().len(), 2);
        assert!(restored.previous().contains(&local));
        assert!(restored.previous().contains(&ssh));
        restored.close().await.unwrap();
    }

    #[tokio::test]
    async fn live_windows_do_not_replace_previous_restore_snapshot() {
        let root = tempfile::tempdir().unwrap();
        let previous = tab(Target::Local, "previous");
        let first = TuiWindow::open(root.path().into())
            .await
            .unwrap();
        first
            .update(vec![previous.clone()])
            .await
            .unwrap();
        first.close().await.unwrap();
        let second = TuiWindow::open(root.path().into())
            .await
            .unwrap();
        second
            .update(vec![tab(Target::Local, "running")])
            .await
            .unwrap();
        let third = TuiWindow::open(root.path().into())
            .await
            .unwrap();
        assert_eq!(third.previous(), [previous]);
        third.close().await.unwrap();
        second.close().await.unwrap();
    }

    #[tokio::test]
    async fn legacy_tabs_seed_only_the_first_client_registry() {
        let root = tempfile::tempdir().unwrap();
        let legacy = tab(Target::Local, "legacy");
        let migrated = TuiWindow::open_with_previous(root.path().into(), vec![legacy.clone()])
            .await
            .unwrap();
        assert_eq!(migrated.previous(), [legacy]);
        let current = tab(Target::Local, "current");
        migrated
            .update(vec![current.clone()])
            .await
            .unwrap();
        migrated.close().await.unwrap();
        let next =
            TuiWindow::open_with_previous(root.path().into(), vec![tab(Target::Local, "stale")])
                .await
                .unwrap();
        assert_eq!(next.previous(), [current]);
        next.close().await.unwrap();
    }
}
