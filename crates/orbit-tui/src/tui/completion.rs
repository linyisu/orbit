use std::{
    cell::RefCell,
    path::{Path, PathBuf},
};

use super::commands;

pub use orbit_protocol::Completion as Candidate;

pub(super) fn cached_directories(
    input: &str,
    previous: &str,
    entries: &[Candidate],
) -> Option<Vec<Candidate>> {
    fn query(input: &str) -> Option<(&str, &str)> {
        let typed = input.strip_prefix("/cd ")?.trim_start();
        let typed = match typed.chars().next() {
            Some(quote @ ('\'' | '"')) => typed.trim_matches(quote),
            _ => typed,
        };
        Some(
            typed
                .rsplit_once('/')
                .map_or(("", typed), |(parent, partial)| {
                    (&typed[..parent.len() + 1], partial)
                }),
        )
    }

    let (parent, partial) = query(input)?;
    if parent != query(previous)?.0 {
        return None;
    }
    let mut entries: Vec<_> = entries.iter().collect();
    entries.sort_by(|left, right| left.display.cmp(&right.display));
    Some(
        super::search::ranked(entries, |entry| {
            let name = entry
                .display
                .as_deref()?
                .trim_end_matches('/');
            if name.starts_with('.') && name != ".." && !partial.starts_with('.') {
                return None;
            }
            super::search::rank(name, partial)
        })
        .into_iter()
        .cloned()
        .collect(),
    )
}

#[derive(Clone, Debug, Default)]
pub struct DirectoryCache {
    entries: RefCell<Option<DirectorySnapshot>>,
}

#[derive(Clone, Debug)]
struct DirectorySnapshot {
    input: String,
    workspace: PathBuf,
    home: Option<PathBuf>,
    candidates: Vec<Candidate>,
}

impl DirectoryCache {
    pub fn matching(&self, input: &str, workspace: &Path, home: Option<&Path>) -> Vec<Candidate> {
        let mut cache = self.entries.borrow_mut();
        if let Some(snapshot) = cache.as_ref()
            && snapshot.input == input
            && snapshot.workspace == workspace
            && snapshot.home.as_deref() == home
        {
            return snapshot.candidates.clone();
        }

        let candidates = directories(input, workspace, home);
        *cache = Some(DirectorySnapshot {
            input: input.into(),
            workspace: workspace.into(),
            home: home.map(Path::to_path_buf),
            candidates: candidates.clone(),
        });
        candidates
    }
}

fn directories(input: &str, workspace: &Path, home: Option<&Path>) -> Vec<Candidate> {
    let typed = input.trim_start();
    if typed == "~" && home.is_some() {
        return vec![Candidate {
            name: "/cd ~/".into(),
            display: Some("~/".into()),
            description: String::new(),
            continue_input: true,
        }];
    }

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
    let directory = if prefix == "~/" || prefix.starts_with("~/") {
        let Some(home) = home else {
            return Vec::new();
        };
        home.join(&prefix[2..])
    } else {
        workspace.join(prefix)
    };
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut names = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| !name.starts_with('.') || partial.starts_with('.'))
        .filter(|name| !name.chars().any(char::is_control))
        .collect::<Vec<_>>();

    names.sort();

    if prefix.is_empty() && "..".starts_with(partial) {
        names.insert(0, "..".into());
    }

    super::search::ranked(names, |name| super::search::rank(name, partial))
        .into_iter()
        .map(|name| {
            let path = format!("{prefix}{name}/");
            let path = if quote.is_some() || path.starts_with(['\'', '"']) {
                shlex::try_quote(&path).map_or(path.clone(), |value| value.into_owned())
            } else {
                path
            };
            Candidate {
                name: format!("/cd {path}"),
                display: Some(format!("{name}/")),
                description: String::new(),
                continue_input: true,
            }
        })
        .collect()
}

pub fn matching(input: &str, models: &[String], skills: &[(String, String)]) -> Vec<Candidate> {
    let Some((command, argument)) = input.split_once(char::is_whitespace) else {
        return commands::matching(input)
            .into_iter()
            .map(|command| Candidate {
                name: command.name.into(),
                display: None,
                description: command.description.into(),
                continue_input: false,
            })
            .collect();
    };
    let argument = argument.trim_start();
    if matches!(command, "/ssh" | "/wsl") {
        let (hosts, distributions) = crate::workspace::discovery::snapshot();
        if argument.contains(char::is_whitespace) {
            return Vec::new();
        }
        let (user, query) = argument
            .rsplit_once('@')
            .map_or((String::new(), argument), |(user, host)| {
                (format!("{user}@"), host)
            });
        let choices: Vec<_> = if command == "/ssh" {
            hosts
                .into_iter()
                .map(|host| {
                    (
                        format!("{user}{}", host.name),
                        format!("{} {} {}", host.hostname, host.user, host.port),
                    )
                })
                .collect()
        } else {
            distributions
                .into_iter()
                .map(|name| (name, "WSL 发行版".into()))
                .collect()
        };
        return super::search::ranked(choices.iter(), |(name, _)| super::search::rank(name, query))
            .into_iter()
            .map(|(name, description)| Candidate {
                name: format!("{command} {}", shlex::try_quote(name).unwrap()),
                display: Some(name.clone()),
                description: description.clone(),
                continue_input: false,
            })
            .collect();
    }

    orbit_client::completion::arguments(input, models, skills)
}

pub fn select_with_mouse(state: &mut super::app::UiState, mouse: &crossterm::event::MouseEvent) {
    use crossterm::event::{MouseButton, MouseEventKind};

    let count = state.menu().len();
    if count == 0 {
        return;
    }

    state.menu_index = state.menu_index.min(count - 1);
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            state.menu_offset = state.menu_offset.saturating_sub(3);
            state.menu_manual_scroll = true;
            state.pending_list_hover = Some(*mouse);
        }

        MouseEventKind::ScrollDown => {
            state.menu_offset = state
                .menu_offset
                .saturating_add(3)
                .min(count.saturating_sub(state.menu_rows.len().max(1)));
            state.menu_manual_scroll = true;
            state.pending_list_hover = Some(*mouse);
        }

        MouseEventKind::Moved | MouseEventKind::Down(MouseButton::Left) => {
            if let Some((_, index)) = state
                .menu_rows
                .iter()
                .find(|(area, _)| area.contains((mouse.column, mouse.row).into()))
            {
                state.menu_index = *index;
            }
        }

        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_selects_rows_and_wheel_scrolls_without_changing_selection() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let mut state = super::super::app::UiState::new("model");
        state.input.set("/".into());
        state.menu_rows = vec![(ratatui::layout::Rect::new(2, 3, 40, 1), 6)];
        let mut mouse = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 3,
            row: 3,
            modifiers: KeyModifiers::NONE,
        };

        select_with_mouse(&mut state, &mouse);
        assert_eq!(state.menu_index, 6);
        assert_eq!(state.input.text, "/");
        state.menu_index = 0;
        mouse.kind = MouseEventKind::Moved;
        select_with_mouse(&mut state, &mouse);
        assert_eq!(state.menu_index, 6);
        assert_eq!(state.input.text, "/");

        state.menu_index = state.menu().len() - 1;
        mouse.kind = MouseEventKind::ScrollDown;
        select_with_mouse(&mut state, &mouse);
        assert_eq!(state.menu_index, state.menu().len() - 1);
        assert_eq!(state.menu_offset, 3);
        assert!(state.menu_manual_scroll);
    }

    #[test]
    fn completes_models_across_providers_and_finite_arguments() {
        let models = vec!["one::fast".into(), "two::fast".into()];
        let skills = vec![("review".into(), "代码审查".into())];

        assert_eq!(matching("/model f", &models, &skills).len(), 3);
        assert_eq!(
            matching("/model two::", &models, &skills)[0].name,
            "/model two::fast"
        );
        assert_eq!(matching("/permissions a", &models, &skills).len(), 2);
        assert_eq!(matching("/mode p", &models, &skills)[0].name, "/mode plan");
        assert_eq!(matching("/plan v", &models, &skills)[0].name, "/plan view");
        assert_eq!(
            matching("/skill re", &models, &skills)[0].name,
            "/skill review"
        );
        assert_eq!(matching("/memory add ", &models, &skills).len(), 4);
        assert!(matching("/tasks add title", &models, &skills).is_empty());
        assert!(matching("/editor arbitrary text", &models, &skills).is_empty());
    }

    #[test]
    fn directories_use_explicit_roots_and_preserve_spaces_and_quotes() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("project space")).unwrap();
        std::fs::write(root.path().join("project file"), "text").unwrap();
        std::fs::create_dir(root.path().join("project space/nested")).unwrap();
        std::fs::create_dir(root.path().join(".hidden")).unwrap();

        let candidates = directories("pro", root.path(), None);
        assert_eq!(candidates.len(), 1);
        assert!(candidates[0].description.is_empty());
        assert!(!candidates[0].name.contains("nested"));
        assert_eq!(candidates[0].name, "/cd project space/");
        assert_eq!(candidates[0].display.as_deref(), Some("project space/"));
        let quoted = directories("\"pro", root.path(), None);
        assert_eq!(
            super::super::directory::resolve(&quoted[0].name[4..], root.path(), None).unwrap(),
            root.path().join("project space")
        );
        assert_eq!(
            directories("~/pro", Path::new("/"), Some(root.path()))[0].name,
            "/cd ~/project space/"
        );
        assert!(
            directories("", root.path(), None)
                .iter()
                .all(|entry| !entry.name.contains(".hidden"))
        );
        assert!(
            directories(".", root.path(), None)
                .iter()
                .any(|entry| entry.name.contains(".hidden"))
        );
        assert!(directories("missing/", root.path(), None).is_empty());
    }
}
