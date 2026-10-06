//! Window-local tabs and an immutable restore snapshot for each group of live TUIs.
//! OS leases detect crashed clients; one registry lock serializes joins and exits.
use std::{
    collections::BTreeMap,
    fs::File,
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::store::{Store, private_file};

#[derive(Default, Serialize, Deserialize)]
struct Registry {
    previous: Vec<String>,
    windows: BTreeMap<String, Window>,
}

#[derive(Default, Serialize, Deserialize)]
struct Window {
    tabs: Vec<String>,
    closed: bool,
}

pub struct TuiWindow {
    directory: PathBuf,
    id: String,
    lease: Option<File>,
    previous: Vec<String>,
}

fn read_registry(directory: &Path, legacy: &[String]) -> Result<Registry> {
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
    std::fs::create_dir_all(directory)?;
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

    if !registry.windows.is_empty() && registry.windows.values().all(|window| window.closed) {
        let mut seen = std::collections::HashSet::new();
        registry.previous = registry
            .windows
            .values()
            .flat_map(|window| window.tabs.iter())
            .filter(|id| seen.insert((*id).clone()))
            .cloned()
            .collect();
        for id in registry.windows.keys() {
            let _ = std::fs::remove_file(directory.join(id));
        }
        registry.windows.clear();
    }

    Ok(())
}

impl Store {
    pub async fn previous_tui_tabs(&self) -> Result<Vec<String>> {
        let directory = self.path().with_extension("tui-windows");
        let legacy = self.all_tui_tabs().await?;
        tokio::task::spawn_blocking(move || {
            let _lock = registry_lock(&directory)?;
            let mut registry = read_registry(&directory, &legacy)?;
            reap(&directory, &mut registry)?;
            save(&directory, &registry)?;
            Ok(registry.previous)
        })
        .await?
    }

    pub async fn open_tui_window(&self) -> Result<TuiWindow> {
        let directory = self.path().with_extension("tui-windows");
        let legacy = self.all_tui_tabs().await?;
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
            registry.windows.insert(id.clone(), Window::default());
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
    pub fn previous(&self) -> &[String] {
        &self.previous
    }

    pub async fn update(&self, tabs: Vec<String>) -> Result<()> {
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
    #[tokio::test]
    async fn windows_merge_at_last_exit_and_next_generation_replaces_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("sessions.db")).await.unwrap();
        let a = store.open_tui_window().await.unwrap();
        let b = store.open_tui_window().await.unwrap();
        a.update(vec!["one".into(), "shared".into()]).await.unwrap();
        b.update(vec!["shared".into(), "two".into()]).await.unwrap();
        a.close().await.unwrap();
        let c = store.open_tui_window().await.unwrap();
        assert!(c.previous().is_empty());
        c.close().await.unwrap();
        b.close().await.unwrap();
        let next = store.open_tui_window().await.unwrap();
        assert_eq!(next.previous(), ["one", "shared", "two"]);
        next.update(vec!["two".into()]).await.unwrap();
        next.close().await.unwrap();
        let last = store.open_tui_window().await.unwrap();
        assert_eq!(last.previous(), ["two"]);
        last.close().await.unwrap();
        assert!(store.open_tui_window().await.unwrap().previous().is_empty());
    }

    #[tokio::test]
    async fn abandoned_window_recovers_latest_tabs_when_next_client_joins() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("sessions.db")).await.unwrap();
        let mut dead = store.open_tui_window().await.unwrap();
        dead.update(vec!["saved".into()]).await.unwrap();
        dead.lease.take(); // simulate process death without orderly close
        drop(dead);
        let next = store.open_tui_window().await.unwrap();
        assert_eq!(next.previous(), ["saved"]);
    }
}
