use super::commands;

pub use orbit_protocol::Completion as Candidate;

pub(super) use orbit_client::completion::{DirectoryCache, cached_directories};

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
}
