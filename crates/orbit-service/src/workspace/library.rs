//! Global extension catalogs have no workspace or conversation ownership.
//! Only explicitly exported immutable snapshots may enter a session.
pub(super) mod package;

use anyhow::{Context, Result, bail, ensure};
use orbit_protocol::{
    library::{Action, Extension, ExtensionKind, Snapshot},
    plugins::{self, Catalog, Inspection},
    resources::{self, Skill},
};
use std::{
    collections::HashSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

pub(super) struct Library {
    home: PathBuf,
    config_home: PathBuf,
    marketplaces: crate::marketplaces::Store,
    scans: std::collections::HashMap<String, super::skill_import::Scan>,
}

impl Library {
    pub(super) fn new(options: &crate::WorkspaceOptions) -> Result<Self> {
        Ok(Self {
            home: options
                .home
                .clone()
                .context("无法确定全局技能的个人目录")?,
            config_home: options
                .config_home
                .clone()
                .context("无法确定全局配置目录")?,
            marketplaces: crate::marketplaces::Store::current()?,
            scans: Default::default(),
        })
    }

    fn skills(&self) -> crate::skills::Skills {
        crate::skills::Skills::global(Some(self.home.clone()), Some(self.config_home.clone()))
    }

    fn destination(&self) -> PathBuf {
        self.config_home.join("orbit/skills")
    }

    fn names(&self) -> HashSet<String> {
        self.skills()
            .completions()
            .into_iter()
            .map(|(name, _)| name)
            .collect()
    }

    /// Manage global definitions without opening a session or loading model configuration.
    pub(super) async fn execute(&mut self, action: Action) -> Result<Snapshot> {
        match action {
            Action::Skills(action) => Ok(Snapshot::Skills(self.manage_skills(action).await?)),
            Action::Plugins(action) => Ok(Snapshot::Plugins(self.manage_plugins(action).await?)),
            Action::ExportSkill { name } => {
                let skills = self.skills();
                let skill = skills
                    .get(&name)
                    .context("全局技能不存在，请刷新列表")?;
                Ok(Snapshot::Bundle(package::export(
                    Extension {
                        key: format!("skill:{name}"),
                        name,
                        kind: ExtensionKind::Skill,
                    },
                    vec![
                        skill
                            .path
                            .parent()
                            .context("技能目录无效")?
                            .to_path_buf(),
                    ],
                )?))
            }
            Action::ExportPlugin { plugin } => {
                let _lease = self.marketplaces.lock()?;
                let markets = self.marketplaces.list()?;
                let market = markets
                    .iter()
                    .find(|market| market.source == plugin.marketplace_source)
                    .context("插件市场已变更，请刷新列表")?;
                let entry = market
                    .plugins
                    .iter()
                    .find(|entry| entry.name == plugin.plugin_name)
                    .context("插件不存在，请刷新列表")?;
                let inspected = crate::marketplaces::inspect_plugin(market, entry)?;
                ensure!(
                    market.revision == plugin.marketplace_revision
                        && entry.source.as_deref() == Some(&plugin.package_source)
                        && inspected.version == plugin.version,
                    "插件版本已变更，请刷新后重新应用"
                );
                let roots = crate::marketplaces::plugin_skill_directories(market, entry)?;
                ensure!(
                    !roots.is_empty(),
                    "该插件没有可应用的技能；Hook、MCP、命令和 Agent 尚不自动启用"
                );
                Ok(Snapshot::Bundle(package::export(
                    Extension {
                        key: format!(
                            "plugin:{}:{}",
                            plugin.marketplace_source, plugin.plugin_name
                        ),
                        name: plugin.plugin_name.clone(),
                        kind: ExtensionKind::Plugin(plugin),
                    },
                    roots,
                )?))
            }
        }
    }

    async fn manage_skills(&mut self, action: resources::Action) -> Result<resources::Snapshot> {
        let _lease = if matches!(
            &action,
            resources::Action::SaveSkill { .. } | resources::Action::ImportSkills { .. }
        ) {
            Some(self.marketplaces.lock()?)
        } else {
            None
        };
        match action {
            resources::Action::Skills => {}
            resources::Action::ReadSkill { name } => {
                let skills = self.skills();
                let skill = skills
                    .get(&name)
                    .context("全局技能不存在，请刷新列表")?;
                return Ok(resources::Snapshot::Skill {
                    name,
                    content: crate::skills::read_skill(&skill.path).map_err(anyhow::Error::msg)?,
                    editable: skill.source == crate::skills::SkillSource::OrbitUser,
                });
            }
            resources::Action::SaveSkill {
                name,
                original,
                content,
            } => {
                ensure!(crate::skills::valid_name(&name), "技能名称无效");
                let skills = self.skills();
                let path = if original.is_some() {
                    let skill = skills
                        .get(&name)
                        .context("全局技能不存在，请刷新列表")?;
                    ensure!(
                        skill.source == crate::skills::SkillSource::OrbitUser,
                        "兼容目录中的技能只读，请导入全局库后编辑"
                    );
                    skill.path.clone()
                } else {
                    ensure!(skills.get(&name).is_none(), "全局技能名称已存在");
                    self.destination()
                        .join(&name)
                        .join("SKILL.md")
                };
                save_skill(&path, &name, original.as_deref(), &content)?;
            }
            resources::Action::ScanSkills { source } => {
                const MAX_PENDING_SKILL_IMPORTS: usize = 32;
                self.scans
                    .retain(|_, scan| !scan.expired());
                ensure!(
                    self.scans.len() < MAX_PENDING_SKILL_IMPORTS,
                    "技能导入数量过多，请稍后重试"
                );
                let scan = super::skill_import::Scan::acquire_to(
                    "global-library".into(),
                    self.home.clone(),
                    source,
                    Some(self.home.clone()),
                    self.names(),
                    self.destination(),
                )
                .await?;
                let snapshot = scan.snapshot();
                if let resources::Snapshot::SkillSource { token, .. } = &snapshot {
                    self.scans.insert(token.clone(), scan);
                }
                return Ok(snapshot);
            }
            resources::Action::ImportSkills { token, paths } => {
                let scan = self
                    .scans
                    .remove(&token)
                    .context("技能来源已过期，请重新扫描")?;
                scan.install(&token, "global-library", &self.home, &paths, &self.names())?;
            }
            _ => bail!("此操作不属于全局技能库"),
        }
        let skills = self.skills();
        Ok(resources::Snapshot::Skills(
            skills
                .completions()
                .into_iter()
                .filter_map(|(name, _)| {
                    let skill = skills.get(&name)?;
                    Some(Skill {
                        name: name.clone(),
                        title: skill.title.clone(),
                        description: skill.description.clone(),
                        source: if skill.source == crate::skills::SkillSource::OrbitUser {
                            "全局技能"
                        } else {
                            "兼容技能（只读）"
                        }
                        .into(),
                        editable: skill.source == crate::skills::SkillSource::OrbitUser,
                    })
                })
                .collect(),
        ))
    }

    async fn manage_plugins(&self, action: plugins::Action) -> Result<Catalog> {
        let _lease = self.marketplaces.lock()?;
        match action {
            plugins::Action::List => {}
            plugins::Action::Add { source } => {
                self.marketplaces.add(source).await?;
            }
            plugins::Action::Refresh { source } => {
                self.marketplaces
                    .refresh(source)
                    .await?;
            }
            plugins::Action::Remove { source } => self.marketplaces.remove(&source)?,
            _ => bail!("插件必须应用到指定会话"),
        }
        let marketplaces = self.marketplaces.list()?;
        let inspections = marketplaces
            .iter()
            .flat_map(|market| {
                market
                    .plugins
                    .iter()
                    .map(|plugin| Inspection {
                        source: market.source.clone(),
                        name: plugin.name.clone(),
                        package: crate::marketplaces::inspect_plugin(market, plugin)
                            .map_err(|e| e.to_string()),
                    })
            })
            .collect();
        Ok(Catalog {
            marketplaces,
            session_id: String::new(),
            applied: Vec::new(),
            inspections,
        })
    }
}

/// Reconstruct only this session's snapshots, independently of global or project changes.
pub(super) async fn session_skills(
    session: &crate::session::SessionState,
) -> Result<crate::skills::Skills> {
    let bundles = session
        .store
        .session_extensions(session.id())
        .await?;
    package::materialize(&bundles)
}

pub(super) async fn applied(setup: &crate::PreparedWorkspace) -> Result<resources::Snapshot> {
    Ok(resources::Snapshot::Extensions(
        setup
            .session
            .store
            .session_extensions(setup.session.id())
            .await?
            .into_iter()
            .map(|bundle| bundle.extension)
            .collect(),
    ))
}

#[cfg(test)]
impl Library {
    pub(super) fn isolated(options: &crate::WorkspaceOptions) -> Self {
        let home = options
            .home
            .clone()
            .unwrap_or_else(|| options.initial_directory.clone());
        Self {
            config_home: options
                .config_home
                .clone()
                .unwrap_or_else(|| home.join("config")),
            marketplaces: crate::marketplaces::Store::at(home.join("data/marketplaces")),
            home,
            scans: Default::default(),
        }
    }
}

#[cfg(test)]
pub(super) fn test_bundle(name: &str, body: &str) -> orbit_protocol::library::Bundle {
    use base64::Engine;
    orbit_protocol::library::Bundle {
        extension: Extension {
            key: format!("skill:{name}"),
            name: name.into(),
            kind: ExtensionKind::Skill,
        },
        skill_roots: vec![format!("0/{name}")],
        files: vec![orbit_protocol::library::PackageFile {
            path: format!("0/{name}/SKILL.md"),
            content: base64::engine::general_purpose::STANDARD.encode(body),
            executable: false,
        }],
    }
}

#[cfg(test)]
mod tests;

fn save_skill(path: &Path, name: &str, original: Option<&str>, content: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 64
            && name
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
        "名称只能包含字母、数字、横线和下划线（最多 64 字符）"
    );
    ensure!(
        !content.trim().is_empty() && content.len() <= crate::skills::MAX_SKILL_BYTES as usize,
        "技能内容不能为空，且不能超过 1 MiB"
    );
    for part in path.ancestors() {
        match fs::symlink_metadata(part) {
            Ok(meta) => ensure!(!meta.file_type().is_symlink(), "不能编辑符号链接中的技能"),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let parent = path.parent().context("技能目录无效")?;
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    if let Ok(meta) = fs::metadata(path) {
        file.as_file()
            .set_permissions(meta.permissions())?;
    }
    file.write_all(content.as_bytes())?;
    file.as_file().sync_all()?;
    if let Some(original) = original {
        ensure!(
            crate::skills::read_skill(path).map_err(anyhow::Error::msg)? == original,
            "技能已被其他程序修改，请重新打开后编辑"
        );
        file.persist(path)?;
    } else {
        file.persist_noclobber(path)
            .context("技能已存在，请换一个名称")?;
    }
    Ok(())
}
