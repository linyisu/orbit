use std::path::{Path, PathBuf};

use orbit_runtime::filesystem::{ResolvedPath, Workspace};

mod shell;

/// Plan literal commands before approval, independently of filesystem state.
/// Auto mode must execute the same pinned programs that its scope check proves.
pub(crate) fn pinned_local_command(command: &str) -> Option<String> {
    Some(shell::plan(command)?.script)
}

/// Validate every input/output path in a literal command plan against the
/// working directory and the workspace's protected names and symlink boundary.
pub(crate) fn local_workspace_command(
    command: &str,
    cwd: Option<&str>,
    root: &Path,
) -> Option<String> {
    let plan = shell::plan(command)?;

    let workspace = Workspace::new(root).ok()?;
    let root = workspace.root();
    let cwd = root.join(cwd.unwrap_or(".")).canonicalize().ok()?;
    if !cwd.starts_with(root) || !cwd.is_dir() {
        return None;
    }

    if !plan
        .paths
        .iter()
        .all(|path| local_target(path, &cwd, &workspace))
    {
        return None;
    }

    Some(plan.script)
}

fn local_target(word: &str, cwd: &Path, workspace: &Workspace) -> bool {
    if word.is_empty() {
        return false;
    }
    let root = workspace.root();
    let path = Path::new(word);
    let requested = if path.is_absolute() {
        let Ok(relative) = path.strip_prefix("/workspace") else {
            return false;
        };
        root.join(relative)
    } else {
        cwd.join(path)
    };
    let Ok(relative) = requested.strip_prefix(root) else {
        return false;
    };
    if relative.as_os_str().is_empty() {
        return true;
    }
    let Ok(relative) = workspace.checked_relative(relative) else {
        return false;
    };

    // Check every ancestor, including symlinks and missing parents for -p.
    // A dangling link cannot be assumed to be an ordinary new directory.
    let mut prefix = PathBuf::from(root);
    for component in relative.components() {
        prefix.push(component);
        if std::fs::symlink_metadata(&prefix).is_ok_and(|meta| meta.file_type().is_symlink())
            && prefix.canonicalize().is_err()
        {
            return false;
        }

        let Ok(resolved) = ResolvedPath::resolve(&prefix, None) else {
            return false;
        };
        let Ok(canonical) = resolved.canonical().strip_prefix(root) else {
            return false;
        };
        if !canonical.as_os_str().is_empty() && workspace.checked_relative(canonical).is_err() {
            return false;
        }
    }

    true
}

/// shlex handles quoting and escapes. The shell grammar splits supported
/// operators first; this guard rejects evaluation inside each literal segment.
fn literal_words(command: &str) -> Option<Vec<String>> {
    let mut quote = None;
    let mut chars = command.chars();
    while let Some(ch) = chars.next() {
        if ch.is_control() && ch != '\t' {
            return None;
        }
        match (quote, ch) {
            (Some('\''), '\'') | (Some('"'), '"') => quote = None,
            (Some('\''), _) => {}
            (_, '\\') => {
                let escaped = chars.next()?;
                if escaped.is_control() {
                    return None;
                }
            }

            (Some('"'), '$' | '`') => return None,
            (Some('"'), _) => {}
            (None, '\'' | '"') => quote = Some(ch),
            (
                None,
                '$' | '`' | ';' | '&' | '|' | '<' | '>' | '(' | ')' | '*' | '?' | '[' | ']' | '{'
                | '}' | '~' | '#',
            ) => return None,

            _ => {}
        }
    }

    shlex::split(command)
}

#[cfg(test)]
mod tests {
    use orbit_runtime::execution::process::system_executable;

    use super::*;

    #[test]
    fn local_text_commands_check_all_pipeline_inputs_outputs_and_and_stages() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("outside")).unwrap();
        std::fs::create_dir(root.path().join(".git")).unwrap();
        std::os::unix::fs::symlink(".git", root.path().join("metadata")).unwrap();
        for source in [
            "echo hello > demo.txt",
            "echo hello >> demo.txt",
            "printf '%s\\n' 'a | b > c' | tee -a demo.txt other.txt",
            "mkdir -p demo && echo hello > demo/a.txt && cat demo/a.txt | tee demo/b.txt",
            "echo data | cat | tee /workspace/file > /workspace/output",
            "echo '$(literal)' > 'file with spaces'",
        ] {
            assert!(
                local_workspace_command(source, None, root.path()).is_some(),
                "{source}"
            );
        }

        for source in [
            "echo data > ../out",
            "echo data > /tmp/out",
            "echo data > /workspace2/out",
            "echo data | tee local.txt outside/out",
            "cat outside/input | tee local.txt",
            "echo data > .git/config",
            "echo data > metadata/config",
            "echo data | tee .env",
            "echo data > .orbit-write-temporary",
            "echo data | tee /etc/passwd",
        ] {
            assert!(
                local_workspace_command(source, None, root.path()).is_none(),
                "{source}"
            );
        }
    }

    #[test]
    fn directory_plan_keeps_literal_paths_and_pins_the_system_program() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("existing")).unwrap();
        for command in [
            "mkdir -p demo/nested",
            "mkdir --parents /workspace/demo/nested",
            "mkdir -pv first '中文目录' 'a; b'",
            "mkdir 'folder with spaces' --parents second",
            "mkdir -p -- -leading-dash",
            r#"/bin/mkdir -p "quoted \\" '$(literal)'"#,
            "mkdir -p /workspace",
        ] {
            let plan = local_workspace_command(command, Some("existing"), root.path())
                .unwrap_or_else(|| panic!("missing plan for {command}"));
            let words = shlex::split(&plan).unwrap();
            assert_eq!(Path::new(&words[0]), system_executable("mkdir").unwrap());
            assert_eq!(words[1..], shlex::split(command).unwrap()[1..]);
            assert_eq!(
                pinned_local_command(command).as_deref(),
                Some(plan.as_str())
            );
        }

        assert!(
            root.path()
                .join("existing")
                .read_dir()
                .unwrap()
                .next()
                .is_none()
        );
    }

    #[test]
    fn directory_plan_rejects_shell_evaluation_and_outside_or_protected_targets() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join(".git")).unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("outside")).unwrap();
        std::os::unix::fs::symlink("missing", root.path().join("dangling")).unwrap();
        std::os::unix::fs::symlink(".git", root.path().join("metadata")).unwrap();
        for command in [
            "mkdir -p demo && touch file",
            "mkdir -p demo; rm file",
            "mkdir -p demo\nmkdir other",
            "mkdir -p demo &",
            "env mkdir demo",
            "mkdir $TARGET",
            "mkdir \"$TARGET\"",
            "mkdir $(pwd)/demo",
            "mkdir `pwd`/demo",
            "mkdir demo*",
            "mkdir ~",
            "mkdir demo # comment",
            "mkdir -m 777 demo",
            "mkdir -p",
            "mkdir ''",
            "mkdir 'unclosed",
            "mkdir -p ../outside",
            "mkdir -p /tmp/elsewhere",
            "mkdir -p /workspace2/elsewhere",
            "mkdir -p demo outside/new/nested",
            "mkdir -p dangling/new",
            "mkdir -p .git/objects",
            "mkdir -p metadata/objects",
            "mkdir -p .env/nested",
            "mkdir -p .env.example/nested",
            "mkdir -p .orbit-write-temporary",
        ] {
            assert!(
                local_workspace_command(command, None, root.path()).is_none(),
                "{command}"
            );
        }

        assert!(
            local_workspace_command("mkdir demo", outside.path().to_str(), root.path()).is_none()
        );
        assert!(local_workspace_command("mkdir demo", Some("outside"), root.path()).is_none());
    }

    #[test]
    fn pinning_is_independent_of_scope_so_path_changes_cannot_restore_path_lookup() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let alias = root.path().join("alias");
        std::os::unix::fs::symlink(outside.path(), &alias).unwrap();
        let command = "mkdir -p alias/demo";
        let pinned = pinned_local_command(command).unwrap();
        assert!(local_workspace_command(command, None, root.path()).is_none());
        std::fs::remove_file(&alias).unwrap();
        std::fs::create_dir(&alias).unwrap();
        assert_eq!(
            local_workspace_command(command, None, root.path()),
            Some(pinned)
        );
    }
}
