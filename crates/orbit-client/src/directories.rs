use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

use super::{interaction::Progress, transport::Connection};

const MARKER: &[u8] = b"\0ORBIT-DIRECTORIES\0";

#[derive(Debug)]
pub struct Listing {
    pub path: String,
    pub directories: Vec<String>,
}

impl Listing {
    /// Return the parent of the resolved target path, or `None` at its filesystem root.
    pub fn parent_path(&self) -> Option<&str> {
        let path = self.path.trim_end_matches('/');
        if path.is_empty() {
            return None;
        }
        Some(path.rsplit_once('/').map_or(
            "/",
            |(parent, _)| {
                if parent.is_empty() { "/" } else { parent }
            },
        ))
    }

    /// Join a name returned by this listing without interpreting quotes or shell syntax.
    pub fn directory_path(&self, name: &str) -> String {
        format!("{}/{}", self.path.trim_end_matches('/'), name)
    }
}

pub async fn list(connection: &Connection, path: &str, progress: &Progress) -> Result<Listing> {
    progress.report(format!("读取远端目录：{path}"));
    let quoted = format!("'{}'", path.replace('\'', "'\\''"));
    let script = format!(
        r#"set -eu; p={quoted}; case "$p" in '~') p="$HOME";; '~/'*) p="$HOME/${{p#\~/}}";; esac; cd -- "$p"; printf '\000ORBIT-DIRECTORIES\000%s\000' "$PWD"; for p in ./* ./.[!.]* ./..?*; do if [ -d "$p" ]; then printf '%s\000' "${{p#./}}"; fi; done"#
    );
    let mut command = connection.target.command(
        &["sh".into(), "-c".into(), script],
        true,
        connection.interactive.as_deref(),
    )?;
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    super::interaction::timeout(std::time::Duration::from_secs(30), Some(progress), async {
        let result = super::command::output(command.spawn()?, Some(progress)).await?;
        ensure!(
            result.status.success(),
            "目录读取失败：{}",
            String::from_utf8_lossy(&result.stderr)
        );
        parse(&result.stdout)
    })
    .await
}

fn parse(bytes: &[u8]) -> Result<Listing> {
    let start = bytes
        .windows(MARKER.len())
        .position(|window| window == MARKER)
        .context("目标未返回目录列表")?
        + MARKER.len();
    let mut fields = bytes[start..]
        .split(|byte| *byte == 0)
        .filter(|field| !field.is_empty());
    let path = String::from_utf8(
        fields
            .next()
            .context("missing directory path")?
            .to_vec(),
    )?;
    let mut directories = fields
        .map(|field| String::from_utf8(field.to_vec()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    directories.sort();
    Ok(Listing { path, directories })
}

/// Resolve an explicit local directory, including quoted and home-relative input.
pub fn resolve(argument: &str, current: &Path, home: Option<&Path>) -> Result<PathBuf> {
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
mod resolution_tests {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_navigation_handles_root_and_literal_directory_names() {
        let root = Listing {
            path: "/".into(),
            directories: vec![],
        };
        assert_eq!(root.parent_path(), None);
        assert_eq!(root.directory_path("sample project"), "/sample project");
        let listing = Listing {
            path: "/srv/sandbox/".into(),
            directories: vec![],
        };
        assert_eq!(listing.parent_path(), Some("/srv"));
        assert_eq!(
            listing.directory_path("draft 'one'"),
            "/srv/sandbox/draft 'one'"
        );
        let child = Listing {
            path: "/sandbox".into(),
            directories: vec![],
        };
        assert_eq!(child.parent_path(), Some("/"));
    }

    #[test]
    fn directory_names_are_delimited_without_shell_or_line_interpretation() {
        let listing =
            parse(b"banner\n\0ORBIT-DIRECTORIES\0/home/me\0a b\0line\nbreak\0$(touch nope)\0")
                .unwrap();
        assert_eq!(listing.path, "/home/me");
        assert_eq!(listing.directories, ["$(touch nope)", "a b", "line\nbreak"]);
    }
}
