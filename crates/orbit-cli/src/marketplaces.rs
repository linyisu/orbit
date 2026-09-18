//! User-owned marketplace catalog storage and safe source acquisition.
//!
//! Marketplaces are never stored in a project directory. Adding one is an
//! explicit user action; Git sources are cloned into Orbit's private data
//! directory, where their catalogue can be inspected before any plugin is
//! installed or executable component enabled.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use tokio::process::Command;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Marketplace {
    pub name: String,
    pub source: String,
    pub directory: PathBuf,
    pub plugins: Vec<Plugin>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Plugin {
    pub name: String,
    pub description: String,
}

#[derive(Default, Serialize, Deserialize)]
struct StoredMarketplaces {
    marketplaces: Vec<Marketplace>,
}

pub(crate) fn list_current() -> Result<Vec<Marketplace>> {
    Store::current()?.list()
}

pub(crate) async fn add_current(source: String) -> Result<Marketplace> {
    Store::current()?.add(source).await
}

struct Store {
    root: PathBuf,
    index: PathBuf,
}

impl Store {
    fn current() -> Result<Self> {
        let root = crate::session::data_directory()?.join("orbit/marketplaces");
        Ok(Self {
            index: root.join("marketplaces.json"),
            root,
        })
    }

    fn at(root: PathBuf) -> Self {
        Self {
            index: root.join("marketplaces.json"),
            root,
        }
    }

    fn list(&self) -> Result<Vec<Marketplace>> {
        if !self.index.exists() {
            return Ok(Vec::new());
        }
        let stored: StoredMarketplaces = serde_json::from_slice(
            &fs::read(&self.index)
                .with_context(|| format!("failed to read {}", self.index.display()))?,
        )
        .with_context(|| format!("failed to parse {}", self.index.display()))?;
        Ok(stored.marketplaces)
    }

    async fn add(&self, source: String) -> Result<Marketplace> {
        let source = source.trim().to_owned();
        ensure!(!source.is_empty(), "marketplace source cannot be empty");
        fs::create_dir_all(&self.root)?;
        let directory = if let Some(path) = local_directory(&source) {
            path.canonicalize().with_context(|| {
                format!("failed to open marketplace directory {}", path.display())
            })?
        } else {
            self.clone_git_source(&source).await?
        };
        let mut market = parse_catalog(&directory)?;
        market.source = source;
        let mut stored = StoredMarketplaces {
            marketplaces: self.list()?,
        };
        stored
            .marketplaces
            .retain(|existing| existing.name != market.name);
        stored.marketplaces.push(market.clone());
        stored
            .marketplaces
            .sort_by(|left, right| left.name.cmp(&right.name));
        let encoded = serde_json::to_vec_pretty(&stored)?;
        fs::write(&self.index, encoded)?;
        Ok(market)
    }

    async fn clone_git_source(&self, source: &str) -> Result<PathBuf> {
        let url = git_url(source)?;
        let target = self
            .root
            .join(format!("source-{:016x}", stable_hash(source)));
        let temporary = self.root.join(format!(
            ".download-{:016x}",
            stable_hash(&format!("{source}:new"))
        ));
        if temporary.exists() {
            fs::remove_dir_all(&temporary)?;
        }
        let output = Command::new("git")
            .args(["clone", "--depth", "1", "--", &url])
            .arg(&temporary)
            .output()
            .await
            .context("failed to start git while downloading marketplace")?;
        if !output.status.success() {
            let _ = fs::remove_dir_all(&temporary);
            bail!(
                "marketplace download failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        // Validate before replacing a previously downloaded private copy. A bad
        // update must leave the last known catalogue untouched.
        if let Err(error) = parse_catalog(&temporary) {
            let _ = fs::remove_dir_all(&temporary);
            return Err(error);
        }
        let backup = self
            .root
            .join(format!(".previous-{:016x}", stable_hash(source)));
        if backup.exists() {
            fs::remove_dir_all(&backup)?;
        }
        let replaced = target.exists();
        if replaced {
            fs::rename(&target, &backup)?;
        }
        if let Err(error) = fs::rename(&temporary, &target) {
            if replaced {
                let _ = fs::rename(&backup, &target);
            }
            return Err(error).context("failed to install downloaded marketplace catalogue");
        }
        if replaced {
            fs::remove_dir_all(backup)?;
        }
        Ok(target)
    }
}

fn local_directory(source: &str) -> Option<PathBuf> {
    let path = PathBuf::from(source);
    path.is_dir().then_some(path)
}

fn git_url(source: &str) -> Result<String> {
    if source.split('/').count() == 2
        && source
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'_' | b'.'))
        && !source.contains("..")
    {
        return Ok(format!("https://github.com/{source}.git"));
    }
    let parsed = url::Url::parse(source).context(
        "marketplace source must be a local directory, GitHub owner/repo, or HTTPS git URL",
    )?;
    if parsed.scheme() != "https"
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
    {
        bail!("marketplace URL must be an HTTPS URL without embedded credentials");
    }
    Ok(source.into())
}

fn parse_catalog(directory: &Path) -> Result<Marketplace> {
    let path = [
        directory.join(".claude-plugin/marketplace.json"),
        directory.join("marketplace.json"),
    ]
    .into_iter()
    .find(|candidate| candidate.is_file())
    .context("marketplace does not contain .claude-plugin/marketplace.json")?;
    let raw: serde_json::Value = serde_json::from_slice(&fs::read(&path)?)
        .with_context(|| format!("invalid marketplace JSON in {}", path.display()))?;
    let name = raw
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim();
    if !valid_name(name) {
        bail!("marketplace name must use letters, digits, _ or - and be at most 64 characters");
    }
    let entries = raw
        .get("plugins")
        .and_then(serde_json::Value::as_array)
        .context("marketplace has no plugins array")?;
    let mut names = BTreeSet::new();
    let mut plugins = Vec::with_capacity(entries.len());
    for (index, entry) in entries.iter().enumerate() {
        let object = entry
            .as_object()
            .with_context(|| format!("plugin entry {} must be an object", index + 1))?;
        let plugin_name = object
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .with_context(|| format!("plugin entry {} has no name", index + 1))?;
        ensure!(
            valid_name(plugin_name),
            "plugin entry {} has an invalid name",
            index + 1
        );
        ensure!(
            names.insert(plugin_name.to_owned()),
            "marketplace contains duplicate plugin name {plugin_name}"
        );
        let description = match object.get("description") {
            Some(value) => value
                .as_str()
                .with_context(|| format!("plugin {plugin_name} has a non-text description"))?,
            None => "未提供说明",
        };
        plugins.push(Plugin {
            name: plugin_name.into(),
            description: description.trim().chars().take(240).collect(),
        });
    }
    Ok(Marketplace {
        name: name.into(),
        source: String::new(),
        directory: directory.to_path_buf(),
        plugins,
    })
}

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn stable_hash(value: &str) -> u64 {
    value.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn registers_a_local_claude_marketplace_without_touching_a_project() {
        let temp = tempfile::tempdir().unwrap();
        let market = temp.path().join("market");
        fs::create_dir_all(market.join(".claude-plugin")).unwrap();
        fs::write(
            market.join(".claude-plugin/marketplace.json"),
            r#"{
          "name":"team-tools",
          "plugins":[{"name":"review","description":"Review code"}]
        }"#,
        )
        .unwrap();
        let store = Store::at(temp.path().join("orbit-data"));
        let registered = store
            .add(market.to_string_lossy().into_owned())
            .await
            .unwrap();
        assert_eq!(registered.name, "team-tools");
        assert_eq!(registered.plugins[0].name, "review");
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn rejects_malformed_or_duplicate_plugin_entries() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join(".claude-plugin")).unwrap();
        fs::write(
            temp.path().join(".claude-plugin/marketplace.json"),
            r#"{"name":"market","plugins":[{"name":"same"},{"name":"same"}]}"#,
        )
        .unwrap();
        assert!(parse_catalog(temp.path()).is_err());

        fs::write(
            temp.path().join(".claude-plugin/marketplace.json"),
            r#"{"name":"market","plugins":[{"description":"no name"}]}"#,
        )
        .unwrap();
        assert!(parse_catalog(temp.path()).is_err());
    }

    #[test]
    fn accepts_github_shorthand_but_not_credential_urls() {
        assert_eq!(
            git_url("owner/catalog").unwrap(),
            "https://github.com/owner/catalog.git"
        );
        assert!(git_url("https://token@example.com/catalog.git").is_err());
    }
}
