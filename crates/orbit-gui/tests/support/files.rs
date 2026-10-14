//! Real fixture IO is limited to the explicitly selected, existing test workspace.
use base64::Engine as _;
use serde_json::{Value, json};
use std::{
    io::Write as _,
    path::{Component, Path, PathBuf},
};

fn checked_path(workspace: &str, path: &str) -> anyhow::Result<PathBuf> {
    let root = Path::new(workspace).canonicalize()?;
    anyhow::ensure!(root.is_dir(), "工作区目录不存在");
    let requested = Path::new(path);
    let relative = if requested.is_absolute() {
        requested.strip_prefix(&root)?
    } else {
        requested
    };
    let mut result = root;
    for component in relative.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(name) => {
                anyhow::ensure!(
                    !matches!(
                        name.to_str(),
                        Some(".git" | ".orbit" | ".agents" | ".codex")
                    ),
                    "受保护的文件路径"
                );
                result.push(name);
                if let Ok(metadata) = std::fs::symlink_metadata(&result) {
                    anyhow::ensure!(!metadata.file_type().is_symlink(), "不允许符号链接路径");
                }
            }
            _ => anyhow::bail!("请使用不含 .. 的工作区相对路径"),
        }
    }
    Ok(result)
}

pub(super) fn read_file(workspace: &str, path: &str) -> Value {
    if !Path::new(workspace).is_dir() {
        return json!({
            "type": "file",
            "value": "aGVsbG8gZ3VpCg=="
        });
    }
    match checked_path(workspace, path).and_then(|path| {
        if !path.exists() {
            return Ok(b"hello gui\n".to_vec());
        }
        anyhow::ensure!(path.is_file(), "只能读取普通文件");
        let bytes = std::fs::read(path)?;
        anyhow::ensure!(bytes.len() <= 20 * 1_024 * 1_024, "文件超过 20 MiB");
        Ok(bytes)
    }) {
        Ok(bytes) => {
            json!({
                "type": "file",
                "value": base64::engine::general_purpose::STANDARD.encode(bytes)
            })
        }
        Err(error) => json!({
            "type": "error",
            "value": error.to_string()
        }),
    }
}

pub(super) fn execute(workspace: &str, request: &Value) -> Value {
    use orbit_protocol::files::{Action, Entry, EntryKind, Snapshot};
    let result = (|| -> anyhow::Result<Snapshot> {
        anyhow::ensure!(
            request["expected_workspace"]
                .as_str()
                .is_none_or(|expected| expected == workspace),
            "工作区已切换，请重新打开文件"
        );
        anyhow::ensure!(Path::new(workspace).is_dir(), "工作区目录不存在");
        let action: Action = serde_json::from_value(request["action"].clone())?;
        match action {
            Action::List {
                path,
                offset,
                include_hidden,
            } => {
                let target = checked_path(workspace, &path)?;
                anyhow::ensure!(offset <= 20_000, "目录分页位置无效");
                let mut entries = vec![];
                for item in std::fs::read_dir(target)? {
                    let item = item?;
                    let Some(name) = item
                        .file_name()
                        .to_str()
                        .map(str::to_owned)
                    else {
                        continue;
                    };
                    if !include_hidden && name.starts_with('.') {
                        continue;
                    }
                    let relative = if path == "." {
                        PathBuf::from(&name)
                    } else {
                        Path::new(&path).join(&name)
                    };
                    if checked_path(workspace, &relative.to_string_lossy()).is_err() {
                        continue;
                    }
                    let kind = item.file_type()?;
                    let kind = if kind.is_dir() {
                        EntryKind::Directory
                    } else if kind.is_file() {
                        EntryKind::File
                    } else {
                        continue;
                    };
                    entries.push(Entry {
                        path: relative.to_string_lossy().into_owned(),
                        kind,
                    });
                }
                entries.sort_by(|left, right| left.path.cmp(&right.path));
                let next_offset = (entries.len() > offset + 200).then_some(offset + 200);
                let entries = entries
                    .into_iter()
                    .skip(offset)
                    .take(200)
                    .collect();
                Ok(Snapshot::Directory {
                    path,
                    entries,
                    next_offset,
                    scan_truncated: false,
                    readonly: false,
                })
            }
            Action::Save {
                path,
                original,
                content,
            } => {
                anyhow::ensure!(
                    content.len() <= 1_024 * 1_024 && !content.contains('\0'),
                    "文件不是可保存的 UTF-8 文本"
                );
                let target = checked_path(workspace, &path)?;
                let before = std::fs::read_to_string(&target)?;
                anyhow::ensure!(
                    before.len() <= 1_024 * 1_024 && !before.contains('\0'),
                    "文件不是可编辑文本"
                );
                anyhow::ensure!(before == original, "文件已被其他程序修改，编辑内容已保留");
                let parent = target
                    .parent()
                    .ok_or_else(|| anyhow::anyhow!("文件目录无效"))?;
                let mut file = tempfile::NamedTempFile::new_in(parent)?;
                file.as_file()
                    .set_permissions(std::fs::metadata(&target)?.permissions())?;
                file.write_all(content.as_bytes())?;
                file.as_file().sync_all()?;
                anyhow::ensure!(
                    std::fs::read_to_string(&target)? == original,
                    "文件保存冲突"
                );
                file.persist(target)?;
                Ok(Snapshot::Saved { path, content })
            }
        }
    })();
    match result {
        Ok(snapshot) => json!({
            "type": "files",
            "value": snapshot
        }),
        Err(error) => json!({
            "type": "error",
            "value": error.to_string()
        }),
    }
}
