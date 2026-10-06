use super::Local;
use std::{
    path::PathBuf,
    time::{Duration, Instant, SystemTime},
};

const CHECK: Duration = Duration::from_secs(1);
const REFRESH: Duration = Duration::from_secs(900);
type Stamp = Vec<(PathBuf, Option<(SystemTime, u64)>)>;

/// Tracks the client's configuration and credential files without reading secrets.
/// A refresh result is usable only while these files still match its starting stamp.
pub struct Watcher {
    local: Local,
    stamp: Stamp,
    checked: Instant,
    refreshed: Instant,
}

fn stamp(local: &Local) -> Stamp {
    let mut paths = vec![local.config.clone(), local.auth.clone()];
    if let Ok(entries) = std::fs::read_dir(&local.auth) {
        paths.extend(entries.filter_map(Result::ok).map(|entry| entry.path()));
    }
    paths.sort();
    paths
        .into_iter()
        .map(|path| {
            let metadata = std::fs::metadata(&path)
                .ok()
                .and_then(|m| Some((m.modified().ok()?, m.len())));
            (path, metadata)
        })
        .collect()
}

impl Watcher {
    pub fn new(local: Local) -> Self {
        let stamp = stamp(&local);
        Self {
            local,
            stamp,
            checked: Instant::now(),
            refreshed: Instant::now(),
        }
    }
    pub fn due(&mut self) -> bool {
        if self.checked.elapsed() < CHECK {
            return false;
        }
        self.checked = Instant::now();
        let current = stamp(&self.local);
        if current == self.stamp && self.refreshed.elapsed() < REFRESH {
            return false;
        }
        self.resynchronize();
        true
    }
    pub fn unchanged(&self) -> bool {
        self.stamp == stamp(&self.local)
    }
    pub fn resynchronize(&mut self) {
        self.stamp = stamp(&self.local);
        self.checked = Instant::now();
        self.refreshed = Instant::now();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn watcher_detects_edits_credentials_and_stale_results_and_refreshes_periodically() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.toml");
        let auth = directory.path().join("auth");
        std::fs::write(&config, "first").unwrap();
        let mut watcher = Watcher::new(Local {
            config: config.clone(),
            auth: auth.clone(),
            model: None,
        });
        assert!(!watcher.due());
        std::fs::write(config, "second configuration").unwrap();
        assert!(!watcher.unchanged());
        watcher.checked -= CHECK;
        assert!(watcher.due());
        assert!(watcher.unchanged());
        watcher.checked -= CHECK;
        assert!(!watcher.due());
        std::fs::create_dir(&auth).unwrap();
        std::fs::write(auth.join("test.json"), "credential-placeholder").unwrap();
        assert!(!watcher.unchanged());
        watcher.checked -= CHECK;
        assert!(watcher.due());
        watcher.checked -= CHECK;
        watcher.refreshed -= REFRESH;
        assert!(watcher.due());
        assert!(watcher.unchanged());
    }
}
