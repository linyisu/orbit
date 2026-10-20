//! Global plugin catalogue and source management share one retained request owner.
use super::super::*;
use anyhow::Context as _;
use gpui_kit::component::{
    InteractiveElementExt, StyledExt, VirtualListScrollHandle,
    input::{Input, InputState},
    scroll::{ScrollableElement, Scrollbar},
    spinner::Spinner,
    tooltip::Tooltip,
    v_virtual_list,
};
use orbit_protocol::{plugins as protocol, plugins::Selection};
use std::rc::Rc;

pub(super) struct Plugins {
    owner: WeakEntity<Workspace>,
    library: orbit_client::library::Library,
    runtime: Arc<tokio::runtime::Runtime>,
    session: Option<String>,
    workspace: String,
    catalog: Option<protocol::Catalog>,
    query: Entity<InputState>,
    busy: bool,
    error: Option<String>,
    task: Task<()>,
    application_task: Task<()>,
    application_error: Option<String>,
    scroll: VirtualListScrollHandle,
    applied_expanded: bool,
    applied_scroll: ScrollHandle,
    _query: Subscription,
}

#[derive(Clone)]
enum CatalogRow {
    Heading(String, usize),
    Cards(Vec<(usize, usize)>, usize),
}

impl Plugins {
    pub(super) fn has_pending_work(&self) -> bool {
        self.busy
    }

    pub(super) fn new(
        owner: WeakEntity<Workspace>,
        session: Option<String>,
        workspace: String,
        library: super::GlobalLibrary,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let runtime = library.runtime;
        let library = library.service;
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("搜索插件"));
        let subscription = cx.subscribe(&query, |view, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                view.scroll
                    .set_offset(point(Pixels::ZERO, Pixels::ZERO));
                cx.notify();
            }
        });
        let entity = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = entity.update(cx, |view, cx| {
                view.request(protocol::Action::List, None, cx)
            });
        });
        Self {
            owner,
            library,
            runtime,
            session,
            workspace,
            catalog: None,
            query,
            busy: false,
            error: None,
            task: Task::ready(()),
            application_task: Task::ready(()),
            application_error: None,
            scroll: VirtualListScrollHandle::new(),
            applied_expanded: false,
            applied_scroll: ScrollHandle::new(),
            _query: subscription,
        }
    }

    pub(super) fn bind_context(
        &mut self,
        session: Option<String>,
        workspace: String,
        cx: &mut Context<Self>,
    ) {
        self.session = session;
        self.workspace = workspace;
        if let Some(catalog) = &mut self.catalog {
            catalog.session_id.clear();
            catalog.applied.clear();
        }
        let owner = cx.weak_entity();
        cx.defer(move |cx| {
            let _ = owner.update(cx, |view, cx| view.refresh_applied(cx));
        });
        cx.notify();
    }

    fn request(
        &mut self,
        action: protocol::Action,
        completion: Option<WeakEntity<Sources>>,
        cx: &mut Context<Self>,
    ) {
        if self.busy {
            return;
        }
        let (result, receiver) = tokio::sync::oneshot::channel();
        let session = self.session.clone();
        let workspace = self.workspace.clone();
        let applying = matches!(
            &action,
            protocol::Action::Enable { .. } | protocol::Action::Disable { .. }
        );
        if !applying {
            let library = self.library.clone();
            self.runtime.spawn(async move {
                let response = library
                    .plugins(action)
                    .await
                    .map_err(|error| format!("{error:#}"));
                let _ = result.send(response);
            });
        } else {
            let sent = self.owner.update(cx, |view, _| {
                anyhow::ensure!(view.connected, "请先连接会话，再应用插件。");
                let session = session.context("请先打开会话，再应用插件")?;
                anyhow::ensure!(
                    view.info
                        .as_ref()
                        .is_some_and(|info| info.id == session && info.workspace == workspace),
                    "目标会话已改变，请重试"
                );
                view.backend
                    .as_ref()
                    .context("请先连接会话")?
                    .send(Command::Settings {
                        request: Request::Plugins {
                            session,
                            expected_workspace: Some(workspace),
                            action,
                        },
                        result,
                    })
            });
            if let Err(error) = sent
                .map_err(|e| e.to_string())
                .and_then(|value| value.map_err(|e| e.to_string()))
            {
                self.error = Some(error);
                cx.notify();
                return;
            }
        }
        self.busy = true;
        self.error = None;
        self.task = cx.spawn(async move |owner, cx| {
            let result = receiver
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r);
            let _ = owner.update(cx, |view, cx| {
                view.busy = false;
                match result {
                    Ok(Response::Plugins(catalog)) => {
                        view.catalog = Some(catalog);
                        view.refresh_applied(cx);
                        if let Some(dialog) = completion.and_then(|dialog| dialog.upgrade()) {
                            dialog.update(cx, |source, cx| {
                                source.added = true;
                                cx.notify();
                            });
                        }
                    }
                    Ok(_) => {
                        view.error = Some("服务没有返回插件数据，请更新工作区服务后重试。".into())
                    }
                    Err(error) => view.error = Some(error),
                }
                cx.notify();
            });
        });
        cx.notify();
    }

    fn refresh_applied(&mut self, cx: &mut Context<Self>) {
        self.application_error = None;
        let Some(session) = self.session.clone() else {
            return;
        };
        let workspace = self.workspace.clone();
        let (result, receiver) = tokio::sync::oneshot::channel();
        let sent = self.owner.update(cx, |view, _| {
            anyhow::ensure!(view.connected, "会话尚未连接");
            view.backend
                .as_ref()
                .context("会话尚未连接")?
                .send(Command::Settings {
                    request: Request::Resources {
                        session: session.clone(),
                        expected_workspace: Some(workspace.clone()),
                        action: orbit_protocol::resources::Action::AppliedExtensions,
                    },
                    result,
                })
        });
        if !matches!(sent, Ok(Ok(()))) {
            return;
        }
        self.application_task = cx.spawn(async move |owner, cx| {
            let response = receiver.await;
            let _ = owner.update(cx, |view, cx| {
                if view.session.as_ref() != Some(&session) || view.workspace != workspace {
                    return;
                }
                match response {
                    Ok(Ok(Response::Resources(
                        orbit_protocol::resources::Snapshot::Extensions(extensions),
                    ))) => {
                        if let Some(catalog) = &mut view.catalog {
                            catalog.session_id = session;
                            catalog.applied = extensions
                                .into_iter()
                                .filter_map(|extension| {
                                    if let orbit_protocol::library::ExtensionKind::Plugin(plugin) =
                                        extension.kind
                                    {
                                        Some(plugin)
                                    } else {
                                        None
                                    }
                                })
                                .collect();
                        }
                    }
                    Ok(Err(error)) => view.application_error = Some(error),
                    _ => view.application_error = Some("会话应用状态未能刷新，请重试。".into()),
                }
                cx.notify();
            });
        });
    }

    /// Let the dialog own scrolling for the entire source-management form.
    fn sources(&mut self, adding: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.error = None;
        let owner = cx.entity();
        let sources = cx.new(|cx| Sources::new(owner, adding, window, cx));
        let focus = sources
            .read(cx)
            .source
            .read(cx)
            .focus_handle(cx);
        let first = std::cell::Cell::new(true);
        crate::dialog::open(window, cx, move |dialog, window, cx| {
            if first.replace(false) && adding {
                focus.focus(window, cx);
            }
            let confirm = sources.clone();
            dialog
                .on_ok(move |_, _, cx| {
                    if adding {
                        if confirm.read(cx).added {
                            return true;
                        }
                        confirm.update(cx, |view, cx| view.add(cx));
                    }
                    false
                })
                .title(if adding {
                    "添加插件市场"
                } else {
                    "市场来源"
                })
                .child(sources.clone())
        });
        cx.notify();
    }

    fn details(
        &self,
        market_ix: usize,
        plugin_ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(catalog) = &self.catalog else {
            return;
        };
        let market = &catalog.marketplaces[market_ix];
        let plugin = &market.plugins[plugin_ix];
        let title = plugin.name.clone();
        let description = plugin.description.clone();
        let source = market.name.clone();
        let inspection = catalog
            .inspections
            .iter()
            .find(|p| p.source == market.source && p.name == plugin.name)
            .cloned();
        crate::dialog::open(window, cx, move |dialog, _, cx| {
            let mut body = v_flex()
                .gap_3()
                .child(description.clone())
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("来源：{source}")),
                );
            if let Some(inspection) = &inspection {
                match &inspection.package {
                    Ok(package) => {
                        if let Some(version) = &package.version {
                            body = body.child(
                                div()
                                    .text_sm()
                                    .child(format!("版本：{version}")),
                            );
                        }
                        for (label, values) in [
                            ("技能", &package.skills),
                            ("命令", &package.commands),
                            ("Agent", &package.agents),
                        ] {
                            if !values.is_empty() {
                                body = body.child(
                                    div()
                                        .text_sm()
                                        .child(format!("{label}：{}", values.join("、"))),
                                );
                            }
                        }
                        if package.declares_hooks {
                            body = body.child(
                                div()
                                    .text_sm()
                                    .child("包含生命周期钩子"),
                            );
                        }
                        if package.declares_mcp {
                            body = body.child(
                                div()
                                    .text_sm()
                                    .child("包含 MCP 工具配置"),
                            );
                        }
                    }
                    Err(error) => {
                        body = body.child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().danger)
                                .child(
                                    orbit_client::directories::display_diagnostic(error)
                                        .into_owned(),
                                ),
                        )
                    }
                }
            }
            dialog.title(title.clone()).child(body)
        });
    }

    fn card(&self, market_ix: usize, plugin_ix: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(catalog) = &self.catalog else {
            return div().into_any_element();
        };
        let market = &catalog.marketplaces[market_ix];
        let plugin = &market.plugins[plugin_ix];
        let enabled = catalog
            .applied
            .iter()
            .any(|p| p.plugin_name == plugin.name && p.marketplace_source == market.source);
        let inspection = catalog
            .inspections
            .iter()
            .find(|p| p.name == plugin.name && p.source == market.source);
        let package = inspection.and_then(|p| p.package.as_ref().ok());
        let unavailable = package.is_none() || plugin.source.is_none();
        let pinned = Selection {
            marketplace_source: market.source.clone(),
            plugin_name: plugin.name.clone(),
            package_source: plugin
                .source
                .clone()
                .unwrap_or_default(),
            version: package.and_then(|p| p.version.clone()),
            marketplace_revision: market.revision.clone(),
        };
        let name = plugin.name.clone();
        let source = market.source.clone();
        let id = format!("{}:{}", source, name);
        h_flex()
            .w_full()
            .min_w_0()
            .gap_3()
            .py_3()
            .child(
                div()
                    .size_10()
                    .flex_shrink_0()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().muted)
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(Icon::new(IconName::Puzzle).size_5()),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        div()
                            .font_semibold()
                            .truncate()
                            .child(name.clone()),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .truncate()
                            .child(plugin.description.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .truncate()
                            .child(if unavailable {
                                "包不可用，查看详情以了解原因".into()
                            } else {
                                package_summary(package.unwrap())
                            }),
                    ),
            )
            .child(
                Button::new(SharedString::from(format!("plugin-toggle:{id}")))
                    .small()
                    .label(if enabled {
                        "从会话移除"
                    } else {
                        "应用到当前会话"
                    })
                    .disabled(
                        self.busy
                            || (!enabled && unavailable)
                            || self.session.is_none()
                            || !self
                                .owner
                                .upgrade()
                                .is_some_and(|owner| owner.read(cx).connected),
                    )
                    .on_click(cx.listener(move |view, _, _, cx| {
                        view.request(
                            if enabled {
                                protocol::Action::Disable {
                                    marketplace_source: source.clone(),
                                    plugin_name: name.clone(),
                                }
                            } else {
                                protocol::Action::Enable {
                                    plugin: pinned.clone(),
                                }
                            },
                            None,
                            cx,
                        )
                    })),
            )
            .child(
                Button::new(SharedString::from(format!("plugin-details:{id}")))
                    .ghost()
                    .small()
                    .icon(IconName::Info)
                    .accessibility_label("插件详情")
                    .tooltip("查看插件详情…")
                    .on_click(cx.listener(move |view, _, window, cx| {
                        view.details(market_ix, plugin_ix, window, cx)
                    })),
            )
            .into_any_element()
    }

    fn row(&self, row: &CatalogRow, cx: &mut Context<Self>) -> AnyElement {
        match row {
            CatalogRow::Heading(name, count) => h_flex()
                .w_full()
                .h_full()
                .items_end()
                .pb_2()
                .gap_2()
                .border_b_1()
                .border_color(cx.theme().border)
                .child(
                    div()
                        .font_semibold()
                        .child(name.clone()),
                )
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(count.to_string()),
                )
                .into_any_element(),
            CatalogRow::Cards(cards, columns) => div()
                .w_full()
                .grid()
                .grid_cols(*columns as u16)
                .gap_6()
                .h_full()
                .children(
                    cards
                        .iter()
                        .map(|&(market, plugin)| self.card(market, plugin, cx)),
                )
                .into_any_element(),
        }
    }
}

fn package_summary(package: &protocol::PluginPackage) -> String {
    let mut fields = Vec::new();
    if !package.skills.is_empty() {
        fields.push(format!("{} 个技能", package.skills.len()));
    }
    if !package.commands.is_empty() {
        fields.push(format!("{} 个命令", package.commands.len()));
    }
    if !package.agents.is_empty() {
        fields.push(format!("{} 个 Agent", package.agents.len()));
    }
    if package.declares_mcp {
        fields.push("MCP 工具".into());
    }
    if package.declares_hooks {
        fields.push("钩子".into());
    }
    if fields.is_empty() {
        package
            .version
            .clone()
            .unwrap_or_else(|| "插件包".into())
    } else {
        fields.join(" · ")
    }
}

impl Plugins {
    fn page(&self, width: Pixels, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let mut content = super::content_column()
            .flex_shrink_0()
            .min_w_0()
            .p_6()
            .gap_6()
            .child(
                h_flex()
                    .items_start()
                    .gap_4()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_2()
                            .child(
                                div()
                                    .text_xl()
                                    .font_semibold()
                                    .child("插件市场"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("在本机统一管理插件来源，将所需插件应用到当前会话。"),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("plugin-reload")
                                    .ghost()
                                    .small()
                                    .icon(IconName::RefreshCw)
                                    .tooltip("重新加载插件列表")
                                    .accessibility_label("重新加载插件列表")
                                    .disabled(self.busy)
                                    .on_click(cx.listener(|v, _, _, cx| {
                                        v.request(protocol::Action::List, None, cx)
                                    })),
                            )
                            .child(
                                Button::new("plugin-sources")
                                    .ghost()
                                    .small()
                                    .icon(IconName::Settings)
                                    .tooltip("市场来源…")
                                    .accessibility_label("市场来源")
                                    .disabled(self.busy)
                                    .on_click(
                                        cx.listener(|v, _, window, cx| {
                                            v.sources(false, window, cx)
                                        }),
                                    ),
                            )
                            .child(
                                Button::new("plugin-add-market")
                                    .small()
                                    .icon(IconName::Plus)
                                    .label("添加…")
                                    .disabled(self.busy)
                                    .on_click(
                                        cx.listener(|v, _, window, cx| v.sources(true, window, cx)),
                                    ),
                            ),
                    ),
            )
            .child(
                Input::new(&self.query)
                    .prefix(Icon::new(IconName::Search))
                    .cleanable(true)
                    .aria_label("搜索插件"),
            )
            .when(self.busy, |v| {
                v.child(
                    h_flex()
                        .gap_2()
                        .child(Spinner::new().small())
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("正在更新插件…"),
                        ),
                )
            })
            .when_some(
                self.error
                    .clone()
                    .or_else(|| self.application_error.clone()),
                |v, error| {
                    v.child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().danger)
                            .child(
                                orbit_client::directories::display_diagnostic(&error).into_owned(),
                            ),
                    )
                },
            );
        let Some(catalog) = &self.catalog else {
            // Container-query roots are painted at the viewport origin; an
            // actual parent must distribute the content column's auto margins.
            return v_flex()
                .size_full()
                .child(
                    content.child(
                        div()
                            .py_8()
                            .text_color(cx.theme().muted_foreground)
                            .child(if self.busy {
                                "正在加载插件…"
                            } else {
                                "尚未加载全局插件库，请刷新重试。"
                            }),
                    ),
                )
                .into_any_element();
        };
        if !catalog.applied.is_empty() {
            content = content.child(
                v_flex()
                    .gap_3()
                    .child(
                        h_flex().gap_2().child(
                            Button::new("toggle-enabled-plugins")
                                .ghost()
                                .small()
                                .icon(if self.applied_expanded {
                                    IconName::ChevronDown
                                } else {
                                    IconName::ChevronRight
                                })
                                .label(format!("当前会话已应用 · {}", catalog.applied.len()))
                                .on_click(cx.listener(|view, _, _, cx| {
                                    view.applied_expanded = !view.applied_expanded;
                                    cx.notify();
                                })),
                        ),
                    )
                    .when(self.applied_expanded, |view| {
                        view.child(
                            div()
                                .relative()
                                .w_full()
                                .child(
                                    div()
                                        .id("enabled-plugins")
                                        .max_h(rems(6.))
                                        .overflow_y_scroll()
                                        .lock_scroll_axis()
                                        .track_scroll(&self.applied_scroll)
                                        .child(h_flex().flex_wrap().gap_2().children(
                                            catalog.applied.iter().map(|plugin| {
                                                let name = plugin.plugin_name.clone();
                                                let source = plugin.marketplace_source.clone();
                                                Button::new(SharedString::from(format!(
                                                    "enabled:{source}:{name}"
                                                )))
                                                .outline()
                                                .small()
                                                .icon(IconName::X)
                                                .label(name.clone())
                                                .accessibility_label(format!("停用插件 {name}"))
                                                .tooltip(format!("{source} · 点击停用 {name}"))
                                                .disabled(self.busy)
                                                .on_click(cx.listener(move |v, _, _, cx| {
                                                    v.request(
                                                        protocol::Action::Disable {
                                                            marketplace_source: source.clone(),
                                                            plugin_name: name.clone(),
                                                        },
                                                        None,
                                                        cx,
                                                    )
                                                }))
                                            }),
                                        )),
                                )
                                .vertical_scrollbar(&self.applied_scroll),
                        )
                    }),
            );
        }
        let query = self
            .query
            .read(cx)
            .value()
            .trim()
            .to_lowercase();
        let columns = if width / window.rem_size() >= 60. {
            2
        } else {
            1
        };
        let mut rows = Vec::new();
        for (market_ix, market) in catalog.marketplaces.iter().enumerate() {
            let matches: Vec<_> = market
                .plugins
                .iter()
                .enumerate()
                .filter(|(_, plugin)| {
                    query.is_empty()
                        || format!("{} {} {}", plugin.name, plugin.description, market.name)
                            .to_lowercase()
                            .contains(&query)
                })
                .map(|(plugin_ix, _)| (market_ix, plugin_ix))
                .collect();
            if !matches.is_empty() {
                rows.push(CatalogRow::Heading(market.name.clone(), matches.len()));
                rows.extend(
                    matches
                        .chunks(columns)
                        .map(|chunk| CatalogRow::Cards(chunk.to_vec(), columns)),
                );
            }
        }
        if rows.is_empty() {
            return v_flex()
                .size_full()
                .child(
                    content.child(
                        div()
                            .py_8()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(if catalog.marketplaces.is_empty() {
                                "还没有插件市场。添加 Git 地址或本地目录来浏览插件。"
                            } else if query.is_empty() {
                                "市场中没有可用插件。可在市场来源中刷新。"
                            } else {
                                "没有匹配的插件，试试其他关键词。"
                            }),
                    ),
                )
                .into_any_element();
        }
        let sizes = Rc::new(
            rows.iter()
                .map(|row| {
                    size(
                        Pixels::ZERO,
                        window.rem_size()
                            * match row {
                                CatalogRow::Heading(_, _) => 3.,
                                CatalogRow::Cards(_, _) => 6.5,
                            },
                    )
                })
                .collect(),
        );
        let rows = Rc::new(rows);
        v_flex()
            .size_full()
            .min_h_0()
            .min_w_0()
            .child(content)
            .child(
                div()
                    .relative()
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .child(
                        v_virtual_list(
                            cx.entity(),
                            "plugin-catalog",
                            sizes,
                            move |view, range, _, cx| {
                                range
                                    .map(|ix| {
                                        div().size_full().child(
                                            super::content_column()
                                                .h_full()
                                                .px_6()
                                                .child(view.row(&rows[ix], cx)),
                                        )
                                    })
                                    .collect::<Vec<_>>()
                            },
                        )
                        .track_scroll(&self.scroll),
                    )
                    .child(Scrollbar::vertical(&self.scroll)),
            )
            .into_any_element()
    }
}

impl Render for Plugins {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.entity();
        div()
            .size_full()
            .min_h_0()
            .min_w_0()
            .child(container_query(move |size, window, cx| {
                owner.update(cx, |view, cx| view.page(size.width, window, cx))
            }))
    }
}

struct Sources {
    owner: WeakEntity<Plugins>,
    adding: bool,
    added: bool,
    source: Entity<InputState>,
    picker: Task<()>,
    _owner: Subscription,
    _source: Subscription,
}

impl Sources {
    fn new(
        owner: Entity<Plugins>,
        adding: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let source =
            cx.new(|cx| InputState::new(window, cx).placeholder("Git 地址或本地插件市场目录"));
        let observer = cx.observe(&owner, |_, _, cx| cx.notify());
        let subscription = cx.subscribe(&source, |view, _, event, cx| {
            if matches!(event, InputEvent::Change) {
                view.added = false;
                cx.notify();
            }
        });
        Self {
            owner: owner.downgrade(),
            adding,
            added: false,
            source,
            picker: Task::ready(()),
            _owner: observer,
            _source: subscription,
        }
    }

    fn add(&mut self, cx: &mut Context<Self>) {
        let source = self
            .source
            .read(cx)
            .value()
            .trim()
            .to_string();
        if source.is_empty() || self.added {
            return;
        }
        let completion = cx.weak_entity();
        let _ = self.owner.update(cx, |v, cx| {
            v.request(protocol::Action::Add { source }, Some(completion), cx)
        });
    }

    fn choose_directory(&mut self, cx: &mut Context<Self>) {
        let prompt = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("添加插件市场".into()),
        });
        self.picker = cx.spawn(async move |owner, cx| {
            if let Ok(Ok(Some(paths))) = prompt.await
                && let Some(path) = paths.first()
            {
                let value = path.to_string_lossy().to_string();
                let _ = owner.update_in(cx, |v, window, cx| {
                    v.source
                        .update(cx, |input, cx| input.set_value(value, window, cx));
                    cx.notify();
                });
            }
        });
    }
}

impl Render for Sources {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(owner) = self.owner.upgrade() else {
            return div().into_any_element();
        };
        let view = owner.read(cx);
        let busy = view.busy;
        let error = view.error.clone();
        let markets = view
            .catalog
            .as_ref()
            .map(|c| c.marketplaces.clone())
            .unwrap_or_default();
        let mut body = v_flex()
            .gap_4()
            .when_some(error, |v, e| {
                v.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(e),
                )
            });
        if self.adding {
            body = body
                .when(self.added, |view| {
                    view.child(
                        h_flex()
                            .gap_2()
                            .child(Icon::new(IconName::Check))
                            .child(
                                div()
                                    .text_sm()
                                    .child("插件市场已添加，可关闭窗口浏览插件。"),
                            ),
                    )
                })
                .child(
                    v_flex()
                        .gap_2()
                        .child("插件市场地址")
                        .child(
                            Input::new(&self.source)
                                .aria_label("插件市场地址")
                                .disabled(busy),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child("输入 Git 仓库地址，或选择包含市场清单的本地目录。"),
                        ),
                )
                .child(
                    h_flex()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("choose-plugin-market")
                                .small()
                                .icon(IconName::FolderOpen)
                                .label("选择目录…")
                                .disabled(busy)
                                .on_click(cx.listener(|v, _, _, cx| v.choose_directory(cx))),
                        )
                        .child(
                            Button::new("add-plugin-market")
                                .primary()
                                .small()
                                .label(if self.added {
                                    "已添加"
                                } else if busy {
                                    "正在添加…"
                                } else {
                                    "添加市场"
                                })
                                .disabled(
                                    busy || self.added
                                        || self
                                            .source
                                            .read(cx)
                                            .value()
                                            .trim()
                                            .is_empty(),
                                )
                                .on_click(cx.listener(|v, _, _, cx| v.add(cx))),
                        ),
                );
        } else if markets.is_empty() {
            body = body.child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("还没有市场来源。"),
            );
        } else {
            body = body
                .child(
                    v_flex()
                        .id("plugin-market-sources")
                        .gap_3()
                        .children(markets.into_iter().map(|market| {
                            let source = market.source.clone();
                            let refresh = source.clone();
                            let remove = source.clone();
                            let owner = self.owner.clone();
                            let owner2 = owner.clone();
                            h_flex()
                                .items_start()
                                .gap_3()
                                .py_2()
                                .border_b_1()
                                .border_color(cx.theme().border)
                                .child(
                                    v_flex()
                                        .min_w_0()
                                        .flex_1()
                                        .gap_1()
                                        .child(div().font_semibold().child(market.name))
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(format!("{} 个插件", market.plugins.len())),
                                        )
                                        .child(
                                            div()
                                                .id(SharedString::from(format!(
                                                    "market-source:{source}"
                                                )))
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .truncate()
                                                .child(source.clone())
                                                .tooltip(move |window, cx| {
                                                    Tooltip::new(source.clone()).build(window, cx)
                                                }),
                                        ),
                                )
                                .child(
                                    Button::new(SharedString::from(format!(
                                        "market-refresh:{refresh}"
                                    )))
                                    .ghost()
                                    .small()
                                    .icon(IconName::RefreshCw)
                                    .tooltip("刷新市场")
                                    .accessibility_label("刷新市场")
                                    .disabled(busy)
                                    .on_click(move |_, _, cx| {
                                        let _ = owner.update(cx, |v, cx| {
                                            v.request(
                                                protocol::Action::Refresh {
                                                    source: refresh.clone(),
                                                },
                                                None,
                                                cx,
                                            )
                                        });
                                    }),
                                )
                                .child(
                                    Button::new(SharedString::from(format!(
                                        "market-remove:{remove}"
                                    )))
                                    .ghost()
                                    .small()
                                    .label("移除")
                                    .disabled(busy)
                                    .on_click(move |_, _, cx| {
                                        let _ = owner2.update(cx, |v, cx| {
                                            v.request(
                                                protocol::Action::Remove {
                                                    source: remove.clone(),
                                                },
                                                None,
                                                cx,
                                            )
                                        });
                                    }),
                                )
                        })),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child("刷新或移除市场不会改变已有会话的扩展快照。"),
                );
        }
        body.into_any_element()
    }
}
