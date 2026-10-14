//! Resolve package declarations from the source, including older cached catalogs
//! whose serialized entries only retained name, description and source.
use super::*;
use serde_json::Value;

pub(super) struct Inspected {
    pub package: PluginPackage,
    /// Exact skill directories; never an unfiltered shared repository root.
    pub skill_directories: Vec<PathBuf>,
}

pub(super) fn inspect(market: &Marketplace, plugin: &Plugin) -> Result<Inspected> {
    let source = plugin.source.as_deref().context(
        "this marketplace entry does not declare a local package source; refresh the marketplace \
            or inspect it outside Orbit",
    )?;
    let market_root = market
        .directory
        .canonicalize()
        .with_context(|| {
            format!(
                "cannot open marketplace directory {}",
                market.directory.display()
            )
        })?;
    let directory = resolve_package_path(&market_root, source)?;
    // Read the original entry instead of requiring a cache schema migration or
    // downloading the source again to recover inline component declarations.
    let entry = if catalog_path(&market_root).is_some() {
        let raw = read_catalog(&market_root)?;
        let current = parse_catalog_value(&market_root, &raw)?;
        ensure!(
            current.plugins.iter().any(|candidate| candidate.name == plugin.name
                && candidate.source == plugin.source),
            "marketplace entry changed; refresh before enabling the plugin"
        );
        raw["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["name"].as_str().map(str::trim) == Some(&plugin.name))
            .unwrap()
            .clone()
    } else {
        Value::Null
    };
    let strict = entry
        .get("strict")
        .map(|value| {
            value
                .as_bool()
                .context("marketplace strict must be a boolean")
        })
        .transpose()?
        .unwrap_or(true);
    let manifest_path = directory.join(".claude-plugin/plugin.json");
    let manifest = match fs::symlink_metadata(&manifest_path) {
        Ok(metadata) => {
            ensure!(
                metadata.is_file() && metadata.len() <= MAX_MANIFEST_BYTES,
                "plugin manifest must be a regular file up to 64 KiB"
            );
            let resolved = manifest_path.canonicalize()?;
            ensure!(
                resolved.starts_with(&directory),
                "plugin manifest escapes its package directory"
            );
            let manifest: Value = serde_json::from_slice(&fs::read(&resolved)?)
                .with_context(|| format!("invalid plugin JSON in {}", manifest_path.display()))?;
            let name = manifest
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .context("plugin manifest has no name")?;
            ensure!(valid_name(name), "plugin manifest name is invalid");
            ensure!(
                name == plugin.name,
                "plugin manifest name {name:?} does not match marketplace entry {:?}",
                plugin.name
            );
            ensure!(
                strict
                    || ![
                        "commands",
                        "agents",
                        "skills",
                        "hooks",
                        "outputStyles",
                        "themes"
                    ]
                    .iter()
                    .any(|key| entry.get(key).is_some()),
                "plugin {} has conflicting marketplace and package manifests",
                plugin.name
            );
            manifest
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !entry.is_null() => {
            Value::Null
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!("plugin manifest is missing: {}", manifest_path.display())
            });
        }
    };
    let description = optional_text(&manifest, "description", "plugin manifest")?
        .or(optional_text(&entry, "description", "marketplace entry")?);
    let version = optional_text(&manifest, "version", "plugin manifest")?.or(optional_text(
        &entry,
        "version",
        "marketplace entry",
    )?);

    let mut skill_directories = Vec::new();
    // Entries sharing the repository root may select disjoint sets of skills.
    // Respect that selection rather than enabling every sibling skill.
    if directory != market_root || entry.get("skills").is_none() {
        let default = directory.join("skills");
        if default.exists() {
            collect_skills(&directory, &default, &mut skill_directories)?;
        } else if entry.get("skills").is_none()
            && manifest.get("skills").is_none()
            && directory.join("SKILL.md").is_file()
        {
            collect_skills(&directory, &directory, &mut skill_directories)?;
        }
    }
    for declaration in [&manifest, &entry] {
        if let Some(paths) = declaration.get("skills") {
            let paths = match paths {
                Value::String(path) => vec![path.as_str()],
                Value::Array(paths) => paths
                    .iter()
                    .map(|path| {
                        path.as_str()
                            .context("plugin skills must contain directory paths")
                    })
                    .collect::<Result<Vec<_>>>()?,
                _ => bail!("plugin skills must be a directory path or an array of paths"),
            };
            for path in paths {
                let path = resolve_package_path(&directory, path)
                    .with_context(|| format!("invalid skill directory {path:?}"))?;
                collect_skills(&directory, &path, &mut skill_directories)?;
            }
        }
    }
    skill_directories.sort();
    skill_directories.dedup();
    let mut skills = skill_directories
        .iter()
        .filter_map(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    skills.sort();
    skills.dedup();
    let package = PluginPackage {
        name: plugin.name.clone(),
        version,
        description,
        commands: declared_markdown_files(&directory.join("commands"), ".md")?,
        agents: declared_markdown_files(&directory.join("agents"), ".md")?,
        declares_hooks: directory
            .join("hooks/hooks.json")
            .is_file()
            || manifest.get("hooks").is_some()
            || entry.get("hooks").is_some(),
        declares_mcp: directory.join(".mcp.json").is_file()
            || manifest.get("mcpServers").is_some()
            || entry.get("mcpServers").is_some(),
        directory,
        skills,
    };
    Ok(Inspected {
        package,
        skill_directories,
    })
}

fn collect_skills(package: &Path, path: &Path, skills: &mut Vec<PathBuf>) -> Result<()> {
    let directory = path.canonicalize()?;
    ensure!(
        directory.starts_with(package),
        "skill directory escapes its plugin package"
    );
    ensure!(directory.is_dir(), "skill source must be a directory");
    if directory.join("SKILL.md").is_file() {
        skills.push(directory);
    } else {
        for child in fs::read_dir(&directory)? {
            let path = child?.path();
            if path.is_dir() && path.join("SKILL.md").is_file() {
                let path = path.canonicalize()?;
                ensure!(
                    path.starts_with(package),
                    "skill directory escapes its plugin package"
                );
                skills.push(path);
            }
        }
    }
    Ok(())
}
