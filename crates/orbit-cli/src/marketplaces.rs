//! User-owned marketplace catalog storage and safe source acquisition.
//!
//! Marketplaces are never stored in a project directory. Adding one is an
//! explicit user action; Git sources are cloned into Orbit's private data
//! directory, where their catalogue and package declarations can be inspected
//! before any plugin is installed or executable component enabled.

use std::{
    collections::BTreeSet,
    fs,
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail, ensure};
use orbit_store::types::ProjectPlugin;
use serde::{Deserialize, Serialize};
use tokio::process::Command;

const MAX_MANIFEST_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Marketplace {
    pub name: String,
    pub source: String,
    pub directory: PathBuf,
    /// The immutable commit acquired for a Git source. Local directories have
    /// no revision because their owner can edit them in place.
    #[serde(default)]
    pub revision: Option<String>,
    pub plugins: Vec<Plugin>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Plugin {
    pub name: String,
    pub description: String,
    /// Relative package path declared by the marketplace. Older stored
    /// catalogues may not have it, so it remains optional until refreshed.
    #[serde(default)]
    pub source: Option<String>,
}

/// Read-only summary of one plugin package. The contents are deliberately
/// limited to declarations: this never runs hooks, starts MCP servers, or
/// interprets a plugin's instructions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PluginPackage {
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub directory: PathBuf,
    pub skills: Vec<String>,
    pub commands: Vec<String>,
    pub agents: Vec<String>,
    pub declares_hooks: bool,
    pub declares_mcp: bool,
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

/// Validated skill roots from plugins enabled for the current project.
///
/// A stale or altered package is skipped rather than made executable. The
/// diagnostic is intended for the TUI transcript so the user can inspect or
/// disable it in `/plugins`.
pub(crate) struct EnabledSkillRoots {
    pub roots: Vec<PathBuf>,
    pub diagnostics: Vec<String>,
}

pub(crate) fn enabled_skill_roots(enabled: &[ProjectPlugin]) -> EnabledSkillRoots {
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
        let result = (|| -> Result<PathBuf> {
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
            let package = inspect_plugin(market, plugin)?;
            ensure!(
                package.version == configured.version,
                "插件 {} 的版本已从 {} 变为 {}；请重新检查后启用",
                configured.plugin_name,
                configured.version.as_deref().unwrap_or("未声明"),
                package.version.as_deref().unwrap_or("未声明")
            );
            Ok(package.directory.join("skills"))
        })();
        match result {
            Ok(root) if root.is_dir() => roots.push(root),
            Ok(_) => {}
            Err(error) => {
                diagnostics.push(format!("已跳过插件 {}：{error}", configured.plugin_name))
            }
        }
    }
    roots.sort();
    roots.dedup();
    EnabledSkillRoots { roots, diagnostics }
}

pub(crate) fn inspect_plugin(market: &Marketplace, plugin: &Plugin) -> Result<PluginPackage> {
    let source = plugin.source.as_deref().context(
        "this marketplace entry does not declare a local package source; refresh the marketplace or inspect it outside Orbit",
    )?;
    let market_root = market.directory.canonicalize().with_context(|| {
        format!(
            "cannot open marketplace directory {}",
            market.directory.display()
        )
    })?;
    let package = resolve_package_path(&market_root, source)?;
    let manifest_path = package.join(".claude-plugin/plugin.json");
    let metadata = fs::metadata(&manifest_path)
        .with_context(|| format!("plugin manifest is missing: {}", manifest_path.display()))?;
    ensure!(
        metadata.is_file() && metadata.len() <= MAX_MANIFEST_BYTES,
        "plugin manifest must be a regular file up to 64 KiB"
    );
    let manifest: serde_json::Value = serde_json::from_slice(&fs::read(&manifest_path)?)
        .with_context(|| format!("invalid plugin JSON in {}", manifest_path.display()))?;
    let name = manifest
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .context("plugin manifest has no name")?;
    ensure!(valid_name(name), "plugin manifest name is invalid");
    ensure!(
        name == plugin.name,
        "plugin manifest name {name:?} does not match marketplace entry {:?}",
        plugin.name
    );
    let description = optional_text(&manifest, "description", "plugin manifest")?;
    let version = optional_text(&manifest, "version", "plugin manifest")?;
    let skills = declared_markdown_files(&package.join("skills"), "SKILL.md")?;
    let commands = declared_markdown_files(&package.join("commands"), ".md")?;
    let agents = declared_markdown_files(&package.join("agents"), ".md")?;
    let declares_hooks =
        package.join("hooks/hooks.json").is_file() || manifest.get("hooks").is_some();
    let declares_mcp = package.join(".mcp.json").is_file() || manifest.get("mcpServers").is_some();
    Ok(PluginPackage {
        name: name.into(),
        version,
        description,
        directory: package,
        skills,
        commands,
        agents,
        declares_hooks,
        declares_mcp,
    })
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

    async fn clone_git_source(&self, source: &str) -> Result<(PathBuf, Option<String>)> {
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
            .args(["clone", "--depth", "1", "--", &url])
            .arg(&temporary);
        let output = command
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
        let revision_output = Command::new("git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .arg("-C")
            .arg(&temporary)
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
        Ok((target, Some(revision)))
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
          "plugins":[{"name":"review","description":"Review code","source":"./plugins/review"}]
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
        assert_eq!(
            registered.plugins[0].source.as_deref(),
            Some("./plugins/review")
        );
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn inspects_declarations_without_executing_them() {
        let temp = tempfile::tempdir().unwrap();
        let market = temp.path();
        fs::create_dir_all(market.join(".claude-plugin")).unwrap();
        let package = market.join("plugins/review");
        fs::create_dir_all(package.join(".claude-plugin")).unwrap();
        fs::create_dir_all(package.join("skills/check")).unwrap();
        fs::create_dir_all(package.join("commands")).unwrap();
        fs::create_dir_all(package.join("agents")).unwrap();
        fs::create_dir_all(package.join("hooks")).unwrap();
        fs::write(
            package.join(".claude-plugin/plugin.json"),
            r#"{"name":"review","version":"1.2.3","description":"Review safely","mcpServers":{"docs":{}}}"#,
        )
        .unwrap();
        fs::write(package.join("skills/check/SKILL.md"), "# check").unwrap();
        fs::write(package.join("commands/review.md"), "# review").unwrap();
        fs::write(package.join("agents/reviewer.md"), "# reviewer").unwrap();
        fs::write(package.join("hooks/hooks.json"), "{}").unwrap();
        let market = Marketplace {
            name: "market".into(),
            source: "local".into(),
            directory: market.to_path_buf(),
            revision: None,
            plugins: vec![],
        };
        let plugin = Plugin {
            name: "review".into(),
            description: "Review".into(),
            source: Some("./plugins/review".into()),
        };
        let inspected = inspect_plugin(&market, &plugin).unwrap();
        assert_eq!(inspected.version.as_deref(), Some("1.2.3"));
        assert_eq!(inspected.skills, ["check"]);
        assert_eq!(inspected.commands, ["review.md"]);
        assert_eq!(inspected.agents, ["reviewer.md"]);
        assert!(inspected.declares_hooks);
        assert!(inspected.declares_mcp);
    }

    #[test]
    fn enabled_plugin_skills_require_the_checked_package_version() {
        let temp = tempfile::tempdir().unwrap();
        let market_root = temp.path().join("market");
        let package = market_root.join("plugins/review");
        fs::create_dir_all(package.join(".claude-plugin")).unwrap();
        fs::create_dir_all(package.join("skills/check")).unwrap();
        fs::write(
            package.join(".claude-plugin/plugin.json"),
            r#"{"name":"review","version":"1.2.3"}"#,
        )
        .unwrap();
        fs::write(package.join("skills/check/SKILL.md"), "# check").unwrap();
        let market = Marketplace {
            name: "market".into(),
            source: "owner/market".into(),
            directory: market_root,
            revision: Some("a".repeat(40)),
            plugins: vec![Plugin {
                name: "review".into(),
                description: "Review".into(),
                source: Some("plugins/review".into()),
            }],
        };
        let enabled = ProjectPlugin {
            project_id: "project".into(),
            marketplace_source: "owner/market".into(),
            plugin_name: "review".into(),
            package_source: "plugins/review".into(),
            version: Some("1.2.3".into()),
            marketplace_revision: Some("a".repeat(40)),
            enabled_at: 0,
        };

        let roots = enabled_skill_roots_from_markets(
            std::slice::from_ref(&enabled),
            std::slice::from_ref(&market),
        );
        assert_eq!(roots.roots, vec![package.join("skills")]);
        assert!(roots.diagnostics.is_empty());

        let stale = ProjectPlugin {
            version: Some("1.0.0".into()),
            ..enabled.clone()
        };
        let stale_roots = enabled_skill_roots_from_markets(&[stale], std::slice::from_ref(&market));
        assert!(stale_roots.roots.is_empty());
        assert!(stale_roots.diagnostics[0].contains("重新检查后启用"));

        let updated_market = Marketplace {
            revision: Some("b".repeat(40)),
            ..market.clone()
        };
        let unchanged_version = ProjectPlugin {
            version: Some("1.2.3".into()),
            marketplace_revision: Some("a".repeat(40)),
            ..enabled
        };
        let updated_roots =
            enabled_skill_roots_from_markets(&[unchanged_version], &[updated_market]);
        assert!(updated_roots.roots.is_empty());
        assert!(updated_roots.diagnostics[0].contains("Git 提交已变更"));
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

        fs::write(
            temp.path().join(".claude-plugin/marketplace.json"),
            r#"{"name":"market","plugins":[{"name":"safe","source":"../escape"}]}"#,
        )
        .unwrap();
        assert!(parse_catalog(temp.path()).is_err());
    }

    #[test]
    fn rejects_packages_outside_the_marketplace() {
        let temp = tempfile::tempdir().unwrap();
        assert!(resolve_package_path(temp.path(), "../outside").is_err());
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
