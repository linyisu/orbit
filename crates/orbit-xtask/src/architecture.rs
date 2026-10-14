use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, ensure};
use regex::Regex;
use toml::Value;

const DEPENDENCIES: &[(&str, &[&str])] = &[
    ("orbit-build", &[]),
    ("orbit-auth", &[]),
    ("orbit-protocol", &[]),
    ("orbit-syntax", &[]),
    (
        "orbit-config",
        &[
            "orbit-agent",
            "orbit-auth",
            "orbit-model",
            "orbit-protocol",
            "orbit-runtime",
            "orbit-tools",
        ],
    ),
    (
        "orbit-client",
        &[
            "orbit-agent",
            "orbit-auth",
            "orbit-model",
            "orbit-tools",
            "orbit-config",
            "orbit-protocol",
        ],
    ),
    ("orbit-model", &["orbit-auth"]),
    ("orbit-runtime", &[]),
    ("orbit-agent", &["orbit-model", "orbit-runtime"]),
    (
        "orbit-tools",
        &["orbit-agent", "orbit-model", "orbit-runtime"],
    ),
    (
        "orbit-store",
        &[
            "orbit-agent",
            "orbit-model",
            "orbit-runtime",
            "orbit-protocol",
        ],
    ),
    (
        "orbit-service",
        &[
            "orbit-agent",
            "orbit-auth",
            "orbit-model",
            "orbit-runtime",
            "orbit-store",
            "orbit-tools",
            "orbit-config",
            "orbit-protocol",
        ],
    ),
    (
        "orbit-tui",
        &[
            "orbit-syntax",
            "orbit-build",
            "orbit-config",
            "orbit-agent",
            "orbit-auth",
            "orbit-model",
            "orbit-runtime",
            "orbit-tools",
            "orbit-client",
            "orbit-protocol",
        ],
    ),
    (
        "orbit-gui",
        &[
            "orbit-syntax",
            "orbit-build",
            "orbit-client",
            "orbit-auth",
            "orbit-config",
            "orbit-protocol",
            "orbit-agent",
            "orbit-model",
            "orbit-tools",
            "orbit-runtime",
        ],
    ),
    (
        "orbit-cli",
        &[
            "orbit-build",
            "orbit-protocol",
            "orbit-config",
            "orbit-client",
            "orbit-agent",
            "orbit-auth",
            "orbit-model",
        ],
    ),
    ("orbit-server", &["orbit-service"]),
    ("orbit-xtask", &["orbit-build", "orbit-protocol"]),
];
const DATABASE: &[&str] = &["sea-orm", "sea-orm-migration", "sqlx", "diesel", "rusqlite"];
const PRESENTATION: &[&str] = &[
    "arboard",
    "clap",
    "crossterm",
    "ratatui",
    "ratatui-image",
    "syntect",
    "gpui-kit",
    "gpui-shell",
    "gpui-component-shell",
];

fn manifest(path: &Path) -> Result<Value> {
    let content = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    toml::from_str(&content).with_context(|| format!("parse {}", path.display()))
}

fn dependencies(value: &Value, kinds: &[&str]) -> BTreeSet<String> {
    let mut result = BTreeSet::new();
    for kind in kinds {
        if let Some(table) = value
            .get(kind)
            .and_then(Value::as_table)
        {
            // Honor renamed dependencies as well as their local alias.
            for (name, definition) in table {
                result.insert(
                    definition
                        .get("package")
                        .and_then(Value::as_str)
                        .unwrap_or(name)
                        .to_owned(),
                );
            }
        }
    }
    if let Some(targets) = value
        .get("target")
        .and_then(Value::as_table)
    {
        for target in targets.values() {
            result.extend(dependencies(target, kinds));
        }
    }
    result
}

fn sources(directory: &Path, extension: &str) -> Result<Vec<PathBuf>> {
    let mut result = Vec::new();
    if !directory.exists() {
        return Ok(result);
    }
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            result.extend(sources(&entry.path(), extension)?);
        } else if entry
            .path()
            .extension()
            .is_some_and(|value| value == extension)
        {
            result.push(entry.path());
        }
    }
    result.sort();
    Ok(result)
}

fn inspect(
    root: &Path,
    path: &Path,
    pattern: &Regex,
    message: &str,
    errors: &mut Vec<String>,
) -> Result<()> {
    if path.is_file() && pattern.is_match(&fs::read_to_string(path)?) {
        errors.push(format!("{}: {message}", path.strip_prefix(root)?.display()));
    }
    Ok(())
}

pub fn check(root: &Path) -> Result<Vec<String>> {
    let workspace = manifest(&root.join("Cargo.toml"))?;
    let members = workspace
        .get("workspace")
        .and_then(|value| value.get("members"))
        .and_then(Value::as_array)
        .context("workspace.members must be an array")?;
    let mut packages = BTreeSet::new();
    let mut errors = Vec::new();
    for member in members {
        let member = member
            .as_str()
            .context("workspace member must be a path")?;
        let config = manifest(&root.join(member).join("Cargo.toml"))?;
        let name = config
            .get("package")
            .and_then(|value| value.get("name"))
            .and_then(Value::as_str)
            .context("package.name is missing")?;
        ensure!(packages.insert(name.to_owned()), "duplicate package {name}");
        let normal = dependencies(&config, &["dependencies", "build-dependencies"]);
        if name != "orbit-store" {
            let database: Vec<_> = normal
                .iter()
                .filter(|dep| DATABASE.contains(&dep.as_str()))
                .collect();
            if !database.is_empty() {
                errors.push(format!(
                    "{name}: database access belongs to orbit-store: {database:?}"
                ));
            }
        } else {
            let all = dependencies(
                &config,
                &["dependencies", "build-dependencies", "dev-dependencies"],
            );
            if all
                .iter()
                .any(|name| ["rusqlite", "diesel", "sqlx"].contains(&name.as_str()))
            {
                errors.push(format!(
                    "{name}: application and test database access must use SeaORM"
                ));
            }
        }
        let deps = if matches!(name, "orbit-tui" | "orbit-gui") {
            dependencies(
                &config,
                &["dependencies", "build-dependencies", "dev-dependencies"],
            )
        } else {
            normal.clone()
        };
        let Some((_, allowed)) = DEPENDENCIES
            .iter()
            .find(|(package, _)| *package == name)
        else {
            errors.push(format!("{name}: define its dependency boundary"));
            continue;
        };
        let unexpected: Vec<_> = deps
            .iter()
            .filter(|dep| dep.starts_with("orbit-") && !allowed.contains(&dep.as_str()))
            .collect();
        if !unexpected.is_empty() {
            errors.push(format!(
                "{name}: unexpected application dependencies {unexpected:?}"
            ));
        }
        if name == "orbit-service" {
            let presentation: Vec<_> = normal
                .iter()
                .filter(|dep| PRESENTATION.contains(&dep.as_str()))
                .collect();
            if !presentation.is_empty() {
                errors.push(format!(
                    "{name}: presentation dependencies {presentation:?}"
                ));
            }
        }
    }
    for (name, _) in DEPENDENCIES {
        if !packages.contains(*name) {
            errors.push(format!("{name}: workspace member missing"));
        }
    }
    let execution =
        Regex::new(r"\b(?:Agent::new|run_with_agent_events|compact_configured|workspace_tools)\b")?;
    for package in ["orbit-cli", "orbit-tui", "orbit-gui"] {
        for source in sources(
            &root
                .join("crates")
                .join(package)
                .join("src"),
            "rs",
        )? {
            inspect(
                root,
                &source,
                &execution,
                "execution belongs to orbit-service",
                &mut errors,
            )?;
        }
    }
    let presentation = Regex::new(r"\b(?:orbit_tui|crate::tui)::")?;
    for source in sources(&root.join("crates/orbit-service/src"), "rs")? {
        inspect(
            root,
            &source,
            &presentation,
            "application depends on terminal presentation",
            &mut errors,
        )?;
    }
    let migration_pattern =
        Regex::new(r"\b(?:execute_unprepared|from_sql_and_values|from_string|include_str)\s*[!(]")?;
    let mut migrations = sources(&root.join("crates/orbit-store/src/migrations"), "rs")?;
    migrations.push(root.join("crates/orbit-store/src/migrations.rs"));
    for source in migrations {
        inspect(
            root,
            &source,
            &migration_pattern,
            "migrations must use Rust schema/query builders; SQLite connection controls belong to \
                database/sqlite.rs",
            &mut errors,
        )?;
    }
    let store = root.join("crates/orbit-store");
    for source in sources(&store, "sql")? {
        errors.push(format!(
            "{}: database fixtures must use Rust migration/schema builders",
            source.strip_prefix(root)?.display()
        ));
    }
    let raw =
        Regex::new(r"\b(?:execute_unprepared|execute_batch|from_sql_and_values|from_string)\s*\(")?;
    let controls = [
        store.join("src/database.rs"),
        store.join("src/database/sqlite.rs"),
    ];
    for source in sources(&store, "rs")? {
        if !controls.contains(&source) {
            inspect(
                root,
                &source,
                &raw,
                "database access must use Rust schema/query builders",
                &mut errors,
            )?;
        }
    }
    Ok(errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn workspace() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let members: Vec<_> = DEPENDENCIES
            .iter()
            .map(|(name, _)| format!("crates/{name}"))
            .collect();
        fs::write(
            dir.path().join("Cargo.toml"),
            toml::toml! { [workspace] members = members }.to_string(),
        )
        .unwrap();
        for (name, _) in DEPENDENCIES {
            let path = dir.path().join("crates").join(name);
            fs::create_dir_all(&path).unwrap();
            fs::write(
                path.join("Cargo.toml"),
                format!("[package]\nname = {name:?}\n"),
            )
            .unwrap();
        }
        dir
    }

    #[test]
    fn tui_tests_cannot_bypass_service_ownership_with_target_or_renamed_dependencies() {
        let dir = workspace();
        let manifest = dir
            .path()
            .join("crates/orbit-tui/Cargo.toml");
        for section in ["dev-dependencies", "target.'cfg(unix)'.dev-dependencies"] {
            for dependency in [
                "orbit-client",
                "orbit-protocol",
                "orbit-service",
                "orbit-store",
            ] {
                fs::write(
                    &manifest,
                    format!(
                        "[package]\nname = 'orbit-tui'\n[{section}]\nlocal = \
                    {{ package = {dependency:?}, version = '*' }}\n"
                    ),
                )
                .unwrap();
                let errors = check(dir.path()).unwrap();
                if ["orbit-client", "orbit-protocol"].contains(&dependency) {
                    assert!(errors.is_empty(), "{errors:?}");
                } else {
                    assert_eq!(errors.len(), 1);
                    assert!(errors[0].contains(dependency));
                }
            }
        }
    }

    #[test]
    fn storage_tests_use_rust_builders_and_seaorm() {
        let dir = workspace();
        let store = dir.path().join("crates/orbit-store");
        fs::create_dir_all(store.join("tests")).unwrap();
        let sql = store.join("tests/schema.sql");
        fs::write(&sql, "").unwrap();
        assert!(
            check(dir.path())
                .unwrap()
                .iter()
                .any(|error| error.contains("Rust migration/schema builders"))
        );
        fs::remove_file(sql).unwrap();
        let source = store.join("tests/schema.rs");
        fs::write(&source, "db.execute_batch(sql);").unwrap();
        assert!(
            check(dir.path())
                .unwrap()
                .iter()
                .any(|error| error.contains("Rust schema/query builders"))
        );
        fs::remove_file(source).unwrap();
        fs::write(
            store.join("Cargo.toml"),
            "[package]\nname = 'orbit-store'\n[dev-dependencies]\nrusqlite = '*'\n",
        )
        .unwrap();
        assert!(
            check(dir.path())
                .unwrap()
                .iter()
                .any(|error| error.contains("must use SeaORM"))
        );
    }
}
