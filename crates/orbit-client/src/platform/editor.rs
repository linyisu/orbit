//! External editors own the terminal temporarily; their result remains a draft.
use std::{io::Write, path::Path, process::Stdio};

use anyhow::{Context, Result, ensure};

#[derive(Clone, Debug)]
pub struct Editor {
    command: String,
}

impl Editor {
    pub fn new(command: String) -> Self {
        Self { command }
    }

    pub async fn edit(&self, draft: &str, directory: &Path) -> Result<String> {
        let words = shlex::split(&self.command).context("编辑器命令包含未闭合的引号")?;
        let (program, arguments) = words
            .split_first()
            .context("请配置 VISUAL 或 EDITOR")?;
        let mut file = tempfile::Builder::new()
            .prefix("orbit-draft-")
            .suffix(".md")
            .tempfile()?;
        file.write_all(draft.as_bytes())?;
        file.flush()?;

        let status = tokio::process::Command::new(program)
            .args(arguments)
            .arg(file.path())
            .current_dir(directory)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .status()
            .await
            .with_context(|| format!("无法启动编辑器 {program}"))?;

        ensure!(status.success(), "编辑器退出失败：{status}；原草稿已保留");

        std::fs::read_to_string(file.path()).context("编辑器返回的内容不是有效的 UTF-8 文本")
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn editor_round_trips_drafts_and_does_not_execute_shell_metacharacters() {
        let directory = tempfile::tempdir().unwrap();
        let script = directory.path().join("fake editor.sh");
        std::fs::write(&script, "printf '\\nchanged' >> \"$1\"\n").unwrap();
        let editor = Editor::new(format!("/bin/sh '{}'", script.display()));

        let result = editor
            .edit("original 中文", directory.path())
            .await
            .unwrap();

        assert_eq!(result, "original 中文\nchanged");

        let unchanged = Editor::new("/bin/true ; touch injected".into())
            .edit("draft", directory.path())
            .await
            .unwrap();
        assert_eq!(unchanged, "draft");
        assert!(
            !directory
                .path()
                .join("injected")
                .exists()
        );
        assert!(
            Editor::new("/bin/false".into())
                .edit("draft", directory.path())
                .await
                .is_err()
        );
        assert!(
            Editor::new("'unclosed".into())
                .edit("draft", directory.path())
                .await
                .is_err()
        );
    }
}
