//! Private marketplace catalogues and pinned source acquisition.
//! Package declarations never execute during acquisition or inspection.

use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use orbit_store::types::ProjectPlugin;
use serde::{Deserialize, Serialize};
use tokio::process::Command;

const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
mod package;

pub use orbit_protocol::plugins::{Marketplace, Plugin, PluginPackage};

#[derive(Default, Serialize, Deserialize)]
struct StoredMarketplaces {
    marketplaces: Vec<Marketplace>,
}

pub fn list_current() -> Result<Vec<Marketplace>> {
    Store::current()?.list()
}

pub async fn add_current(source: String) -> Result<Marketplace> {
    Store::current()?.add(source).await
}

pub async fn refresh_current(source: String) -> Result<Marketplace> {
    Store::current()?.refresh(source).await
}

pub fn remove_current(source: &str) -> Result<()> {
    Store::current()?.remove(source)
}

/// Serialize catalogue changes with project enablement checks across service processes.
pub fn lock_current() -> Result<CatalogueLease> {
    Store::current()?.lock()
}

pub struct CatalogueLease(fs::File);

impl Drop for CatalogueLease {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// Enabled skill roots, with diagnostics for packages whose contents have changed.
pub struct EnabledSkillRoots {
    pub roots: Vec<PathBuf>,
    pub diagnostics: Vec<String>,
}

pub fn enabled_skill_roots(enabled: &[ProjectPlugin]) -> EnabledSkillRoots {
    let markets = match Store::current().and_then(|store| store.list()) {
        Ok(markets) => markets,
        Err(error) => {
            return EnabledSkillRoots {
                roots: Vec::new(),
                diagnostics: vec![format!("无法读取插件市场缓存：{error}")],
            };
        }
    };

    enabled_skill_roots_from_markets(enabled, &markets)
}

fn enabled_skill_roots_from_markets(
    enabled: &[ProjectPlugin],
    markets: &[Marketplace],
) -> EnabledSkillRoots {
    let mut roots = Vec::new();
    let mut diagnostics = Vec::new();

    for configured in enabled {
        let result = (|| -> Result<Vec<PathBuf>> {
            let market = markets
                .iter()
                .find(|market| market.source == configured.marketplace_source)
                .with_context(|| {
                    format!("市场 {} 已不在本地缓存中", configured.marketplace_source)
                })?;

            ensure!(
                market.revision.is_some() || local_directory(&market.source).is_some(),
                "市场 {} 的旧缓存未记录 Git 提交；请重新添加市场后启用",
                market.source
            );
            ensure!(
                configured.marketplace_revision == market.revision,
                "市场 {} 的 Git 提交已变更；请重新检查后启用插件",
                market.source
            );
            verify_git_checkout(market)?;

            let plugin = market
                .plugins
                .iter()
                .find(|plugin| {
                    plugin.name == configured.plugin_name
                        && plugin.source.as_deref() == Some(configured.package_source.as_str())
                })
                .with_context(|| {
                    format!(
                        "市场 {} 不再提供插件 {} 的已启用版本",
                        configured.marketplace_source, configured.plugin_name
                    )
                })?;
            let inspected = package::inspect(market, plugin)?;
            let package = inspected.package;
            ensure!(
                package.version == configured.version,
                "插件 {} 的版本已从 {} 变为 {}；请重新检查后启用",
                configured.plugin_name,
                configured.version.as_deref().unwrap_or("未声明"),
                package.version.as_deref().unwrap_or("未声明")
            );

            Ok(inspected.skill_directories)
        })();

        match result {
            Ok(directories) => roots.extend(directories),
            Err(error) => {
                diagnostics.push(format!("已跳过插件 {}：{error}", configured.plugin_name))
            }
        }
    }

    roots.sort();
    roots.dedup();

    EnabledSkillRoots { roots, diagnostics }
}

pub fn inspect_plugin(market: &Marketplace, plugin: &Plugin) -> Result<PluginPackage> {
    Ok(package::inspect(market, plugin)?.package)
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

    #[cfg(test)]
    fn at(root: PathBuf) -> Self {
        Self {
            index: root.join("marketplaces.json"),
            root,
        }
    }

    fn lock(&self) -> Result<CatalogueLease> {
        fs::create_dir_all(&self.root)?;
        let file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.root.join("marketplaces.lock"))?;
        file.try_lock()
            .context("插件市场正在被其他会话更新，请稍后重试")?;
        Ok(CatalogueLease(file))
    }

    fn list(&self) -> Result<Vec<Marketplace>> {
        Ok(self.load()?.marketplaces)
    }

    fn load(&self) -> Result<StoredMarketplaces> {
        if !self.index.exists() {
            return Ok(StoredMarketplaces::default());
        }

        let stored: StoredMarketplaces = serde_json::from_slice(
            &fs::read(&self.index)
                .with_context(|| format!("failed to read {}", self.index.display()))?,
        )
        .with_context(|| format!("failed to parse {}", self.index.display()))?;

        Ok(stored)
    }

    fn save(&self, stored: &StoredMarketplaces) -> Result<()> {
        let mut file = tempfile::NamedTempFile::new_in(&self.root)?;
        serde_json::to_writer_pretty(&mut file, stored)?;
        file.flush()?;
        file.as_file().sync_all()?;
        file.persist(&self.index)?;
        Ok(())
    }

    async fn add(&self, source: String) -> Result<Marketplace> {
        let source = source.trim().to_owned();
        ensure!(!source.is_empty(), "marketplace source cannot be empty");

        fs::create_dir_all(&self.root)?;
        let (directory, revision) = if let Some(path) = local_directory(&source) {
            let directory = path.canonicalize().with_context(|| {
                format!("failed to open marketplace directory {}", path.display())
            })?;
            (directory, None)
        } else {
            self.clone_git_source(&source).await?
        };

        let mut market = parse_catalog(&directory)?;
        market.source = source;
        market.revision = revision;
        self.register(market)
    }

    async fn refresh(&self, source: String) -> Result<Marketplace> {
        let source = source.trim().to_owned();
        ensure!(
            self.list()?.iter().any(|market| market.source == source),
            "插件市场已移除，请重新添加"
        );
        self.add(source).await
    }

    fn register(&self, market: Marketplace) -> Result<Marketplace> {
        let mut stored = self.load()?;
        ensure!(
            !stored.marketplaces.iter().any(|existing| {
                existing.name == market.name && existing.source != market.source
            }),
            "已存在同名的插件市场，请先移除旧来源"
        );
        stored
            .marketplaces
            .retain(|existing| existing.source != market.source);
        stored.marketplaces.push(market.clone());
        stored
            .marketplaces
            .sort_by(|left, right| left.name.cmp(&right.name));
        self.save(&stored)?;
        Ok(market)
    }

    fn remove(&self, source: &str) -> Result<()> {
        let mut stored = self.load()?;
        let previous_len = stored.marketplaces.len();
        stored.marketplaces.retain(|market| market.source != source);
        ensure!(
            stored.marketplaces.len() != previous_len,
            "插件市场已移除，请刷新列表"
        );
        self.save(&stored)
    }

    async fn clone_git_source(&self, source: &str) -> Result<(PathBuf, Option<String>)> {
        let url = git_url(source)?;
        self.clone_git_url(source, &url).await
    }

    async fn clone_git_url(&self, source: &str, url: &str) -> Result<(PathBuf, Option<String>)> {
        let download = tempfile::Builder::new()
            .prefix(&format!("source-{:016x}-", stable_hash(source)))
            .tempdir_in(&self.root)?;
        let temporary = download.path();
        let mut command = Command::new("git");
        command
            // Marketplace acquisition must not execute a user's Git aliases,
            // credential helpers, proxy configuration or interactive prompts.
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LANG", "C.UTF-8")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .args(["clone", "--depth", "1", "--", url])
            .arg(temporary);
        let output = command
            .output()
            .await
            .context("failed to start git while downloading marketplace")?;

        if !output.status.success() {
            bail!(
                "marketplace download failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        let revision_output = Command::new("git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .arg("-C")
            .arg(temporary)
            .args(["rev-parse", "--verify", "HEAD"])
            .output()
            .await
            .context("failed to inspect downloaded marketplace revision")?;

        ensure!(
            revision_output.status.success(),
            "downloaded marketplace has no Git commit"
        );

        let revision = String::from_utf8(revision_output.stdout)?.trim().to_owned();
        ensure!(
            revision.len() == 40 && revision.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "downloaded marketplace has an invalid Git commit"
        );

        // Each acquisition keeps its own checkout. An update cannot replace
        // files already selected by another live session.
        parse_catalog(temporary)?;

        Ok((download.keep(), Some(revision)))
    }
}

fn local_directory(source: &str) -> Option<PathBuf> {
    let path = PathBuf::from(source);
    path.is_dir().then_some(path)
}

fn verify_git_checkout(market: &Marketplace) -> Result<()> {
    let Some(expected) = market.revision.as_deref() else {
        return Ok(());
    };

    let command = |args: &[&str]| -> Result<std::process::Output> {
        std::process::Command::new("git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .arg("--no-optional-locks")
            .args(["-c", "core.fsmonitor=false"])
            .arg("-C")
            .arg(&market.directory)
            .args(args)
            .output()
            .context("cannot inspect cached marketplace checkout")
    };
    let head = command(&["rev-parse", "--verify", "HEAD"])?;
    ensure!(
        head.status.success(),
        "cached marketplace has no Git commit"
    );
    ensure!(
        String::from_utf8(head.stdout)?.trim() == expected,
        "cached marketplace Git commit no longer matches its enabled revision"
    );

    let status = command(&[
        "status",
        "--porcelain",
        "--untracked-files=all",
        "--ignored=matching",
        "--ignore-submodules=none",
    ])?;

    ensure!(
        status.status.success(),
        "cannot inspect cached marketplace files"
    );
    ensure!(
        status.stdout.is_empty(),
        "cached marketplace files changed after download; add the marketplace again"
    );

    Ok(())
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
    let raw = read_catalog(directory)?;
    parse_catalog_value(directory, &raw)
}

fn catalog_path(directory: &Path) -> Option<PathBuf> {
    [
        directory.join(".claude-plugin/marketplace.json"),
        directory.join("marketplace.json"),
    ]
    .into_iter()
    .find(|candidate| candidate.is_file())
}

fn read_catalog(directory: &Path) -> Result<serde_json::Value> {
    let path = catalog_path(directory)
        .context("marketplace does not contain .claude-plugin/marketplace.json")?;
    serde_json::from_slice(&fs::read(&path)?)
        .with_context(|| format!("invalid marketplace JSON in {}", path.display()))
}

fn parse_catalog_value(directory: &Path, raw: &serde_json::Value) -> Result<Marketplace> {
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

        let source = match object.get("source") {
            Some(value) => {
                let source = value
                    .as_str()
                    .map(str::trim)
                    .with_context(|| format!("plugin {plugin_name} has a non-text source"))?;
                validate_relative_package_source(source)
                    .with_context(|| format!("plugin {plugin_name} has an unsafe source"))?;

                Some(source.to_owned())
            }

            None => None,
        };

        plugins.push(Plugin {
            name: plugin_name.into(),
            description: description.trim().chars().take(240).collect(),
            source,
        });
    }

    Ok(Marketplace {
        name: name.into(),
        source: String::new(),
        directory: directory.to_path_buf(),
        revision: None,
        plugins,
    })
}

fn resolve_package_path(market_root: &Path, source: &str) -> Result<PathBuf> {
    validate_relative_package_source(source)?;
    let package = market_root.join(source).canonicalize().with_context(|| {
        format!(
            "plugin package source {source:?} cannot be opened below {}",
            market_root.display()
        )
    })?;

    ensure!(
        package.starts_with(market_root),
        "plugin package source escapes its marketplace directory"
    );
    ensure!(
        package.is_dir(),
        "plugin package source must be a directory"
    );

    Ok(package)
}

fn validate_relative_package_source(source: &str) -> Result<()> {
    ensure!(!source.is_empty(), "source cannot be empty");

    let path = Path::new(source);
    ensure!(
        !path.is_absolute(),
        "source must be a relative directory path"
    );
    ensure!(
        path.components()
            .all(|component| matches!(component, Component::CurDir | Component::Normal(_))),
        "source cannot contain parent, root, or platform prefix components"
    );

    Ok(())
}

fn optional_text(value: &serde_json::Value, key: &str, location: &str) -> Result<Option<String>> {
    match value.get(key) {
        Some(value) => Ok(Some(
            value
                .as_str()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .with_context(|| format!("{location} has a non-text {key}"))?
                .chars()
                .take(240)
                .collect(),
        )),

        None => Ok(None),
    }
}

fn declared_markdown_files(directory: &Path, required_file: &str) -> Result<Vec<String>> {
    if !directory.exists() {
        return Ok(Vec::new());
    }
    ensure!(
        directory.is_dir(),
        "{} must be a directory",
        directory.display()
    );

    let mut names = fs::read_dir(directory)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();

            if required_file == ".md" {
                (path.is_file() && name.ends_with(".md")).then_some(name)
            } else {
                (path.is_dir() && path.join(required_file).is_file()).then_some(name)
            }
        })
        .collect::<Vec<_>>();

    names.sort();

    Ok(names)
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
mod tests;
