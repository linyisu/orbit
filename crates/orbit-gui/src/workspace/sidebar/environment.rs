//! Environment navigation reuses workspace connection and directory actions.
use super::*;
use gpui_kit::component::tab::{Tab, TabBar};

impl HostNavigation {
    pub(super) fn is_remote(&self) -> bool {
        self.options.ssh.is_some() || self.options.wsl.is_some()
    }

    fn label(&self) -> String {
        self.options
            .connection_options()
            .and_then(|options| options.target())
            .map(|target| target.label())
            .unwrap_or_else(|_| "连接配置无效".into())
    }
}

impl Sidebar {
    /// Select an existing target without carrying a session ID across services.
    /// Unsent drafts are retargeted by the workspace's existing connection flow.
    fn select_host(&mut self, mut options: Options, window: &mut Window, cx: &mut Context<Self>) {
        if self.host.is_some_and(|ix| {
            self.hosts[ix]
                .options
                .same_host(&options)
        }) {
            return;
        }
        options.resume = None;
        options.recover_draft = false;
        options.relink_on_resume = false;
        self.with_owner(
            cx,
            |view, window, cx| view.open_target(options, window, cx),
            window,
        );
    }

    /// Visit the most recently used environment of this kind, or ask for its target.
    fn select_environment(&mut self, remote: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .host
            .is_some_and(|ix| self.hosts[ix].is_remote() == remote)
        {
            return;
        }
        let options = if remote {
            self.last_remote
        } else {
            self.last_local
        }
        .map(|ix| self.hosts[ix].options.clone());
        if let Some(options) = options {
            self.select_host(options, window, cx);
        } else {
            self.with_owner(
                cx,
                |view, window, cx| {
                    if remote {
                        view.remote_connection_dialog(window, cx);
                    } else {
                        view.choose_project_directory(None, cx);
                    }
                },
                window,
            );
        }
    }

    pub(super) fn environment_switcher(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let current = &self.hosts[self.host.expect("bound sidebar")];
        let remote = current.is_remote();
        let remote_count = self
            .hosts
            .iter()
            .filter(|host| host.is_remote())
            .count();
        let label = if remote {
            current.label()
        } else {
            "此计算机".into()
        };
        let hosts: Vec<_> = self
            .hosts
            .iter()
            .filter(|host| host.is_remote() == remote)
            .map(|host| {
                (
                    host.label(),
                    host.options.clone(),
                    current.options.same_host(&host.options),
                )
            })
            .collect();
        let owner = cx.weak_entity();
        v_flex()
            .gap_1()
            .mb_2()
            .child(
                TabBar::new("sidebar-environments")
                    .segmented()
                    .w_full()
                    .py_1()
                    .bg(cx.theme().muted)
                    .selected_index(usize::from(remote))
                    .child(
                        Tab::new()
                            .aria_label("本地")
                            .flex_1()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(Icon::new(IconName::Laptop).size_4())
                                    .child("本地"),
                            ),
                    )
                    .child(
                        Tab::new()
                            .flex_1()
                            .aria_label(format!("远程，{remote_count} 个已访问环境"))
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(Icon::new(IconName::Cloud).size_4())
                                    .child("远程")
                                    .when(remote_count > 0, |row| {
                                        row.child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(remote_count.to_string()),
                                        )
                                    }),
                            ),
                    )
                    .on_click(cx.listener(|view, ix: &usize, window, cx| {
                        view.select_environment(*ix == 1, window, cx);
                    })),
            )
            .child(
                Button::new("environment-switcher")
                    .ghost()
                    .small()
                    .w_full()
                    .accessibility_label("当前环境与项目操作")
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .child(
                                Icon::new(if remote {
                                    IconName::Monitor
                                } else {
                                    IconName::Laptop
                                })
                                .size_4(),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_ellipsis()
                                    .child(label.clone()),
                            )
                            .child(Icon::new(IconName::ChevronDown).size_3()),
                    )
                    .tooltip(label)
                    .dropdown_menu(move |mut menu, _, _| {
                        for (label, options, selected) in &hosts {
                            let owner = owner.clone();
                            let options = options.clone();
                            menu = menu.item(
                                PopupMenuItem::new(label.clone())
                                    .checked(*selected)
                                    .on_click(move |_, window, cx| {
                                        let _ = owner.update(cx, |view, cx| {
                                            view.select_host(options.clone(), window, cx)
                                        });
                                    }),
                            );
                        }
                        let project_owner = owner.clone();
                        menu = menu.separator().item(
                            PopupMenuItem::new("打开项目…")
                                .icon(IconName::Folder)
                                .on_click(move |_, window, cx| {
                                    let _ = project_owner.update(cx, |view, cx| {
                                        view.with_owner(
                                            cx,
                                            |view, window, cx| view.open_project(window, cx),
                                            window,
                                        );
                                    });
                                }),
                        );
                        if remote {
                            let owner = owner.clone();
                            menu = menu.item(
                                PopupMenuItem::new("添加远程连接…")
                                    .icon(IconName::Plus)
                                    .on_click(move |_, window, cx| {
                                        let _ = owner.update(cx, |view, cx| {
                                            view.with_owner(
                                                cx,
                                                |view, window, cx| {
                                                    view.remote_connection_dialog(window, cx)
                                                },
                                                window,
                                            );
                                        });
                                    }),
                            );
                        }
                        menu
                    }),
            )
    }
}
