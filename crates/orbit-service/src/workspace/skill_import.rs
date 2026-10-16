//! User-selected skill sources are fetched on the owning workspace server.
//! Discovery snapshots whole skill packages without executing their contents.
mod source;
pub(super) use source::acquire as acquire_source;
#[cfg(test)]
mod tests;

use anyhow::{Context, Result, ensure};
use orbit_protocol::resources::{SkillCandidate, Snapshot};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

const MAX_ENTRIES: usize = 10_000;
const MAX_SKILLS: usize = 128;
const MAX_DEPTH: usize = 12;
const MAX_FILE_BYTES: u64 = 8 * 1_024 * 1_024;
const MAX_PACKAGE_BYTES: u64 = 32 * 1_024 * 1_024;
const MAX_SNAPSHOT_BYTES: u64 = 128 * 1_024 * 1_024;
const LIFETIME: Duration = Duration::from_secs(15 * 60);

pub(super) struct Scan {
    storage: tempfile::TempDir,
    token: String,
    session: String,
    workspace: PathBuf,
    destination: PathBuf,
    source: String,
    candidates: Vec<SkillCandidate>,
    created: Instant,
}

impl Scan {
    #[cfg(test)]
    pub(super) async fn acquire(
        session: String,
        workspace: PathBuf,
        source: String,
        home: Option<PathBuf>,
        existing: HashSet<String>,
    ) -> Result<Self> {
        let destination = project_root(&workspace).join(".orbit/skills");
        Self::acquire_to(session, workspace, source, home, existing, destination).await
    }

    /// Global imports use the same frozen-package validation with an explicit destination.
    pub(super) async fn acquire_to(
        session: String,
        workspace: PathBuf,
        source: String,
        home: Option<PathBuf>,
        existing: HashSet<String>,
        destination: PathBuf,
    ) -> Result<Self> {
        ensure!(!source.trim().is_empty(), "请输入技能来源");
        ensure!(source.len() <= 4_096, "技能来源地址过长");
        let storage = tempfile::Builder::new()
            .prefix("orbit-skills-")
            .tempdir()?;
        let root =
            source::acquire(source.trim(), &workspace, home.as_deref(), storage.path()).await?;
        // Keeping all copies under a private temporary directory makes the selected
        // content independent of a later Git update or edit to a local source.
        tokio::task::spawn_blocking(move || {
            let workspace = fs::canonicalize(workspace).context("无法读取当前工作区")?;
            let mut scan = Self {
                token: storage
                    .path()
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned(),
                storage,
                session,
                workspace,
                destination,
                source,
                candidates: Vec::new(),
                created: Instant::now(),
            };
            scan.discover(&root, &existing)?;
            Ok(scan)
        })
        .await?
    }

    pub(super) fn expired(&self) -> bool {
        self.created.elapsed() >= LIFETIME
    }

    pub(super) fn snapshot(&self) -> Snapshot {
        Snapshot::SkillSource {
            token: self.token.clone(),
            source: self.source.clone(),
            skills: self.candidates.clone(),
        }
    }

    fn discover(&mut self, root: &Path, existing: &HashSet<String>) -> Result<()> {
        reject_symlink_ancestors(root)?;
        let root = fs::canonicalize(root).context("无法读取技能来源")?;
        let root = if root.is_file() {
            ensure!(
                root.file_name()
                    .is_some_and(|name| name == "SKILL.md"),
                "请选择 SKILL.md 文件或技能目录"
            );
            root.parent()
                .context("技能目录无效")?
                .to_path_buf()
        } else {
            root
        };
        ensure!(root.is_dir(), "技能来源不是目录");
        let mut directories = vec![(root.clone(), 0)];
        let mut entries = 0;
        let mut total = 0;
        while let Some((directory, depth)) = directories.pop() {
            ensure!(
                depth <= MAX_DEPTH,
                "技能来源目录层级过深，请选择更具体的子目录"
            );
            let skill_file = directory.join("SKILL.md");
            if fs::symlink_metadata(&skill_file).is_ok() {
                ensure!(
                    self.candidates.len() < MAX_SKILLS,
                    "技能来源超过 128 个技能，请选择子目录后重试"
                );
                let body = crate::skills::read_skill(&skill_file).map_err(anyhow::Error::msg)?;
                let (fields, text) = crate::skills::split_frontmatter(&body);
                let name = fields
                    .get("name")
                    .filter(|value| crate::skills::valid_name(value))
                    .cloned()
                    .unwrap_or_else(|| {
                        directory
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned()
                    });
                ensure!(
                    crate::skills::valid_name(&name),
                    "技能 {} 的名称无效，应使用 1–64 个字母、数字、横线或下划线",
                    directory.display()
                );
                let description = fields
                    .get("description")
                    .cloned()
                    .unwrap_or_else(|| crate::skills::first_sentence(text));
                ensure!(
                    !description.trim().is_empty(),
                    "技能 {} 缺少说明或正文，无法加载；请补全 SKILL.md 后重新扫描",
                    directory.display()
                );
                let title = fields
                    .get("name")
                    .cloned()
                    .unwrap_or_else(|| name.clone());
                let relative = directory.strip_prefix(&root)?;
                let path = if relative.as_os_str().is_empty() {
                    ".".into()
                } else {
                    let relative = relative
                        .to_str()
                        .context("技能目录名称必须为 UTF-8")?;
                    #[cfg(unix)]
                    ensure!(!relative.contains('\\'), "技能目录名称不能包含反斜线");
                    relative.replace('\\', "/")
                };
                validate_relative(&path)?;
                let destination = self
                    .storage
                    .path()
                    .join(format!("package-{}", self.candidates.len()));
                let mut copied = 0;
                copy_directory(&directory, &destination, 0, &mut copied, &mut entries)?;
                total += copied;
                ensure!(
                    total <= MAX_SNAPSHOT_BYTES,
                    "技能来源总大小超过 128 MiB，请选择更具体的子目录"
                );
                let target = self.destination.join(&name);
                let conflict = existing.contains(&name) || fs::symlink_metadata(target).is_ok();
                self.candidates.push(SkillCandidate {
                    path,
                    name,
                    title,
                    description: description.chars().take(500).collect(),
                    conflict,
                });
                // A package owns its descendants; nested reference examples are
                // resources, not separately installable skills.
                continue;
            }
            let mut children = fs::read_dir(&directory)?
                .take(MAX_ENTRIES.saturating_sub(entries) + 1)
                .collect::<std::io::Result<Vec<_>>>()?;
            ensure!(
                children.len() + entries <= MAX_ENTRIES,
                "技能来源包含过多文件，请选择子目录后重试"
            );
            children.sort_by_key(|entry| entry.file_name());
            for entry in children.into_iter().rev() {
                entries += 1;
                ensure!(
                    entries <= MAX_ENTRIES,
                    "技能来源包含过多文件，请选择子目录后重试"
                );
                if ignored(&entry.file_name()) {
                    continue;
                }
                let kind = entry.file_type()?;
                // Unrelated repository links are never traversed; links inside
                // a discovered package are rejected by copy_directory instead.
                if kind.is_symlink() {
                    continue;
                }
                if kind.is_dir() {
                    directories.push((entry.path(), depth + 1));
                }
            }
        }
        let counts = self
            .candidates
            .iter()
            .fold(HashMap::new(), |mut counts, candidate| {
                *counts
                    .entry(candidate.name.clone())
                    .or_insert(0) += 1;
                counts
            });
        for candidate in &mut self.candidates {
            candidate.conflict |= counts[&candidate.name] > 1;
        }
        Ok(())
    }

    pub(super) fn install(
        &self,
        token: &str,
        session: &str,
        workspace: &Path,
        paths: &[String],
        existing: &HashSet<String>,
    ) -> Result<()> {
        ensure!(
            self.token == token && self.session == session && self.created.elapsed() < LIFETIME,
            "技能来源已过期，请重新扫描"
        );
        ensure!(
            fs::canonicalize(workspace)? == self.workspace,
            "工作区已切换，请重新扫描技能来源"
        );
        ensure!(
            !paths.is_empty() && paths.len() <= MAX_SKILLS,
            "请选择要添加的技能"
        );
        let target = self.destination.clone();
        reject_symlink_ancestors(&target)?;
        let mut selected = Vec::new();
        let mut names = HashSet::new();
        for path in paths {
            validate_relative(path)?;
            let (index, candidate) = self
                .candidates
                .iter()
                .enumerate()
                .find(|(_, candidate)| &candidate.path == path)
                .context("所选技能不属于此次扫描，请重新扫描")?;
            ensure!(
                !candidate.conflict && !existing.contains(&candidate.name),
                "技能 {} 已存在或名称重复，请先处理冲突",
                candidate.name
            );
            ensure!(names.insert(&candidate.name), "同一技能不能重复添加");
            let destination = target.join(&candidate.name);
            ensure!(
                fs::symlink_metadata(&destination)
                    .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
                "技能 {} 的目录已存在，请刷新后重试",
                candidate.name
            );
            selected.push((index, destination));
        }
        fs::create_dir_all(&target)?;
        reject_symlink_ancestors(&target)?;
        let staging = tempfile::Builder::new()
            .prefix(".import-")
            .tempdir_in(&target)?;
        for (index, _) in &selected {
            copy_directory(
                &self
                    .storage
                    .path()
                    .join(format!("package-{index}")),
                &staging.path().join(index.to_string()),
                0,
                &mut 0,
                &mut 0,
            )?;
        }
        // Reserve every destination without clobbering before copying any package.
        // If reservation or copying fails, remove only directories this operation
        // created, and report failure rather than silently accepting a partial set.
        let mut reserved = Vec::new();
        let result = (|| {
            for (_, destination) in &selected {
                fs::create_dir(destination).with_context(|| {
                    format!("无法添加 {}，目录可能已存在", destination.display())
                })?;
                reserved.push(destination.clone());
            }
            for (index, destination) in &selected {
                let package = staging.path().join(index.to_string());
                for entry in fs::read_dir(package)? {
                    let entry = entry?;
                    fs::rename(entry.path(), destination.join(entry.file_name()))?;
                }
            }
            Ok(())
        })();
        if result.is_err() {
            for directory in reserved.into_iter().rev() {
                let _ = fs::remove_dir_all(directory);
            }
        }
        result
    }
}

#[cfg(test)]
fn project_root(workspace: &Path) -> PathBuf {
    crate::instructions::project_root(workspace).unwrap_or_else(|| workspace.to_path_buf())
}

fn ignored(name: &std::ffi::OsStr) -> bool {
    name == ".git" || name == "node_modules" || name == "target" || name == ".venv"
}

fn validate_relative(path: &str) -> Result<()> {
    ensure!(
        !path.is_empty()
            && !path.contains('\\')
            && !path.contains('\0')
            && Path::new(path)
                .components()
                .all(|part| matches!(part, Component::Normal(_) | Component::CurDir)),
        "技能路径必须在此次扫描的目录内"
    );
    Ok(())
}

pub(super) fn reject_symlink_ancestors(path: &Path) -> Result<()> {
    for part in path.ancestors() {
        match fs::symlink_metadata(part) {
            Ok(metadata) => ensure!(
                !metadata.file_type().is_symlink(),
                "技能路径不能经过符号链接：{}",
                part.display()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn copy_directory(
    source: &Path,
    destination: &Path,
    depth: usize,
    bytes: &mut u64,
    entries: &mut usize,
) -> Result<()> {
    ensure!(depth <= MAX_DEPTH, "技能资源目录层级过深");
    fs::create_dir(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        if ignored(&entry.file_name()) {
            continue;
        }
        *entries += 1;
        ensure!(*entries <= MAX_ENTRIES, "技能来源包含过多文件");
        let metadata = entry.metadata()?;
        let kind = entry.file_type()?;
        ensure!(
            !kind.is_symlink(),
            "技能资源不能包含符号链接：{}",
            entry.path().display()
        );
        let output = destination.join(entry.file_name());
        if kind.is_dir() {
            copy_directory(&entry.path(), &output, depth + 1, bytes, entries)?;
        } else {
            ensure!(
                kind.is_file(),
                "技能资源必须为普通文件：{}",
                entry.path().display()
            );
            ensure!(
                metadata.len() <= MAX_FILE_BYTES,
                "技能资源文件超过 8 MiB：{}",
                entry.path().display()
            );
            // O_NOFOLLOW closes the final-component check/open race on supported
            // Unix workspace servers; parent paths were checked during traversal.
            let mut options = fs::OpenOptions::new();
            options.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.custom_flags(libc::O_NOFOLLOW);
            }
            let mut input = options
                .open(entry.path())?
                .take(MAX_FILE_BYTES + 1);
            let mut content = Vec::new();
            input.read_to_end(&mut content)?;
            ensure!(
                content.len() as u64 <= MAX_FILE_BYTES,
                "技能资源文件超过 8 MiB"
            );
            *bytes += content.len() as u64;
            ensure!(*bytes <= MAX_PACKAGE_BYTES, "单个技能资源超过 32 MiB");
            let mut output = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(output)?;
            output.write_all(&content)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                output.set_permissions(fs::Permissions::from_mode(
                    metadata.permissions().mode() & 0o777,
                ))?;
            }
        }
    }
    Ok(())
}
