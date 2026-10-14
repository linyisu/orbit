use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

pub(super) fn resolve(argument: &str, current: &Path, home: Option<&Path>) -> Result<PathBuf> {
    let argument = argument.trim();
    let path = if argument.is_empty() || argument == "~" {
        home.context("未配置用户主目录；请使用 /cd <路径>")?
            .to_path_buf()
    } else if let Some(relative) = argument.strip_prefix("~/") {
        home.context("未配置用户主目录")?
            .join(relative)
    } else {
        let path = if argument.starts_with(['\'', '"']) {
            let words = shlex::split(argument).context("路径引号未闭合")?;
            ensure!(words.len() == 1, "请提供一个目录路径");
            PathBuf::from(&words[0])
        } else {
            PathBuf::from(argument)
        };
        current.join(path)
    };
    let path = path
        .canonicalize()
        .with_context(|| format!("无法访问目录 {}", path.display()))?;
    ensure!(path.is_dir(), "目标不是目录：{}", path.display());

    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_paths_are_explicit_and_support_spaces_and_parent_components() {
        let root = tempfile::tempdir().unwrap();
        let child = root.path().join("project space");
        std::fs::create_dir(&child).unwrap();
        std::fs::write(root.path().join("file"), "data").unwrap();

        assert_eq!(resolve("project space", root.path(), None).unwrap(), child);
        assert_eq!(
            resolve("\"project space\"", root.path(), None).unwrap(),
            child
        );
        assert_eq!(
            resolve("..", &child, None).unwrap(),
            root.path().canonicalize().unwrap()
        );
        assert_eq!(
            resolve("~/project space", &child, Some(root.path())).unwrap(),
            child
        );
        assert!(resolve("file", root.path(), None).is_err());
        assert!(resolve("missing", root.path(), None).is_err());
    }
}
