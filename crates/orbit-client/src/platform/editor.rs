//! External editors own the terminal temporarily; their result remains a draft.
use std::{ffi::OsString, io::Write, path::Path, process::Stdio};

use anyhow::{Context, Result, ensure};

#[derive(Clone, Debug)]
pub struct Editor {
    command: String,
}

impl Editor {
    pub fn new(command: String) -> Self {
        Self { command }
    }

    /// Yield the terminal to an editor and return its successful UTF-8 edits.
    /// Windows uses native argument quoting; Unix uses shell words without invoking a shell.
    /// Errors leave the caller's draft intact; dropping the future kills the editor process.
    pub async fn edit(&self, draft: &str, directory: &Path) -> Result<String> {
        let words = split_command(&self.command)?;
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
            .with_context(|| format!("无法启动编辑器 {}", program.to_string_lossy()))?;

        ensure!(status.success(), "编辑器退出失败：{status}；原草稿已保留");

        std::fs::read_to_string(file.path()).context("编辑器返回的内容不是有效的 UTF-8 文本")
    }
}

#[cfg(not(windows))]
fn split_command(command: &str) -> Result<Vec<OsString>> {
    Ok(shlex::split(command)
        .context("编辑器命令包含未闭合的引号")?
        .into_iter()
        .map(OsString::from)
        .collect())
}

#[cfg(windows)]
fn split_command(command: &str) -> Result<Vec<OsString>> {
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::Win32::{Foundation::LocalFree, UI::Shell::CommandLineToArgvW};

    let command = command.trim_start_matches([' ', '\t']);
    if command.is_empty() {
        return Ok(Vec::new());
    }
    ensure!(!command.contains('\0'), "编辑器命令包含无效的空字符");
    let command = command
        .encode_utf16()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut count = 0;
    // SAFETY: command is NUL-terminated; count points to valid writable storage.
    let arguments = unsafe { CommandLineToArgvW(command.as_ptr(), &mut count) };
    if arguments.is_null() {
        return Err(std::io::Error::last_os_error()).context("无法解析编辑器命令");
    }
    let mut words = Vec::new();
    for index in 0..count as usize {
        // SAFETY: the successful call owns count NUL-terminated strings until LocalFree.
        let word = unsafe {
            let start = *arguments.add(index);
            let mut length = 0;
            while *start.add(length) != 0 {
                length += 1;
            }
            OsString::from_wide(std::slice::from_raw_parts(start, length))
        };
        words.push(word);
    }
    // SAFETY: CommandLineToArgvW returns one LocalAlloc block containing this entire array.
    unsafe { LocalFree(arguments.cast()) };
    Ok(words)
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

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn native_editor_arguments_preserve_paths_quotes_and_literal_shell_characters() {
        let words = split_command(
            r#""C:\Program Files\编辑器\editor.exe" --wait "C:\work\设计 文档.md" ; & calc"#,
        )
        .unwrap();
        assert_eq!(
            words,
            [
                r"C:\Program Files\编辑器\editor.exe",
                "--wait",
                r"C:\work\设计 文档.md",
                ";",
                "&",
                "calc",
            ]
            .map(OsString::from)
        );
        assert_eq!(
            split_command(r"C:\Tools\editor.exe --wait").unwrap(),
            [r"C:\Tools\editor.exe", "--wait"].map(OsString::from)
        );
        assert_eq!(
            split_command(r#"editor.exe "say \"hello\"""#).unwrap(),
            ["editor.exe", "say \"hello\""].map(OsString::from)
        );
        assert!(split_command(" \t").unwrap().is_empty());
        assert!(split_command("editor.exe\0hidden").is_err());
    }

    #[test]
    #[ignore = "subprocess fixture for the native editor adapter"]
    fn native_editor_fixture() {
        let Some(path) = std::env::args_os()
            .last()
            .map(std::path::PathBuf::from)
        else {
            return;
        };
        if !path.file_name().is_some_and(|name| {
            name.to_string_lossy()
                .starts_with("orbit-draft-")
        }) {
            return;
        }
        std::fs::OpenOptions::new()
            .append(true)
            .open(path)
            .unwrap()
            .write_all(b"\nchanged")
            .unwrap();
    }

    #[tokio::test]
    async fn native_editor_roundtrips_a_draft_through_a_quoted_unicode_executable() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("编辑器 folder");
        std::fs::create_dir(&directory).unwrap();
        let executable = directory.join("编辑器.exe");
        std::fs::copy(std::env::current_exe().unwrap(), &executable).unwrap();
        let editor = Editor::new(format!(
            "\"{}\" --ignored --exact platform::editor::tests::native_editor_fixture --nocapture",
            executable.display()
        ));
        assert_eq!(
            editor
                .edit("original 中文", root.path())
                .await
                .unwrap(),
            "original 中文\nchanged"
        );
    }
}
