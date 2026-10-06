use std::path::{Path, PathBuf};

use anyhow::Result;
use orbit_protocol::Completion;
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
    let words = input.split_whitespace().collect::<Vec<_>>();
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
            let current = store.session(session).await?;
            let projects = store.equivalent_project_ids(&current.project_id).await?;
            store
                .list_resumable()
                .await?
                .into_iter()
                .filter(|entry| projects.contains(&entry.project_id))
                .map(|entry| (entry.id, entry.title))
                .collect()
        }
        "/tasks" => store
            .tasks(&store.session(session).await?.project_id, true)
            .await?
            .into_iter()
            .map(|task| (task.id.to_string(), task.title))
            .collect(),
        "/memory" => store
            .session_memories(session, false)
            .await?
            .into_iter()
            .map(|memory| (memory.id.to_string(), memory.title))
            .collect(),
        _ => Vec::new(),
    };
    let query = argument.trim().to_lowercase();
    let mut entries: Vec<_> = values
        .into_iter()
        .filter(|(value, description)| {
            value.to_lowercase().contains(&query) || description.to_lowercase().contains(&query)
        })
        .map(|(value, description)| Completion {
            name: format!("{prefix} {value}"),
            display: None,
            description,
            continue_input: false,
        })
        .collect();
    entries.sort_by(|left, right| left.name.cmp(&right.name));
    entries.dedup_by(|left, right| left.name == right.name);
    entries.truncate(100);
    Ok(entries)
}

fn directories(workspace: &Path, home: Option<&Path>, argument: &str) -> Result<Vec<Completion>> {
    let typed = argument.trim_start();
    let quote = typed.chars().next().filter(|ch| matches!(ch, '\'' | '"'));
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
                && name.to_lowercase().contains(&partial.to_lowercase())
        })
        .collect();
    if prefix.is_empty() && "..".starts_with(partial) {
        names.push("..".into());
    }
    names.sort();
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
}
