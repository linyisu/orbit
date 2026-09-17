//! Discovery and prompt injection for user-authored `AGENTS.md` files.
//!
//! Orbit deliberately only reads these files. It never creates instruction,
//! index, or memory files in a user workspace.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use orbit_model::types::ChatMessage;

pub(crate) const MAX_INSTRUCTION_BYTES: usize = 32 * 1024;
const MESSAGE_PREFIX: &str = "ORBIT_PROJECT_INSTRUCTIONS_V1\n";
const CANDIDATES: [&str; 2] = ["AGENTS.override.md", "AGENTS.md"];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct InstructionSource {
    pub path: PathBuf,
    pub bytes: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Instructions {
    pub sources: Vec<InstructionSource>,
    pub content: String,
    pub truncated: bool,
}

impl Instructions {
    pub(crate) fn source_summary(&self) -> String {
        if self.sources.is_empty() {
            return "未找到".into();
        }
        self.sources
            .iter()
            .map(|source| source.path.display().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn prompt(&self) -> Option<String> {
        (!self.content.is_empty()).then(|| {
            format!(
                "{MESSAGE_PREFIX}以下是用户提供的项目指令。请和当前用户请求一起遵守；后面的文件比前面的文件更具体。\n\n{}",
                self.content
            )
        })
    }
}

/// Load user-level instructions followed by project instructions from repository root to cwd.
/// A sibling `AGENTS.override.md` takes precedence over `AGENTS.md` at each level.
pub(crate) fn load_current() -> Instructions {
    let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    load_for(
        &cwd,
        default_user_instruction_dir().as_deref(),
        MAX_INSTRUCTION_BYTES,
    )
}

pub(crate) fn load_for(cwd: &Path, user_dir: Option<&Path>, max_bytes: usize) -> Instructions {
    let mut result = Instructions::default();
    if let Some(dir) = user_dir {
        add_directory(&mut result, dir, max_bytes);
    }

    let root = project_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let mut directories = cwd
        .ancestors()
        .take_while(|path| path.starts_with(&root))
        .map(Path::to_path_buf)
        .collect::<Vec<_>>();
    directories.reverse();
    for directory in directories {
        add_directory(&mut result, &directory, max_bytes);
        if result.truncated {
            break;
        }
    }
    result
}

/// Replace a previous Orbit-owned instruction message. This makes restored sessions pick up
/// moved workspaces and changed rules without duplicating the prompt after every turn.
pub(crate) fn inject(history: &mut Vec<ChatMessage>, instructions: &Instructions) {
    history.retain(|message| {
        !matches!(message, ChatMessage::System { content } if content.starts_with(MESSAGE_PREFIX))
    });
    if let Some(message) = instructions.prompt() {
        history.insert(0, ChatMessage::system(message));
    }
}

pub(crate) fn is_orbit_instruction(message: &ChatMessage) -> bool {
    matches!(message, ChatMessage::System { content } if content.starts_with(MESSAGE_PREFIX))
}

fn default_user_instruction_dir() -> Option<PathBuf> {
    if let Some(path) = env::var_os("ORBIT_HOME").filter(|value| !value.is_empty()) {
        return Some(PathBuf::from(path));
    }
    #[cfg(target_os = "macos")]
    {
        return env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join("Library/Application Support/orbit"));
    }
    #[cfg(windows)]
    {
        return env::var_os("APPDATA")
            .map(PathBuf::from)
            .map(|base| base.join("orbit"));
    }
    #[cfg(all(not(target_os = "macos"), not(windows)))]
    {
        let base = env::var_os("XDG_CONFIG_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;
        Some(base.join("orbit"))
    }
}

fn project_root(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .find(|directory| directory.join(".git").symlink_metadata().is_ok())
        .map(Path::to_path_buf)
}

fn add_directory(result: &mut Instructions, directory: &Path, max_bytes: usize) {
    let Some(path) = CANDIDATES
        .iter()
        .map(|name| directory.join(name))
        .find(|path| is_nonempty_regular_file(path))
    else {
        return;
    };
    let Ok(content) = fs::read_to_string(&path) else {
        return;
    };
    if result.content.len().saturating_add(content.len()) > max_bytes {
        result.truncated = true;
        return;
    }
    if !result.content.is_empty() {
        result.content.push_str("\n\n");
    }
    result
        .content
        .push_str(&format!("--- {} ---\n{content}", path.display()));
    result.sources.push(InstructionSource {
        path,
        bytes: content.len(),
    });
}

fn is_nonempty_regular_file(path: &Path) -> bool {
    path.symlink_metadata()
        .ok()
        .is_some_and(|metadata| metadata.file_type().is_file() && metadata.len() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        fs::write(path, text).unwrap();
    }

    #[test]
    fn loads_user_then_nested_project_rules_with_override_precedence() {
        let temp = tempfile::tempdir().unwrap();
        let user = temp.path().join("user");
        let root = temp.path().join("repo");
        let nested = root.join("crates/app");
        fs::create_dir_all(&user).unwrap();
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        write(&user.join("AGENTS.md"), "user rule");
        write(&root.join("AGENTS.md"), "root rule");
        write(&nested.join("AGENTS.md"), "ignored nested rule");
        write(&nested.join("AGENTS.override.md"), "nested override");

        let loaded = load_for(&nested, Some(&user), MAX_INSTRUCTION_BYTES);
        assert_eq!(
            loaded
                .sources
                .iter()
                .map(|source| source
                    .path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .to_string())
                .collect::<Vec<_>>(),
            vec!["AGENTS.md", "AGENTS.md", "AGENTS.override.md"]
        );
        assert!(loaded.content.find("user rule") < loaded.content.find("root rule"));
        assert!(loaded.content.find("root rule") < loaded.content.find("nested override"));
        assert!(!loaded.content.contains("ignored nested rule"));
    }

    #[test]
    fn enforces_a_whole_file_budget() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        write(&root.join("AGENTS.md"), "too long");

        let loaded = load_for(&root, None, 3);
        assert!(loaded.content.is_empty());
        assert!(loaded.sources.is_empty());
        assert!(loaded.truncated);
    }

    #[test]
    fn reinjection_replaces_only_orbit_owned_system_prompt() {
        let instructions = Instructions {
            sources: vec![InstructionSource {
                path: PathBuf::from("/repo/AGENTS.md"),
                bytes: 4,
            }],
            content: "rule".into(),
            truncated: false,
        };
        let mut history = vec![
            ChatMessage::system("base"),
            ChatMessage::system(format!("{MESSAGE_PREFIX}stale")),
            ChatMessage::user("hello"),
        ];
        inject(&mut history, &instructions);
        inject(&mut history, &instructions);
        assert_eq!(history.len(), 3);
        assert!(matches!(&history[0], ChatMessage::System { content } if content.contains("rule")));
        assert!(matches!(&history[1], ChatMessage::System { content } if content == "base"));
        assert!(matches!(&history[2], ChatMessage::User { content, .. } if content == "hello"));
    }
}
