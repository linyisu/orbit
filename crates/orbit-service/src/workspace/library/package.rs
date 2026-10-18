//! Bounded file snapshots preserve package resources without executing their contents.
use anyhow::{Context, Result, ensure};
use base64::Engine;
use orbit_protocol::library::{Bundle, Extension, MAX_BUNDLE_BYTES, MAX_BUNDLE_FILES, PackageFile};
use std::{
    collections::HashSet,
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
};

fn relative(value: &str) -> Result<&Path> {
    let path = Path::new(value);
    ensure!(
        !value.is_empty()
            && !value.contains(['\\', '\0'])
            && path
                .components()
                .all(|part| matches!(part, Component::Normal(_))),
        "扩展资源必须使用包内相对路径"
    );
    #[cfg(windows)]
    orbit_runtime::filesystem::path_security::validate_windows_path(path)?;
    Ok(path)
}

pub(super) fn export(extension: Extension, roots: Vec<PathBuf>) -> Result<Bundle> {
    let mut bundle = Bundle {
        extension,
        skill_roots: Vec::new(),
        files: Vec::new(),
    };
    let mut bytes = 0;
    for (ix, root) in roots.iter().enumerate() {
        let name = root
            .file_name()
            .and_then(|name| name.to_str())
            .context("技能目录名称无效")?;
        ensure!(crate::skills::valid_name(name), "技能目录名称无效");
        super::super::skill_import::reject_symlink_ancestors(root)?;
        let prefix = format!("{ix}/{name}");
        bundle.skill_roots.push(prefix.clone());
        collect(root, root, &prefix, &mut bundle.files, &mut bytes, 0)?;
    }
    validate(&bundle)?;
    Ok(bundle)
}

fn collect(
    root: &Path,
    directory: &Path,
    prefix: &str,
    files: &mut Vec<PackageFile>,
    bytes: &mut usize,
    depth: usize,
) -> Result<()> {
    const MAX_RESOURCE_DIRECTORY_DEPTH: usize = 12;
    ensure!(
        depth <= MAX_RESOURCE_DIRECTORY_DEPTH,
        "扩展资源目录层级过深"
    );
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if [".git", "node_modules", "target", ".venv"]
            .iter()
            .any(|name| entry.file_name() == *name)
        {
            continue;
        }
        let kind = entry.file_type()?;
        ensure!(!kind.is_symlink(), "扩展资源不能包含符号链接");
        if kind.is_dir() {
            collect(root, &entry.path(), prefix, files, bytes, depth + 1)?;
            continue;
        }
        ensure!(
            kind.is_file() && files.len() < MAX_BUNDLE_FILES,
            "扩展资源必须为有界的普通文件集合"
        );
        let input = orbit_runtime::filesystem::open_regular_file(&entry.path())?;
        let metadata = input.metadata()?;
        ensure!(
            metadata.is_file() && metadata.len() <= MAX_BUNDLE_BYTES as u64,
            "扩展资源文件过大"
        );
        let mut content = Vec::new();
        input
            .take((MAX_BUNDLE_BYTES - *bytes + 1) as u64)
            .read_to_end(&mut content)?;
        *bytes += content.len();
        ensure!(
            *bytes <= MAX_BUNDLE_BYTES,
            "扩展资源超过 16 MiB，请缩小包内容"
        );
        let path = entry.path();
        let suffix = path
            .strip_prefix(root)?
            .components()
            .map(|component| {
                component
                    .as_os_str()
                    .to_str()
                    .context("扩展资源路径必须为 UTF-8")
            })
            .collect::<Result<Vec<_>>>()?
            .join("/");
        #[cfg(unix)]
        let executable = {
            use std::os::unix::fs::PermissionsExt;
            metadata.permissions().mode() & 0o111 != 0
        };
        #[cfg(not(unix))]
        let executable = false;
        files.push(PackageFile {
            path: format!("{prefix}/{suffix}"),
            content: base64::engine::general_purpose::STANDARD.encode(content),
            executable,
        });
    }
    Ok(())
}

/// Validate every path and byte budget before writing or persisting any received package.
pub(in crate::workspace) fn validate(bundle: &Bundle) -> Result<()> {
    const MAX_EXTENSION_KEY_BYTES: usize = 4_608;
    const MAX_EXTENSION_NAME_BYTES: usize = 256;
    const MAX_SKILL_ROOTS: usize = 128;
    const MAX_RESOURCE_PATH_BYTES: usize = 4_096;
    ensure!(
        !bundle.extension.key.is_empty()
            && bundle.extension.key.len() <= MAX_EXTENSION_KEY_BYTES
            && !bundle.extension.name.trim().is_empty()
            && bundle.extension.name.len() <= MAX_EXTENSION_NAME_BYTES,
        "扩展标识无效"
    );
    let expected_key = match &bundle.extension.kind {
        orbit_protocol::library::ExtensionKind::Skill => format!("skill:{}", bundle.extension.name),
        orbit_protocol::library::ExtensionKind::Plugin(plugin) => {
            ensure!(
                !plugin.marketplace_source.is_empty()
                    && plugin.plugin_name == bundle.extension.name,
                "插件标识无效"
            );
            format!(
                "plugin:{}:{}",
                plugin.marketplace_source, plugin.plugin_name
            )
        }
    };
    ensure!(bundle.extension.key == expected_key, "扩展标识与种类不一致");
    ensure!(
        !bundle.skill_roots.is_empty()
            && bundle.skill_roots.len() <= MAX_SKILL_ROOTS
            && bundle.files.len() <= MAX_BUNDLE_FILES,
        "扩展资源数量无效"
    );
    let mut names = HashSet::new();
    let mut bytes = 0usize;
    for file in &bundle.files {
        let path = relative(&file.path)?;
        ensure!(
            file.path.len() <= MAX_RESOURCE_PATH_BYTES
                && bundle
                    .skill_roots
                    .iter()
                    .any(|root| path.starts_with(root)),
            "扩展文件不属于声明的技能目录"
        );
        ensure!(names.insert(&file.path), "扩展资源路径重复");
        let content = base64::engine::general_purpose::STANDARD.decode(&file.content)?;
        bytes = bytes
            .checked_add(content.len())
            .context("扩展大小溢出")?;
        ensure!(bytes <= MAX_BUNDLE_BYTES, "扩展资源超过 16 MiB");
    }
    let mut commands = HashSet::new();
    for root in &bundle.skill_roots {
        let path = relative(root)?;
        let command = path
            .file_name()
            .and_then(|name| name.to_str())
            .context("技能名称无效")?;
        ensure!(
            crate::skills::valid_name(command) && commands.insert(command),
            "技能名称无效或重复"
        );
        let body = bundle
            .files
            .iter()
            .find(|file| file.path == format!("{root}/SKILL.md"))
            .context("扩展缺少 SKILL.md")?;
        let bytes = base64::engine::general_purpose::STANDARD.decode(&body.content)?;
        ensure!(
            bytes.len() <= crate::skills::MAX_SKILL_BYTES as usize,
            "技能正文超过 1 MiB"
        );
        std::str::from_utf8(&bytes).context("技能正文必须为 UTF-8")?;
    }
    Ok(())
}

pub(in crate::workspace) fn materialize(bundles: &[Bundle]) -> Result<crate::skills::Skills> {
    if bundles.is_empty() {
        return Ok(Default::default());
    }
    const MAX_SESSION_PACKAGES: usize = 64;
    const MAX_SESSION_ENCODED_BYTES: usize = 192 * 1_024 * 1_024;
    ensure!(
        bundles.len() <= MAX_SESSION_PACKAGES,
        "单个会话应用的扩展过多"
    );
    let encoded_bytes = bundles
        .iter()
        .flat_map(|bundle| &bundle.files)
        .try_fold(0usize, |total, file| total.checked_add(file.content.len()))
        .context("会话扩展大小溢出")?;
    ensure!(
        encoded_bytes <= MAX_SESSION_ENCODED_BYTES,
        "会话扩展资源超过容量上限"
    );
    let assets = orbit_runtime::filesystem::temporary_directory("orbit-session-extensions-")?;
    let mut roots = Vec::new();
    let mut names = HashSet::new();
    for (ix, bundle) in bundles.iter().enumerate() {
        validate(bundle)?;
        let directory = assets.path().join(ix.to_string());
        for root in &bundle.skill_roots {
            let name = Path::new(root)
                .file_name()
                .unwrap()
                .to_owned();
            ensure!(
                names.insert(name),
                "已应用的扩展包含重名技能，请先移除冲突扩展"
            );
            roots.push((
                directory.join(root),
                crate::skills::SkillSource::AppliedSession,
            ));
        }
        for file in &bundle.files {
            let path = directory.join(relative(&file.path)?);
            fs::create_dir_all(path.parent().unwrap())?;
            fs::write(
                &path,
                base64::engine::general_purpose::STANDARD.decode(&file.content)?,
            )?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(
                    &path,
                    fs::Permissions::from_mode(if file.executable { 0o700 } else { 0o600 }),
                )?;
            }
        }
    }
    let mut skills = crate::skills::Skills::from_roots(roots);
    skills.retain_assets(assets);
    Ok(skills)
}

#[cfg(test)]
mod native_tests {
    use super::*;

    #[test]
    fn exports_nested_resources_with_portable_paths_and_materializes_them() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("sample-skill");
        fs::create_dir_all(root.join("resources/nested")).unwrap();
        fs::write(
            root.join("SKILL.md"),
            "---\ndescription: Generated skill\n---\nUse resources.",
        )
        .unwrap();
        fs::write(
            root.join("resources/nested/example.txt"),
            b"synthetic resource",
        )
        .unwrap();
        let bundle = export(
            Extension {
                key: "skill:sample-skill".into(),
                name: "sample-skill".into(),
                kind: orbit_protocol::library::ExtensionKind::Skill,
            },
            vec![root],
        )
        .unwrap();
        assert!(
            bundle
                .files
                .iter()
                .all(|file| !file.path.contains('\\'))
        );
        assert!(bundle.files.iter().any(|file| {
            file.path
                .ends_with("/resources/nested/example.txt")
        }));
        materialize(&[bundle]).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn rejects_windows_device_stream_and_trailing_aliases_in_received_packages() {
        for path in [
            "skill/NUL",
            "skill/file:stream",
            "skill/trailing.",
            "skill/trailing ",
        ] {
            assert!(relative(path).is_err(), "accepted {path}");
        }
    }
}
