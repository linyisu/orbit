use std::path::{Path, PathBuf};

use anyhow::Result;
use orbit_protocol::{Completion, search};
use orbit_store::store::Store;

pub(super) async fn complete(
    store: &Store,
    session: &str,
    workspace: &str,
    home: Option<&Path>,
    input: &str,
) -> Result<Vec<Completion>> {
    if let Some(argument) = input.strip_prefix("/cd ") {
        let workspace = PathBuf::from(workspace);
        let home = home.map(Path::to_path_buf);
        let argument = argument.to_owned();
        return tokio::task::spawn_blocking(move || {
            directories(&workspace, home.as_deref(), &argument)
        })
        .await?;
    }
    let words = input
        .split_whitespace()
        .collect::<Vec<_>>();
    let prefix = match words.as_slice() {
        ["/diff" | "/undo" | "/resume", ..] => words[0].to_owned(),
        ["/tasks", "start" | "done" | "block" | "cancel", ..] => words[..2].join(" "),
        ["/memory", "forget", ..] => words[..2].join(" "),
        _ => return Ok(Vec::new()),
    };
    let Some(argument) = input.strip_prefix(&format!("{prefix} ")) else {
        return Ok(Vec::new());
    };
    let values: Vec<(String, String)> = match words[0] {
        "/diff" | "/undo" => store
            .workspace_changes(session)
            .await?
            .into_iter()
            .filter(|change| words[0] != "/undo" || !change.undone)
            .map(|change| (change.change.path, "会话文件变更".into()))
            .collect(),
        "/resume" => {
            let Some(project) = store.project_at(workspace).await? else {
                return Ok(Vec::new());
            };
            let projects = store
                .equivalent_project_ids(&project.id)
                .await?;
            store
                .list_resumable()
                .await?
                .into_iter()
                .filter(|entry| projects.contains(&entry.project_id))
                .map(|entry| (entry.id, entry.title))
                .collect()
        }
        "/tasks" => {
            let Some(project) = store.project_at(workspace).await? else {
                return Ok(Vec::new());
            };
            store
                .tasks(&project.id, true)
                .await?
                .into_iter()
                .map(|task| (task.id.to_string(), task.title))
                .collect()
        }
        "/memory" => store
            .session_memories(session, false)
            .await?
            .into_iter()
            .map(|memory| (memory.id.to_string(), memory.title))
            .collect(),
        _ => Vec::new(),
    };
    let query = argument.trim().to_lowercase();
    let mut values = values;
    values.sort_by(|left, right| left.0.cmp(&right.0));
    let mut entries: Vec<_> = search::ranked(values, |(value, description)| {
        search::rank(value, &query)
            .into_iter()
            .chain(search::rank(description, &query))
            .min()
    })
    .into_iter()
    .map(|(value, description)| Completion {
        name: format!("{prefix} {value}"),
        display: None,
        description,
        continue_input: false,
    })
    .collect();
    entries.dedup_by(|left, right| left.name == right.name);
    entries.truncate(100);
    Ok(entries)
}

fn directories(workspace: &Path, home: Option<&Path>, argument: &str) -> Result<Vec<Completion>> {
    let typed = argument.trim_start();
    let quote = typed
        .chars()
        .next()
        .filter(|ch| matches!(ch, '\'' | '"'));
    let typed = if let Some(quote) = quote {
        typed.trim_matches(quote)
    } else {
        typed
    };
    let (prefix, partial) = typed
        .rsplit_once('/')
        .map_or(("", typed), |(parent, partial)| {
            (&typed[..parent.len() + 1], partial)
        });
    let directory = if let Some(relative) = prefix.strip_prefix("~/") {
        let Some(home) = home else {
            return Ok(Vec::new());
        };
        home.join(relative)
    } else {
        workspace.join(prefix)
    };
    let Ok(files) = std::fs::read_dir(directory) else {
        return Ok(Vec::new());
    };
    let mut names: Vec<_> = files
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| {
            (!name.starts_with('.') || partial.starts_with('.'))
                && !name.chars().any(char::is_control)
        })
        .collect();
    if prefix.is_empty() && "..".starts_with(partial) {
        names.push("..".into());
    }
    names.sort();
    let mut names = search::ranked(names, |name| search::rank(name, partial));
    names.truncate(100);
    Ok(names
        .into_iter()
        .map(|name| {
            let path = format!("{prefix}{name}/");
            let path = if quote.is_some()
                || path.contains(char::is_whitespace)
                || path.starts_with(['\'', '"'])
            {
                shlex::try_quote(&path).map_or(path.clone(), |value| value.into_owned())
            } else {
                path
            };
            Completion {
                name: format!("/cd {path}"),
                display: Some(format!("{name}/")),
                description: String::new(),
                continue_input: true,
            }
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_completion_uses_target_workspace_and_quotes_names() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("target folder")).unwrap();
        std::fs::create_dir(root.path().join(".hidden")).unwrap();
        let entries = directories(root.path(), None, "target").unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(
            shlex::split(&entries[0].name).unwrap(),
            ["/cd", "target folder/"]
        );
        assert_eq!(
            directories(root.path(), None, ".h").unwrap()[0]
                .display
                .as_deref(),
            Some(".hidden/")
        );
    }

    #[test]
    fn directories_rank_prefix_substring_and_subsequence_before_limiting_results() {
        let root = tempfile::tempdir().unwrap();
        for name in [
            "Tests",
            "ATests",
            "Templates",
            "T_e_s",
            ".Tests",
            "unrelated",
        ] {
            std::fs::create_dir(root.path().join(name)).unwrap();
        }
        for input in ["tes", "~/Tes", "'~/TES"] {
            let entries = directories(root.path(), Some(root.path()), input).unwrap();
            assert_eq!(
                entries
                    .iter()
                    .map(|entry| entry.display.as_deref().unwrap())
                    .collect::<Vec<_>>(),
                ["Tests/", "ATests/", "Templates/", "T_e_s/"]
            );
            assert_eq!(
                shlex::split(&entries[0].name)
                    .unwrap()
                    .len(),
                2
            );
        }
        for index in 0..110 {
            std::fs::create_dir(
                root.path()
                    .join(format!("ATests{index:03}")),
            )
            .unwrap();
        }
        let entries = directories(root.path(), None, "tes").unwrap();
        assert_eq!(entries.len(), 100);
        assert_eq!(entries[0].display.as_deref(), Some("Tests/"));
    }
}
