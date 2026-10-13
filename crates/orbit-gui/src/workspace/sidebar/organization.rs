//! Personal organization changes navigation, never a session's workspace.
use super::*;
use gpui_kit::component::{
    input::{Input, InputState},
    notification::Notification,
};

pub(super) fn grouped_items(
    sessions: &[SessionEntry],
    placements: &[Placement],
    previous: &[TreeItem],
) -> Vec<TreeItem> {
    let mut pinned = Vec::new();
    let mut categories = std::collections::BTreeMap::<String, Vec<TreeItem>>::new();
    let mut projects = Vec::new();
    for (entry, placement) in sessions.iter().zip(placements) {
        let title = if entry.title.trim().is_empty() {
            "未命名会话".into()
        } else {
            entry.title.clone()
        };
        let item = TreeItem::new(format!("session:{}", entry.id), title);
        if placement.is_pinned() {
            pinned.push(item);
        } else if let Some(category) = placement.category() {
            categories
                .entry(category.to_owned())
                .or_default()
                .push(item);
        } else {
            projects.push(entry.clone());
        }
    }
    let group = |id: String, label: String, children: Vec<TreeItem>| {
        let expanded = previous
            .iter()
            .find(|item| item.id.as_ref() == id)
            .is_none_or(TreeItem::is_expanded);
        let mut item = TreeItem::new(id, label).expanded(expanded);
        item.children = children;
        item
    };
    let mut groups = Vec::new();
    if !pinned.is_empty() {
        groups.push(group("pinned".into(), "置顶".into(), pinned));
    }
    groups.extend(
        categories
            .into_iter()
            .map(|(category, entries)| group(format!("category:{category}"), category, entries)),
    );
    groups.extend(super::super::navigation::project_items(&projects, previous));
    groups
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
    let pin_options = options.clone();
    let pin_id = id.clone();
    let pinned = placement.is_pinned();
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
    .item(
        PopupMenuItem::new(if pinned { "取消置顶" } else { "置顶" })
            .icon(IconName::Pin)
            .on_click(move |_, _, cx| {
                Preferences::set_session_pin(&pin_options, &pin_id, !pinned, cx);
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
    use super::{Placement, SessionEntry, grouped_items};
    fn entry(id: &str, project: &str) -> SessionEntry {
        SessionEntry {
            id: id.to_owned().into(),
            title: id.to_owned().into(),
            workspace: project.to_owned().into(),
        }
    }
    fn placement(pinned: bool, category: Option<&str>) -> Placement {
        serde_json::from_value(serde_json::json!({"pinned": pinned, "category": category})).unwrap()
    }
    #[test]
    fn pinned_and_categorized_sessions_appear_once_without_changing_projects() {
        let sessions = vec![
            entry("a", "/alpha"),
            entry("b", "/beta"),
            entry("c", "/alpha"),
        ];
        let mut placements = vec![
            placement(true, Some("学习")),
            placement(false, Some("学习")),
            Placement::default(),
        ];
        let groups = grouped_items(&sessions, &placements, &[]);
        assert_eq!(
            groups
                .iter()
                .map(|group| group.id.as_ref())
                .collect::<Vec<_>>(),
            ["pinned", "category:学习", "project:/alpha"]
        );
        assert_eq!(groups.iter().flat_map(|group| &group.children).count(), 3);
        placements[0] = placement(false, Some("学习"));
        let groups = grouped_items(&sessions, &placements, &groups);
        assert_eq!(
            groups[0]
                .children
                .iter()
                .map(|child| child.id.as_ref())
                .collect::<Vec<_>>(),
            ["session:a", "session:b"]
        );
        placements[0] = Placement::default();
        let groups = grouped_items(&sessions, &placements, &groups);
        assert_eq!(groups[1].id.as_ref(), "project:/alpha");
        assert_eq!(groups[1].children.len(), 2);
        assert_eq!(sessions[0].workspace.as_ref(), "/alpha");
    }
    #[test]
    fn catalog_refresh_keeps_folder_expansion_and_reveals_new_organization_groups() {
        let sessions = vec![entry("a", "/alpha"), entry("b", "/beta")];
        let placements = vec![placement(false, Some("工作")), Placement::default()];
        let groups = grouped_items(&sessions, &placements, &[]);
        assert!(groups[0].is_expanded());
        let old = vec![
            groups[0].clone().expanded(false),
            groups[1].clone().expanded(true),
        ];
        let refreshed = grouped_items(&sessions, &placements, &old);
        assert!(!refreshed[0].is_expanded());
        assert!(refreshed[1].is_expanded());
    }
}
