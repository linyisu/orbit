//! Personal organization changes navigation, never a session's workspace.
use super::*;
use gpui_kit::component::{
    input::{Input, InputState},
    notification::Notification,
};

#[derive(Clone)]
pub(super) struct ProjectDrag {
    owner: WeakEntity<Sidebar>,
    options: Options,
    path: String,
    label: SharedString,
    position: usize,
}
impl ProjectDrag {
    pub(super) fn drop_before(&self, target: &Self) -> bool {
        self.position > target.position
    }
    pub(super) fn owner_matches(&self, other: &Self) -> bool {
        self.owner.upgrade() == other.owner.upgrade()
            && self.options.same_host(&other.options)
            && self.path != other.path
    }
}
impl Render for ProjectDrag {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        h_flex()
            .px_3()
            .py_2()
            .gap_2()
            .bg(cx.theme().popover)
            .border_1()
            .border_color(cx.theme().border)
            .rounded(cx.theme().radius)
            .child(Icon::new(IconName::Folder).size_4())
            .child(self.label.clone())
    }
}

pub(super) fn project_drag(
    owner: WeakEntity<Sidebar>,
    options: Options,
    path: String,
    label: SharedString,
    position: usize,
) -> ProjectDrag {
    ProjectDrag {
        owner,
        options,
        path,
        label,
        position,
    }
}

pub(super) fn drop_project(
    drag: &ProjectDrag,
    target: &str,
    owner: &WeakEntity<Sidebar>,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(sidebar) = owner
        .upgrade()
        .filter(|sidebar| Some(sidebar) == drag.owner.upgrade().as_ref())
    else {
        return;
    };
    let view = sidebar.read(cx);
    let Some(host) = view
        .host
        .map(|ix| &view.hosts[ix])
        .filter(|host| host.options.same_host(&drag.options))
    else {
        return;
    };
    let paths = project_paths(&host.groups);
    category_result(
        Preferences::reorder_project(&drag.options, &drag.path, target, &paths, cx),
        window,
        cx,
    );
}

fn project_paths(groups: &[TreeItem]) -> Vec<String> {
    groups
        .iter()
        .flat_map(|section| &section.children)
        .filter_map(|item| item.id.strip_prefix("project:").map(str::to_owned))
        .collect()
}

pub(super) fn row_menu(
    owner: WeakEntity<Sidebar>,
    id: &str,
    menu: PopupMenu,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let Some(sidebar) = owner.upgrade() else {
        return menu;
    };
    let view = sidebar.read(cx);
    if let Some(path) = id.strip_prefix("project:") {
        let Some(host) = view.host.map(|ix| &view.hosts[ix]) else {
            return menu;
        };
        let options = host.options.clone();
        let pinned = host.projects.is_pinned(path);
        let pin_options = options.clone();
        let pin_path = path.to_owned();
        menu.item(
            PopupMenuItem::new(if pinned {
                "取消项目置顶"
            } else {
                "置顶项目"
            })
            .icon(IconName::Pin)
            .on_click(move |_, _, cx| {
                Preferences::set_project_pin(&pin_options, &pin_path, !pinned, cx);
            }),
        )
    } else {
        session_menu(
            view.owner.clone(),
            id.strip_prefix("session:").map(str::to_owned),
            menu,
            window,
            cx,
        )
    }
}

pub(super) fn grouped_items(
    sessions: &[SessionEntry],
    placements: &[Placement],
    projects: &crate::preferences::Projects,
    previous: &[TreeItem],
) -> Vec<TreeItem> {
    let old_projects = previous
        .iter()
        .flat_map(|section| section.children.iter())
        .cloned()
        .collect::<Vec<_>>();
    let mut groups = super::super::navigation::project_items(sessions, &old_projects);
    let placements_by_id = sessions
        .iter()
        .zip(placements)
        .map(|(entry, placement)| (entry.id.as_ref(), placement))
        .collect::<std::collections::BTreeMap<_, _>>();
    // Categories live inside their actual project. Pinning moves the entire
    // project, including every conversation and its category, as one object.
    for project in &mut groups {
        let path = project.id.strip_prefix("project:").unwrap();
        let mut categories = std::collections::BTreeMap::<String, Vec<TreeItem>>::new();
        project.children.retain(|item| {
            let category = item
                .id
                .strip_prefix("session:")
                .and_then(|id| placements_by_id.get(id))
                .and_then(|placement| placement.category());
            if let Some(category) = category {
                categories
                    .entry(category.to_owned())
                    .or_default()
                    .push(item.clone());
                false
            } else {
                true
            }
        });
        let previous = old_projects.iter().find(|old| old.id == project.id);
        project
            .children
            .extend(categories.into_iter().map(|(category, children)| {
                let id = format!("category:{}:{path}:{category}", path.len());
                let expanded = previous
                    .and_then(|project| project.children.iter().find(|item| item.id.as_ref() == id))
                    .is_none_or(TreeItem::is_expanded);
                TreeItem::new(id, category)
                    .expanded(expanded)
                    .children(children)
            }));
    }
    groups.sort_by_key(|project| projects.position(project.id.strip_prefix("project:").unwrap()));
    let (pinned, regular): (Vec<_>, Vec<_>) = groups
        .into_iter()
        .partition(|project| projects.is_pinned(project.id.strip_prefix("project:").unwrap()));
    [("pinned", "置顶", pinned), ("projects", "项目", regular)]
        .into_iter()
        .filter(|(_, _, children)| !children.is_empty())
        .map(|(id, label, children)| {
            let expanded = previous
                .iter()
                .find(|section| section.id.as_ref() == id)
                .is_none_or(TreeItem::is_expanded);
            TreeItem::new(id, label)
                .expanded(expanded)
                .children(children)
        })
        .collect()
}

pub(super) fn session_menu(
    owner: Option<WeakEntity<Workspace>>,
    id: Option<String>,
    menu: PopupMenu,
    window: &mut Window,
    cx: &mut Context<PopupMenu>,
) -> PopupMenu {
    let Some(workspace) = owner.as_ref().and_then(WeakEntity::upgrade) else {
        return menu.label("没有可管理的会话");
    };
    let Some(id) = id else {
        return menu.label("请先选择一个会话");
    };
    let view = workspace.read(cx);
    let available = view.connected;
    let busy = view.session_busy(&id);
    let options = view.options.clone();
    let placement = Preferences::session_placement(&options, &id, cx);
    let categories = Preferences::session_categories(&options, cx);
    let rename_owner = owner.clone();
    let rename_id = id.clone();
    let category_id = id.clone();
    menu.item(
        PopupMenuItem::new("重命名会话…")
            .icon(IconName::Pencil)
            .disabled(!available)
            .on_click(move |_, window, cx| {
                if let Some(owner) = &rename_owner {
                    let _ =
                        owner.update(cx, |view, cx| view.rename_session(&rename_id, window, cx));
                }
            }),
    )
    .submenu_with_icon(
        Some(Icon::new(IconName::Tags)),
        "移到分类",
        window,
        cx,
        move |mut menu, _, _| {
            let clear_options = options.clone();
            let clear_id = category_id.clone();
            menu = menu.item(
                PopupMenuItem::new("按项目归类")
                    .checked(placement.category().is_none())
                    .on_click(move |_, window, cx| {
                        category_result(
                            Preferences::set_session_category(&clear_options, &clear_id, None, cx),
                            window,
                            cx,
                        );
                    }),
            );
            for category in &categories {
                let name = category.clone();
                let options = options.clone();
                let id = category_id.clone();
                menu = menu.item(
                    PopupMenuItem::new(category.clone())
                        .checked(placement.category() == Some(category.as_str()))
                        .on_click(move |_, window, cx| {
                            category_result(
                                Preferences::set_session_category(
                                    &options,
                                    &id,
                                    Some(name.clone()),
                                    cx,
                                ),
                                window,
                                cx,
                            );
                        }),
                );
            }
            let options = options.clone();
            let id = category_id.clone();
            menu.separator().item(
                PopupMenuItem::new("新建分类…")
                    .icon(IconName::Plus)
                    .on_click(move |_, window, cx| {
                        CategoryEditor::open(options.clone(), id.clone(), window, cx)
                    }),
            )
        },
    )
    .separator()
    .item(
        PopupMenuItem::new(if busy {
            "删除会话（运行中）"
        } else {
            "删除会话…"
        })
        .icon(IconName::Trash)
        .disabled(!available || busy)
        .on_click(move |_, window, cx| {
            if let Some(owner) = &owner {
                let _ = owner.update(cx, |view, cx| view.delete_session(&id, window, cx));
            }
        }),
    )
}

fn category_result(result: anyhow::Result<u64>, window: &mut Window, cx: &mut App) {
    if let Err(error) = result {
        window.push_notification(Notification::error(error.to_string()), cx);
    }
}

struct CategoryEditor {
    options: Options,
    session: String,
    name: Entity<InputState>,
    error: Option<SharedString>,
    submitted: bool,
    _events: Subscription,
}
impl CategoryEditor {
    fn open(options: Options, session: String, window: &mut Window, cx: &mut App) {
        let form = cx.new(|cx| {
            let name = cx.new(|cx| InputState::new(window, cx).placeholder("例如：工作、学习"));
            let events = cx.subscribe_in(&name, window, |form: &mut Self, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { shift: false, .. }) && form.save(cx) {
                    window.close_dialog(cx);
                } else if matches!(event, InputEvent::Change) {
                    form.error = None;
                    cx.notify();
                }
            });
            Self {
                options,
                session,
                name,
                error: None,
                submitted: false,
                _events: events,
            }
        });
        let first = std::cell::Cell::new(true);
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                form.read(cx)
                    .name
                    .clone()
                    .update(cx, |input, cx| input.focus(window, cx));
            }
            let save = form.clone();
            let confirm = form.clone();
            dialog
                .title("新建分类")
                .width(window.rem_size() * 25.)
                .child(form.clone())
                .footer(
                    h_flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("cancel-category")
                                .label("取消")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("create-category")
                                .primary()
                                .label("创建并归类")
                                .on_click(move |_, window, cx| {
                                    if confirm.update(cx, |form, cx| form.save(cx)) {
                                        window.close_dialog(cx);
                                    }
                                }),
                        ),
                )
                .on_ok(move |_, _, cx| save.update(cx, |form, cx| form.save(cx)))
        });
    }
    fn save(&mut self, cx: &mut Context<Self>) -> bool {
        // Input Enter and Dialog Confirm can both observe the same keystroke.
        // Only the first successful path may save and dismiss this dialog.
        if self.submitted {
            return false;
        }
        let value = self.name.read(cx).value().to_string();
        match Preferences::set_session_category(&self.options, &self.session, Some(value), cx) {
            Ok(_) => {
                self.submitted = true;
                true
            }
            Err(error) => {
                self.error = Some(error.to_string().into());
                cx.notify();
                false
            }
        }
    }
}
impl Render for CategoryEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .gap_3()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("用分类整理当前环境的会话。会话仍保留原来的项目目录。"),
            )
            .child(Input::new(&self.name))
            .when_some(self.error.clone(), |form, error| {
                form.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{Placement, Projects, SessionEntry, grouped_items, project_paths};
    fn entry(id: &str, project: &str) -> SessionEntry {
        SessionEntry {
            id: id.to_owned().into(),
            title: id.to_owned().into(),
            workspace: project.to_owned().into(),
        }
    }
    fn projects() -> Projects {
        serde_json::from_value(
            serde_json::json!({"pinned":["/alpha"],"order":["/gamma","/beta","/alpha"]}),
        )
        .unwrap()
    }
    #[test]
    fn pinned_projects_keep_all_sessions_and_categories_under_their_project() {
        let sessions = vec![
            entry("a", "/alpha"),
            entry("b", "/beta"),
            entry("c", "/alpha"),
            entry("d", "/gamma"),
        ];
        let placements = vec![
            serde_json::from_value(serde_json::json!({"category":"学习"})).unwrap(),
            Placement::default(),
            Placement::default(),
            Placement::default(),
        ];
        let groups = grouped_items(&sessions, &placements, &projects(), &[]);
        assert_eq!(
            groups
                .iter()
                .map(|section| section.id.as_ref())
                .collect::<Vec<_>>(),
            ["pinned", "projects"]
        );
        let alpha = &groups[0].children[0];
        assert_eq!(alpha.id.as_ref(), "project:/alpha");
        assert_eq!(alpha.children[0].id.as_ref(), "session:c");
        assert_eq!(alpha.children[1].label.as_ref(), "学习");
        assert_eq!(alpha.children[1].children[0].id.as_ref(), "session:a");
        assert_eq!(project_paths(&groups), ["/alpha", "/gamma", "/beta"]);
        let category = alpha.children[1].clone().expanded(false);
        let alpha = alpha.clone().expanded(true).children([]);
        let mut old = groups.clone();
        old[0].children[0] = alpha;
        old[0].children[0].children = vec![category];
        let unpinned = Projects::default();
        let moved = grouped_items(&sessions, &placements, &unpinned, &old);
        assert_eq!(moved.len(), 1);
        let alpha = moved[0]
            .children
            .iter()
            .find(|item| item.id.as_ref() == "project:/alpha")
            .unwrap();
        assert!(alpha.is_expanded());
        assert!(!alpha.children[1].is_expanded());
        assert_eq!(sessions[0].workspace.as_ref(), "/alpha");
    }
    #[test]
    fn new_catalog_entries_keep_saved_project_order_and_existing_expansion() {
        let sessions = vec![entry("b", "/beta"), entry("d", "/gamma")];
        let placements = vec![Placement::default(); 2];
        let groups = grouped_items(&sessions, &placements, &projects(), &[]);
        groups[0].children[0].clone().expanded(true);
        let refreshed = grouped_items(
            &sessions.iter().rev().cloned().collect::<Vec<_>>(),
            &placements,
            &projects(),
            &groups,
        );
        assert_eq!(project_paths(&refreshed), ["/gamma", "/beta"]);
        assert!(refreshed[0].children[0].is_expanded());
    }
}
