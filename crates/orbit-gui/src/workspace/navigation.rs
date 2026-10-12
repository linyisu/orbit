//! Project directories and their conversations, with Kit owning tree navigation.
use super::*;
use gpui_kit::component::{list::ListItem, tooltip::Tooltip};

impl Workspace {
    pub(super) fn regroup(&mut self, cx: &mut Context<Self>) {
        let mut groups = project_items(&self.sessions, &self.groups);
        for group in &mut groups {
            if group.id.as_ref() == format!("project:{}", self.location)
                && !self.groups.iter().any(|previous| previous.id == group.id)
            {
                *group = group.clone().expanded(true);
            }
        }
        self.groups = groups;
        let selected = self
            .info
            .as_ref()
            .map(|info| TreeItem::new(format!("session:{}", info.id), info.title.clone()));
        self.navigation.update(cx, |tree, cx| {
            tree.set_items(self.groups.clone(), cx);
            let ix = selected.as_ref().and_then(|item| tree.index_of(&item.id));
            tree.set_selected_index(ix, cx);
        });
    }

    pub(super) fn project_tree(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.weak_entity();
        let active = self
            .info
            .as_ref()
            .map(|info| format!("session:{}", info.id));
        div()
            .id("project-conversations")
            .size_full()
            .capture_key_down(cx.listener(|view, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "enter" {
                    let id = view
                        .navigation
                        .read(cx)
                        .selected_item()
                        .map(|item| item.id.clone());
                    if let Some(id) =
                        id.and_then(|id| id.strip_prefix("session:").map(str::to_owned))
                    {
                        view.select_session(&id, window, cx);
                        cx.stop_propagation();
                    }
                }
            }))
            .child(Tree::new(&self.navigation, move |_, entry, _, _, _| {
                let id = entry.item().id.clone();
                let owner = owner.clone();
                let folder = entry.is_folder();
                let expanded = entry.is_expanded();
                ListItem::new(id.clone())
                    .accessibility_label(entry.item().label.clone())
                    .confirmed(active.as_deref() == Some(id.as_ref()))
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .gap_2()
                            .child(div().w_3().flex_shrink_0().when(folder, |slot| {
                                slot.child(
                                    Icon::new(if expanded {
                                        IconName::ChevronDown
                                    } else {
                                        IconName::ChevronRight
                                    })
                                    .size_3(),
                                )
                            }))
                            .child(div().w_4().flex_shrink_0().when(folder, |slot| {
                                slot.child(Icon::new(IconName::Folder).size_4())
                            }))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_ellipsis()
                                    .child(entry.item().label.clone()),
                            ),
                    )
                    .when(folder, |row| {
                        let path = id.strip_prefix("project:").unwrap_or(&id).to_owned();
                        row.tooltip(move |window, cx| Tooltip::new(path.clone()).build(window, cx))
                    })
                    .on_click(move |_, window, cx| {
                        if let Some(session) = id.strip_prefix("session:") {
                            let _ = owner
                                .update(cx, |view, cx| view.select_session(session, window, cx));
                        }
                    })
            }))
    }

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

fn project_items(sessions: &[SessionEntry], previous: &[TreeItem]) -> Vec<TreeItem> {
    let mut groups: Vec<TreeItem> = Vec::new();
    for session in sessions {
        let id: SharedString = format!("project:{}", session.workspace).into();
        let child = TreeItem::new(format!("session:{}", session.id), session.title.clone());
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
    groups
}

#[cfg(test)]
mod tests {
    use super::{SessionEntry, project_items};

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
        assert_eq!(groups[0].children.len(), 2);
        assert_eq!(groups[1].children[0].id, "session:b");
        let previous = vec![groups[1].clone().expanded(true)];
        let refreshed = project_items(&entries, &previous);
        assert!(!refreshed[0].is_expanded());
        assert!(refreshed[1].is_expanded());
    }
}
