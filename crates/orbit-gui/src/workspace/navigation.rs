//! Project directories and their conversations, with Kit owning tree navigation.
use super::*;

impl Workspace {
    pub(super) fn select_session(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if self.opening || self.choosing_project {
            return;
        }
        let Some(entry) = self.sessions.iter().find(|entry| entry.id.as_ref() == id) else {
            return;
        };
        let path = PathBuf::from(entry.workspace.as_ref());
        if self.connected && self.location == entry.workspace {
            self.open(Some(id.to_owned()), cx);
        } else if self.options.ssh.is_some() || self.options.wsl.is_some() {
            // Remote paths belong to their host, never to the desktop filesystem.
            self.switch_project(path, Some(id.to_owned()), false, false, window, cx);
        } else {
            let id = id.to_owned();
            self.choosing_project = true;
            let check_path = path.clone();
            let check = cx.background_spawn(async move { std::fs::metadata(check_path) });
            self.project_picker = cx.spawn(async move |owner, cx| {
                let result = check.await;
                let _ = owner.update_in(cx, |view, window, cx| {
                    view.choosing_project = false;
                    match result {
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                            view.choose_project_directory(Some(id), cx);
                        }
                        Ok(metadata) if metadata.is_dir() => {
                            view.switch_project(path, Some(id), true, false, window, cx);
                        }
                        Ok(_) => view.report_error("项目路径已不再是目录", cx),
                        Err(error) => view.report_error(format!("无法访问项目目录：{error}"), cx),
                    }
                    cx.notify();
                });
            });
            cx.notify();
        }
    }
}

/// Measure the actual shaped text so CJK, proportional fonts and zoom all use
/// the same clipping boundary. Only the hovered, overflowing row animates.
pub(super) fn scrolling_title(title: SharedString, started: Instant) -> impl IntoElement {
    canvas(
        move |_, window, _| {
            let style = window.text_style();
            let font_size = style.font_size.to_pixels(window.rem_size());
            window.text_system().shape_line(
                title.clone(),
                font_size,
                &[style.to_run(title.len())],
                None,
            )
        },
        move |bounds, line, window, cx| {
            let overflow = (line.width() - bounds.size.width).max(px(0.));
            let offset = marquee_offset(
                f32::from(overflow),
                started.elapsed().as_secs_f32(),
                f32::from(window.rem_size()),
            );
            window.with_content_mask(Some(ContentMask { bounds }), |window| {
                let _ = line.paint(
                    bounds.origin - point(px(offset), px(0.)),
                    bounds.size.height,
                    TextAlign::Left,
                    None,
                    window,
                    cx,
                );
            });
            if overflow > px(0.) {
                window.request_animation_frame();
            }
        },
    )
    .w_full()
    .h_full()
}

fn marquee_offset(overflow: f32, elapsed: f32, rem: f32) -> f32 {
    if overflow <= 0. {
        return 0.;
    }
    let travel = overflow / (rem * 2.5);
    let phase = elapsed % (travel * 2. + 1.6);
    if phase < 0.8 {
        0.
    } else if phase < 0.8 + travel {
        (phase - 0.8) / travel * overflow
    } else if phase < 1.6 + travel {
        overflow
    } else {
        overflow * (1. - (phase - 1.6 - travel) / travel)
    }
}

pub(super) fn project_items(sessions: &[SessionEntry], previous: &[TreeItem]) -> Vec<TreeItem> {
    let mut groups: Vec<TreeItem> = Vec::new();
    for session in sessions {
        let id: SharedString = format!("project:{}", session.workspace).into();
        let title = if session.title.trim().is_empty() {
            "未命名会话".into()
        } else {
            session.title.clone()
        };
        let child = TreeItem::new(format!("session:{}", session.id), title);
        if let Some(group) = groups.iter_mut().find(|group| group.id == id) {
            group.children.push(child);
        } else {
            let label = PathBuf::from(session.workspace.as_ref())
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| session.workspace.to_string());
            let expanded = previous
                .iter()
                .find(|group| group.id == id)
                .is_some_and(TreeItem::is_expanded);
            groups.push(TreeItem::new(id, label).expanded(expanded).child(child));
        }
    }
    // The server returns sessions by recent activity. Project navigation must
    // not move when opening a session updates that activity, including when a
    // new Workspace entity is created with no previous tree to consult.
    groups.sort_by(|left, right| {
        left.label
            .cmp(&right.label)
            .then_with(|| left.id.cmp(&right.id))
    });
    groups
}

#[cfg(test)]
mod tests {
    use super::{SessionEntry, marquee_offset, project_items};

    #[test]
    fn hover_scroll_pauses_at_each_end_and_stays_inside_the_overflow() {
        assert_eq!(marquee_offset(0., 20., 16.), 0.);
        assert_eq!(marquee_offset(80., 0.5, 16.), 0.);
        assert!((marquee_offset(80., 1.8, 16.) - 40.).abs() < 0.001);
        assert_eq!(marquee_offset(80., 3.2, 16.), 80.);
        assert!((marquee_offset(80., 4.6, 16.) - 40.).abs() < 0.001);
        for tick in 0..2000 {
            assert!((0. ..=80.).contains(&marquee_offset(80., tick as f32 / 60., 16.)));
        }
        // Doubling UI scale preserves the visual speed in rems.
        assert!((marquee_offset(160., 1.8, 32.) - 80.).abs() < 0.001);
    }

    #[test]
    fn directories_with_identical_names_stay_distinct_and_keep_expansion_on_refresh() {
        let entries =
            [("a", "/work/app"), ("b", "/other/app"), ("c", "/work/app")].map(|(id, path)| {
                SessionEntry {
                    id: id.into(),
                    title: id.into(),
                    workspace: path.into(),
                }
            });
        let groups = project_items(&entries, &[]);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[1].children.len(), 2);
        assert_eq!(groups[0].children[0].id, "session:b");
        let previous = vec![groups[1].clone().expanded(true)];
        let refreshed = project_items(&entries, &previous);
        assert!(!refreshed[0].is_expanded());
        assert!(refreshed[1].is_expanded());
    }

    #[test]
    fn opening_a_session_does_not_reorder_projects() {
        let mut entries =
            [("b", "/work/beta"), ("a", "/work/alpha")].map(|(id, path)| SessionEntry {
                id: id.into(),
                title: id.into(),
                workspace: path.into(),
            });
        let initial = project_items(&entries, &[]);
        entries.reverse();
        for previous in [&[][..], initial.as_slice()] {
            let refreshed = project_items(&entries, previous);
            assert_eq!(
                refreshed.iter().map(|group| &group.id).collect::<Vec<_>>(),
                initial.iter().map(|group| &group.id).collect::<Vec<_>>()
            );
        }
    }
}
