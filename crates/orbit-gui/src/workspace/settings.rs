//! Window-level settings navigation with a single scroll owner per page.
mod advanced;
mod fonts;
mod mcp;
mod plugins;
mod resources;
mod shortcuts;
use super::actions::WorkspaceCommand;
use super::*;
use crate::preferences::{ColorMode, Preferences};
use gpui_kit::component::{
    InteractiveElementExt, StyledExt,
    input::{Input, InputState, NumberInput},
    radio::{Radio, RadioGroup},
    scroll::ScrollableElement,
    sidebar::{Sidebar, SidebarGroup, SidebarMenu, SidebarMenuItem},
    switch::Switch,
    tooltip::Tooltip,
};

// Keep the existing page identifiers stable for command destinations.
const PAGES: [(&str, IconName, &str); 8] = [
    (
        "外观与布局",
        IconName::Settings,
        "主题 浅色 深色 系统 字体 字号 font 代码 等宽 宽度 对话 布局",
    ),
    (
        "账号与模型",
        IconName::User,
        "账号 API 连接 模型 密钥 供应商",
    ),
    ("技能", IconName::BookOpen, "技能 Skill Agent"),
    ("MCP 工具", IconName::Wrench, "MCP 工具 服务器"),
    ("会话记忆", IconName::Brain, "记忆 偏好 事实"),
    ("插件市场", IconName::Puzzle, "插件 市场 plugin"),
    (
        "快捷键",
        IconName::Command,
        "快捷键 keyboard Enter Ctrl 发送 换行 新任务 搜索 切换 聚焦 设置 工作面板",
    ),
    (
        "高级 Agent",
        IconName::Settings,
        "Agent 能力 禁用 工具 上限 预算 轮数 调用 子代理 命令 超时 自动压缩",
    ),
];

#[derive(Clone)]
struct GlobalLibrary {
    service: orbit_client::library::Library,
    runtime: Arc<tokio::runtime::Runtime>,
}

const GROUPS: [(&str, &[usize]); 2] = [("全局设置", &[0, 1, 2, 3, 5, 6, 7]), ("当前会话", &[4])];

impl Workspace {
    pub(crate) fn settings(
        &mut self,
        accounts_first: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(page) = &self.settings_page {
            // The Settings command always opens application preferences. Direct
            // resource commands select their destination immediately afterwards.
            page.update(cx, |page, cx| {
                page.select_page(usize::from(accounts_first), window, cx)
            });
            cx.emit(WorkspaceEvent::Settings(page.clone()));
            return;
        }
        // Explicit appearance extensions remain available. The bundled setting
        // uses native controls and no longer depends on loading a toggle script.
        if self.appearance.is_none()
            && let Some(path) = self.options.appearance.clone()
        {
            match Appearance::load(&path, window, cx) {
                Ok(appearance) => self.appearance = Some(appearance),
                Err(error) => {
                    self.report_error(format!("无法打开设置：{error:#}"), cx);
                    return;
                }
            }
        }
        let appearance = self
            .appearance
            .as_ref()
            .map(|view| view.view.clone());
        let accounts = cx.new(|cx| {
            crate::accounts::Accounts::new(&self.options, self.runtime.clone(), window, cx)
        });
        let owner = cx.weak_entity();
        let session = self
            .info
            .as_ref()
            .map(|info| info.id.clone());
        let workspace = self.location.to_string();
        let environment = self.environment();
        let library = GlobalLibrary {
            service: orbit_client::library::Library::for_options(
                &self
                    .options
                    .connection_options()
                    .expect("validated connection options"),
                self.options.config.clone(),
                self.options.auth_dir.clone(),
            ),
            runtime: self.runtime.clone(),
        };
        let skills = cx.new(|cx| {
            resources::Resources::new(
                owner.clone(),
                session.clone(),
                workspace.clone(),
                resources::Kind::Skills,
                library.clone(),
                window,
                cx,
            )
        });
        let memory = cx.new(|cx| {
            resources::Resources::new(
                owner.clone(),
                session.clone(),
                workspace.clone(),
                resources::Kind::Memory,
                library.clone(),
                window,
                cx,
            )
        });
        let plugins = cx.new(|cx| {
            plugins::Plugins::new(
                owner.clone(),
                session.clone(),
                workspace.clone(),
                library.clone(),
                window,
                cx,
            )
        });
        let fonts = cx.new(|cx| fonts::Fonts::new(window, cx));
        let shortcuts = cx.new(|cx| shortcuts::Shortcuts::new(window, cx));
        let advanced =
            cx.new(|cx| advanced::Advanced::new(&self.options, self.runtime.clone(), window, cx));
        let mcp = cx.new(|cx| {
            mcp::Mcp::new(
                &self.options,
                self.runtime.clone(),
                owner.clone(),
                session.clone(),
                workspace.clone(),
                window,
                cx,
            )
        });
        let page = cx.new(|cx| {
            let account_owner = owner.clone();
            let account_events = cx.subscribe_in(
                &accounts,
                window,
                move |_: &mut SettingsPage, _, _, window, cx| {
                    let _ = account_owner.update(cx, |view, cx| {
                        view.execute(WorkspaceCommand::RefreshModels, window, cx)
                    });
                },
            );
            let query = cx.new(|cx| InputState::new(window, cx).placeholder("搜索设置…"));
            let query_events = cx.subscribe_in(
                &query,
                window,
                |page: &mut SettingsPage, input, event, window, cx| {
                    if matches!(event, InputEvent::Change) {
                        let query = input
                            .read(cx)
                            .value()
                            .trim()
                            .to_lowercase();
                        if !page_matches(page.initial_page, &query)
                            && let Some(ix) = (0..PAGES.len()).find(|ix| page_matches(*ix, &query))
                        {
                            page.deactivate_shortcuts(window, cx);
                            page.initial_page = ix;
                        }
                        cx.notify();
                    }
                },
            );
            let font_size = cx.new(|cx| {
                InputState::new(window, cx)
                    .default_value(Preferences::font_size(cx).to_string())
                    .min(12.)
                    .max(20.)
                    .step(1.)
            });
            let size_events =
                cx.subscribe(&font_size, |page: &mut SettingsPage, input, event, cx| {
                    if matches!(event, InputEvent::Change)
                        && let Ok(size) = input.read(cx).value().parse::<f64>()
                    {
                        let size = size.round().clamp(12., 20.) as u8;
                        page.applied_font_size = size;
                        Preferences::set_font_size(size, cx);
                    }
                });
            let preferences = cx.observe_global_in::<Preferences>(
                window,
                |page: &mut SettingsPage, window, cx| {
                    let size = Preferences::font_size(cx);
                    if size != page.applied_font_size {
                        page.applied_font_size = size;
                        page.font_size.update(cx, |input, cx| {
                            input.set_value(size.to_string(), window, cx)
                        });
                    }
                    cx.notify();
                },
            );
            let contexts = vec![ResourceContext {
                session,
                workspace,
                memory: memory.clone(),
            }];
            SettingsPage {
                owner,
                library,
                accounts,
                appearance,
                initial_page: usize::from(accounts_first),
                skills,
                memory,
                plugins,
                fonts,
                shortcuts,
                mcp,
                advanced,
                query,
                font_size,
                applied_font_size: Preferences::font_size(cx),
                environment,
                contexts,
                context: 0,
                scrolls: std::array::from_fn(|_| ScrollHandle::new()),
                _subscriptions: vec![query_events, size_events, preferences, account_events],
            }
        });
        self.settings_page = Some(page.clone());
        cx.emit(WorkspaceEvent::Settings(page));
        cx.notify();
    }
}

pub(crate) struct SettingsPage {
    library: GlobalLibrary,
    owner: WeakEntity<Workspace>,
    accounts: Entity<crate::accounts::Accounts>,
    appearance: Option<Entity<gpui_shell::ShellRoot>>,
    pub(super) initial_page: usize,
    skills: Entity<resources::Resources>,
    memory: Entity<resources::Resources>,
    plugins: Entity<plugins::Plugins>,
    mcp: Entity<mcp::Mcp>,
    advanced: Entity<advanced::Advanced>,
    fonts: Entity<fonts::Fonts>,
    shortcuts: Entity<shortcuts::Shortcuts>,
    query: Entity<InputState>,
    font_size: Entity<InputState>,
    applied_font_size: u8,
    environment: String,
    contexts: Vec<ResourceContext>,
    context: usize,
    scrolls: [ScrollHandle; PAGES.len()],
    _subscriptions: Vec<Subscription>,
}

struct ResourceContext {
    session: Option<String>,
    workspace: String,
    memory: Entity<resources::Resources>,
}

impl SettingsPage {
    pub(crate) fn check_account_edit(
        &mut self,
        step: &mut crate::accounts::AccountCheck,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<bool> {
        self.accounts
            .update(cx, |accounts, cx| {
                accounts.check_account_edit(step, window, cx)
            })
    }
    /// Retained forms belong to this workspace even when another settings page
    /// or the conversation is visible. A blank conversation is not disposable
    /// while one of those forms is being edited or saved.
    pub(super) fn has_pending_work(&self, cx: &App) -> bool {
        self.accounts
            .read(cx)
            .has_pending_work()
            || self
                .advanced
                .read(cx)
                .has_pending_work(cx)
            || self.mcp.read(cx).has_pending_work()
            || self
                .shortcuts
                .read(cx)
                .has_pending_work(cx)
            || self.skills.read(cx).has_pending_work()
            || self.plugins.read(cx).has_pending_work()
            || self.contexts.iter().any(|context| {
                context
                    .memory
                    .read(cx)
                    .has_pending_work()
            })
    }

    pub(super) fn bind_context(
        &mut self,
        session: Option<String>,
        workspace: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.mcp.update(cx, |mcp, cx| {
            mcp.bind_context(session.clone(), workspace.clone(), cx)
        });
        if self.contexts[self.context].session == session
            && self.contexts[self.context].workspace == workspace
        {
            return;
        }
        let context = self
            .contexts
            .iter()
            .position(|context| context.session == session && context.workspace == workspace)
            .unwrap_or_else(|| {
                let owner = self.owner.clone();
                let memory = cx.new(|cx| {
                    resources::Resources::new(
                        owner.clone(),
                        session.clone(),
                        workspace.clone(),
                        resources::Kind::Memory,
                        self.library.clone(),
                        window,
                        cx,
                    )
                });
                self.contexts.push(ResourceContext {
                    session: session.clone(),
                    workspace: workspace.clone(),
                    memory,
                });
                self.contexts.len() - 1
            });
        self.context = context;
        let resources = &self.contexts[context];
        self.memory = resources.memory.clone();
        self.skills.update(cx, |skills, cx| {
            skills.bind_context(session.clone(), workspace.clone(), cx)
        });
        self.plugins.update(cx, |plugins, cx| {
            plugins.bind_context(session, workspace, cx)
        });
        cx.notify();
    }

    pub(super) fn select_page(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix != 6 {
            self.deactivate_shortcuts(window, cx);
        }
        self.initial_page = ix.min(PAGES.len() - 1);
        self.query
            .update(cx, |input, cx| input.set_value("", window, cx));
        cx.notify();
    }

    fn deactivate_shortcuts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self
            .shortcuts
            .update(cx, |shortcuts, cx| shortcuts.deactivate(window, cx))
        {
            self.query
                .update(cx, |input, cx| input.focus(window, cx));
        }
    }

    fn appearance_page(&self, cx: &App) -> impl IntoElement + use<> {
        let reset_size = self.font_size.clone();
        v_flex()
            .gap_6()
            .child(
                section("颜色模式", "跟随系统外观，或始终使用浅色、深色主题。", cx)
                    .child(color_mode(cx)),
            )
            .child(
                section("主题", "当前提供的 Orbit 主题。选择后立即应用。", cx)
                    .child(theme_previews(cx)),
            )
            .child(
                section("文字与布局", "调整应用的阅读密度与内容宽度。", cx)
                    .child(
                        setting_row("文字大小", "用于界面、对话正文与输入框", cx).child(
                            h_flex()
                                .gap_2()
                                .child(
                                    div()
                                        .w_32()
                                        .child(NumberInput::new(&self.font_size).small()),
                                )
                                .child(
                                    Button::new("reset-font-size")
                                        .outline()
                                        .small()
                                        .label("重置")
                                        .accessibility_label("重置文字大小")
                                        .disabled(Preferences::font_size(cx) == 16)
                                        .on_click(move |_, window, cx| {
                                            Preferences::set_font_size(16, cx);
                                            reset_size.update(cx, |input, cx| {
                                                input.set_value("16", window, cx)
                                            });
                                        }),
                                ),
                        ),
                    )
                    .child(
                        setting_row(
                            "居中窄栏",
                            "对话内容与输入框始终对齐；开启后限制最大宽度",
                            cx,
                        )
                        .child(
                            Switch::new("comfortable-width")
                                .accessibility_label("居中窄栏")
                                .checked(Preferences::comfortable_width(cx))
                                .on_change(|value, _, cx| {
                                    Preferences::set_comfortable_width(*value, cx)
                                }),
                        ),
                    ),
            )
            .child(
                section("字体", "选择已安装的字体，修改后立即应用。", cx).child(self.fonts.clone()),
            )
            .when_some(self.appearance.clone(), |view, appearance| {
                view.child(
                    section("自定义外观扩展", "由启动时指定的外观文件提供。", cx).child(appearance),
                )
            })
    }

    fn content(&self, cx: &mut Context<Self>) -> AnyElement {
        match self.initial_page {
            0 => self
                .appearance_page(cx)
                .into_any_element(),
            1 => self.accounts.clone().into_any_element(),
            2 => self.skills.clone().into_any_element(),
            3 => self.mcp.clone().into_any_element(),
            4 => self.memory.clone().into_any_element(),
            6 => self
                .shortcuts
                .clone()
                .into_any_element(),
            7 => self.advanced.clone().into_any_element(),
            _ => div().into_any_element(),
        }
    }
}

impl Render for SettingsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self
            .query
            .read(cx)
            .value()
            .trim()
            .to_lowercase();
        let visible: Vec<_> = (0..PAGES.len())
            .filter(|ix| page_matches(*ix, &query))
            .collect();
        let has_results = !visible.is_empty();
        let mut sidebar = Sidebar::new("settings-navigation")
            .w_full()
            .h_full()
            .border_0()
            .collapsible(false)
            .collapsed(false)
            .header(
                Input::new(&self.query)
                    .small()
                    .prefix(Icon::new(IconName::Search))
                    .cleanable(true)
                    .aria_label("搜索设置"),
            );
        for (label, pages) in GROUPS {
            let pages: Vec<_> = pages
                .iter()
                .copied()
                .filter(|ix| visible.contains(ix))
                .collect();
            if pages.is_empty() {
                continue;
            }
            sidebar = sidebar.child(
                SidebarGroup::new(label).child(SidebarMenu::new().gap_1().children(
                    pages.into_iter().map(|ix| {
                        SidebarMenuItem::new(PAGES[ix].0)
                            .icon(PAGES[ix].1)
                            .active(self.initial_page == ix)
                            .on_click(cx.listener(move |view, _, window, cx| {
                                if ix != 6 {
                                    view.deactivate_shortcuts(window, cx);
                                }
                                view.initial_page = ix;
                                cx.notify();
                            }))
                    }),
                )),
            );
        }
        let body = if !has_results {
            div()
                .p_6()
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .child("没有匹配的设置，试试其他关键词。")
                .into_any_element()
        } else if self.initial_page == 5 {
            // The catalogue owns the complete available viewport, including its
            // virtualized collection. There is no outer settings scroll region.
            div()
                .size_full()
                .min_h_0()
                .min_w_0()
                .child(self.plugins.clone())
                .into_any_element()
        } else {
            let (title, description) = match self.initial_page {
                0 => (
                    "外观与布局",
                    "让 Orbit 适应你的阅读与工作习惯，修改后立即应用。",
                ),
                1 => (
                    "账号与模型",
                    "在本机管理账号、API 凭据与模型，供所有项目使用。",
                ),
                2 => (
                    "技能",
                    "全局管理技能，按需应用到当前会话；其他会话保持独立。",
                ),
                3 => ("MCP 工具", "配置工具服务器，可应用到当前会话和新建会话。"),
                4 => ("会话记忆", "管理当前会话保存的事实与偏好。"),
                7 => (
                    "高级 Agent",
                    "设置新建会话的运行预算、能力上限与子 Agent 参数。",
                ),
                _ => ("快捷键", "修改后立即应用于整个应用，并保存到本机设置。"),
            };
            let content = self.content(cx);
            let scroll = &self.scrolls[self.initial_page.min(PAGES.len() - 1)];
            div()
                .id(SharedString::from(format!(
                    "settings-page:{}",
                    PAGES[self.initial_page].0
                )))
                .relative()
                .size_full()
                .min_h_0()
                .min_w_0()
                .child(
                    div()
                        .id("settings-page-content")
                        .size_full()
                        .overflow_y_scroll()
                        .lock_scroll_axis()
                        .track_scroll(scroll)
                        .child(
                            content_column()
                                .p_6()
                                .gap_6()
                                .child(
                                    v_flex()
                                        .gap_2()
                                        .child(
                                            div()
                                                .text_xl()
                                                .font_semibold()
                                                .child(title),
                                        )
                                        .child(
                                            div()
                                                .text_sm()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(description),
                                        ),
                                )
                                .child(content),
                        ),
                )
                .vertical_scrollbar(scroll)
                .into_any_element()
        };
        let back = self.owner.clone();
        let shortcuts = self.shortcuts.clone();
        let scope = if self.initial_page == 4 {
            format!(
                "{} · {}",
                self.environment, self.contexts[self.context].workspace
            )
        } else if matches!(self.initial_page, 2 | 5) {
            "全局库 · 按会话应用".to_owned()
        } else {
            "全局 · 对所有项目生效".to_owned()
        };
        let scope_tooltip = scope.clone();
        v_flex()
            .size_full()
            .min_h_0()
            .min_w_0()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                h_flex()
                    .flex_shrink_0()
                    .px_3()
                    .py_2()
                    .gap_2()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("settings-back")
                            .ghost()
                            .small()
                            .icon(IconName::ArrowLeft)
                            .accessibility_label("返回会话")
                            .tooltip("返回会话")
                            .on_click(move |_, window, cx| {
                                shortcuts
                                    .update(cx, |shortcuts, cx| shortcuts.deactivate(window, cx));
                                let _ = back.update(cx, |view, cx| {
                                    cx.emit(WorkspaceEvent::CloseSettings);
                                    view.input
                                        .update(cx, |input, cx| input.focus(window, cx));
                                    cx.notify();
                                });
                            }),
                    )
                    .child(div().font_semibold().child("设置"))
                    .child(
                        div()
                            .id("settings-scope")
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_ellipsis()
                            .text_color(cx.theme().muted_foreground)
                            .child(scope)
                            .tooltip(move |window, cx| {
                                Tooltip::new(scope_tooltip.clone()).build(window, cx)
                            }),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .child(
                        h_resizable("settings-panes")
                            .child(
                                resizable_panel()
                                    .size(
                                        (window.rem_size() * 12.5)
                                            .min(window.viewport_size().width * 0.23),
                                    )
                                    .size_range(window.rem_size() * 9. ..window.rem_size() * 18.)
                                    .child(sidebar),
                            )
                            .child(resizable_panel().child(body)),
                    ),
            )
    }
}

fn page_matches(ix: usize, query: &str) -> bool {
    let (title, _, keywords) = PAGES[ix.min(PAGES.len() - 1)];
    query.is_empty()
        || title.to_lowercase().contains(query)
        || keywords.to_lowercase().contains(query)
}

fn content_column() -> Div {
    v_flex()
        .w_full()
        .max_w(rems(60.))
        .mx_auto()
}

fn section(title: &'static str, description: &'static str, cx: &App) -> Div {
    v_flex().w_full().gap_4().child(
        v_flex()
            .gap_1()
            .pb_3()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(div().font_semibold().child(title))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(description),
            ),
    )
}

fn setting_row(title: &'static str, description: &'static str, cx: &App) -> Div {
    h_flex()
        .w_full()
        .gap_4()
        .flex_wrap()
        .child(
            v_flex()
                .flex_1()
                .min_w_48()
                .gap_1()
                .child(title)
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(description),
                ),
        )
}

fn color_mode(cx: &App) -> impl IntoElement + use<> {
    let selected = match Preferences::color_mode(cx) {
        ColorMode::System => 0,
        ColorMode::Light => 1,
        ColorMode::Dark => 2,
    };
    RadioGroup::horizontal("appearance-mode")
        .flex_none()
        .selected_index(Some(selected))
        .child(Radio::new("system").label("跟随系统"))
        .child(Radio::new("light").label("浅色"))
        .child(Radio::new("dark").label("深色"))
        .on_change(|ix, window, cx| {
            Preferences::set_color_mode(
                match ix {
                    1 => ColorMode::Light,
                    2 => ColorMode::Dark,
                    _ => ColorMode::System,
                },
                window,
                cx,
            );
        })
}

fn theme_previews(cx: &App) -> impl IntoElement + use<> {
    let theme = cx.theme();
    h_flex().gap_3().flex_wrap().children(
        [
            (ColorMode::Light, theme.light_theme.clone()),
            (ColorMode::Dark, theme.dark_theme.clone()),
        ]
        .into_iter()
        .map(|(mode, config)| {
            let color = |value: &Option<SharedString>, fallback| {
                value
                    .as_ref()
                    .and_then(|value| gpui_kit::component::theme::try_parse_color(value).ok())
                    .unwrap_or(fallback)
            };
            let background = color(&config.colors.background, theme.background);
            let foreground = color(&config.colors.foreground, theme.foreground);
            let muted = color(&config.colors.muted, theme.muted);
            let sidebar = color(&config.colors.sidebar, theme.sidebar);
            let selected = theme.mode.is_dark() == (mode == ColorMode::Dark);
            Button::new(if mode == ColorMode::Dark {
                "theme-dark"
            } else {
                "theme-light"
            })
            .outline()
            .accessibility_label(config.name.clone())
            .w_48()
            .h_32()
            .p_2()
            .border_color(if selected {
                theme.primary
            } else {
                theme.border
            })
            .child(
                v_flex()
                    .size_full()
                    .gap_2()
                    .child(
                        h_flex()
                            .flex_1()
                            .min_h_0()
                            .bg(background)
                            .border_1()
                            .border_color(theme.border)
                            .child(
                                v_flex()
                                    .w_10()
                                    .h_full()
                                    .p_2()
                                    .gap_2()
                                    .bg(sidebar)
                                    .child(div().h_1().w_full().bg(foreground))
                                    .child(
                                        div()
                                            .h_1()
                                            .w_full()
                                            .bg(foreground.opacity(0.3)),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .p_3()
                                    .gap_2()
                                    .child(div().w_12().h_1().bg(foreground))
                                    .child(
                                        div()
                                            .w_full()
                                            .h_1()
                                            .bg(foreground.opacity(0.3)),
                                    )
                                    .child(div().w_full().h_5().bg(muted)),
                            ),
                    )
                    .child(
                        h_flex()
                            .justify_between()
                            .child(config.name.clone())
                            .when(selected, |view| {
                                view.child(Icon::new(IconName::Check).small())
                            }),
                    ),
            )
            .on_click(move |_, window, cx| Preferences::set_color_mode(mode, window, cx))
        }),
    )
}
