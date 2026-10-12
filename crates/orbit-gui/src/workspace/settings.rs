//! Searchable settings with retained, inline editors for each resource type.
mod fonts;
mod mcp;
mod plugins;
mod resources;
use super::actions::WorkspaceCommand;
use super::*;
use gpui_kit::component::setting::{
    NumberFieldOptions, SelectIndex, SettingField, SettingGroup, SettingItem, SettingPage, Settings,
};

impl Workspace {
    pub(super) fn settings(
        &mut self,
        accounts_first: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.appearance.is_none() {
            let path =
                self.options.appearance.clone().unwrap_or_else(|| {
                    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("appearance")
                });
            match Appearance::load(&path, window, cx) {
                Ok(appearance) => self.appearance = Some(appearance),
                Err(error) => {
                    self.report_error(format!("无法打开设置：{error:#}"), cx);
                    return;
                }
            }
        }
        let appearance = self.appearance.as_ref().unwrap().view.clone();
        let accounts = cx.new(|cx| {
            crate::accounts::Accounts::new(&self.options, self.runtime.clone(), window, cx)
        });

        let owner = cx.weak_entity();
        let session = self.info.as_ref().map(|info| info.id.clone());
        let skills = cx.new(|cx| {
            resources::Resources::new(
                owner.clone(),
                session.clone(),
                resources::Kind::Skills,
                window,
                cx,
            )
        });
        let memory = cx.new(|cx| {
            resources::Resources::new(
                owner.clone(),
                session.clone(),
                resources::Kind::Memory,
                window,
                cx,
            )
        });
        let plugins = cx.new(|cx| plugins::Plugins::new(owner.clone(), session, window, cx));
        let fonts = cx.new(|cx| fonts::Fonts::new(window, cx));
        let mcp = cx.new(|cx| mcp::Mcp::new(&self.options, self.runtime.clone(), window, cx));
        self.settings_page = Some(cx.new(|_| SettingsPage {
            owner,
            accounts,
            appearance,
            initial_page: usize::from(accounts_first),
            skills,
            memory,
            plugins,
            fonts,
            mcp,
        }));
        cx.notify();
    }
}
pub(super) struct SettingsPage {
    owner: WeakEntity<Workspace>,
    accounts: Entity<crate::accounts::Accounts>,
    appearance: Entity<gpui_shell::ShellRoot>,
    pub(super) initial_page: usize,
    skills: Entity<resources::Resources>,
    memory: Entity<resources::Resources>,
    plugins: Entity<plugins::Plugins>,
    mcp: Entity<mcp::Mcp>,
    fonts: Entity<fonts::Fonts>,
}
impl Render for SettingsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let accounts = self.accounts.clone();
        let appearance = self.appearance.clone();
        let owner = self.owner.clone();
        let skills = self.skills.clone();
        let memory = self.memory.clone();
        let plugins = self.plugins.clone();
        let mcp = self.mcp.clone();
        let fonts = self.fonts.clone();
        let back = self.owner.clone();
        v_flex().size_full().min_h_0().bg(cx.theme().background).text_color(cx.theme().foreground)
            .child(gpui_kit::component::TitleBar::new().child(h_flex().gap_2()
                .child(Button::new("settings-back").ghost().small().icon(IconName::ArrowLeft)
                    .accessibility_label("返回会话").tooltip("返回会话")
                    .on_click(move |_, window, cx| { let _ = back.update(cx, |view, cx| {
                        view.settings_page = None;
                        view.input.update(cx, |input, cx| input.focus(window, cx));
                        cx.notify();
                    }); }))
                .child("设置")))
            .child(div().flex_1().min_h_0().child(
                    Settings::new("orbit-settings").with_group_variant(gpui_kit::component::group_box::GroupBoxVariant::Normal)
                        .sidebar_width((window.rem_size() * 15.).min(window.viewport_size().width * 0.28))
                        .sidebar_size_range(window.rem_size() * 9. .. window.rem_size() * 20.)
                        .default_selected_index(SelectIndex { page_ix: self.initial_page, group_ix: None })
                        .page(SettingPage::new("外观与布局").icon(IconName::Settings)
                            .group(SettingGroup::new().title("外观")
                                .item(SettingItem::render(|_, _, cx| theme_previews(cx)).keywords(["主题", "浅色", "深色"]))
                                .item(SettingItem::new("深色主题", SettingField::render(move |_, _, _| div().w_12().h_8().child(appearance.clone())))
                                    .description("切换应用的浅色与深色外观").keywords(["主题", "浅色", "深色"]))
                                .item(SettingItem::new("文字大小", SettingField::number_input(
                                    NumberFieldOptions { min: 12., max: 20., step: 1. },
                                    |cx| crate::preferences::Preferences::font_size(cx) as f64,
                                    |size, cx| crate::preferences::Preferences::set_font_size(size.round() as u8, cx),
                                ).default_value(16.)).description("调整界面与对话的文字大小").keywords(["字体", "字号"]))
                                .item(SettingItem::new("居中窄栏", SettingField::switch(
                                    crate::preferences::Preferences::comfortable_width,
                                    crate::preferences::Preferences::set_comfortable_width,
                                ).default_value(false)).description("对话内容与输入框使用相同宽度；关闭时随窗口展开").keywords(["宽度", "对话", "布局"])))
                            .group(SettingGroup::new().title("字体").item(SettingItem::render(move |_, _, _| fonts.clone()).keywords(["字体", "font", "等宽", "代码"]))))
                        .page(SettingPage::new("账号与模型").icon(IconName::User).resettable(false)
                            .group(SettingGroup::new().item(SettingItem::render(move |_, _, _| accounts.clone()).keywords(["账号", "API", "连接", "模型", "密钥"])))
                            .group(SettingGroup::new().item(SettingItem::render(move |_, _, _| {
                                let owner = owner.clone();
                                Button::new("refresh-settings-models").label("刷新模型列表").on_click(move |_, window, cx| {

                                    let _ = owner.update(cx, |view, cx| view.execute(WorkspaceCommand::RefreshModels, window, cx));
                                })
                            }))))
                        .page(SettingPage::new("技能").icon(IconName::BookOpen).resettable(false)
                            .group(SettingGroup::new().item(SettingItem::render(move |_, _, _| skills.clone()).keywords(["技能", "Skill", "Agent"]))))
                        .page(SettingPage::new("MCP 工具").icon(IconName::Wrench).resettable(false)
                            .group(SettingGroup::new().item(SettingItem::render(move |_, _, _| mcp.clone()).keywords(["MCP", "工具", "服务器"]))))
                        .page(SettingPage::new("会话记忆").icon(IconName::Brain).resettable(false)
                            .group(SettingGroup::new().item(SettingItem::render(move |_, _, _| memory.clone()).keywords(["记忆", "偏好"]))))
                        .page(SettingPage::new("插件市场").icon(IconName::Puzzle).resettable(false).header_style(&StyleRefinement::default().hidden())
                            .group(SettingGroup::new().item(SettingItem::render(move |_, _, _| plugins.clone()).keywords(["插件", "市场"]))))
                        .page(SettingPage::new("快捷键").icon(IconName::Command).resettable(false)
                            .group(SettingGroup::new().items([
                                ("发送消息", "Enter"), ("换行", "Shift+Enter"), ("新任务", "Ctrl+N"),
                                ("搜索命令", "Ctrl+Shift+P"), ("搜索会话", "Ctrl+P"),
                                ("切换任务", "Ctrl+Tab / Ctrl+Shift+Tab"), ("聚焦输入框", "Ctrl+L"),
                                ("工作面板", "Ctrl+Shift+E"), ("设置", "Ctrl+,"),
                            ].into_iter().map(|(label, shortcut)| SettingItem::render(move |_, _, cx|
                                h_flex().gap_4().child(div().flex_1().child(label))
                                    .child(div().text_sm().text_color(cx.theme().muted_foreground).child(shortcut))
                            ).keywords([label, shortcut])))))

            ))
    }
}

fn theme_previews(cx: &mut App) -> impl IntoElement + use<> {
    use gpui_kit::component::{Theme, ThemeMode};
    let theme = cx.theme();
    h_flex().gap_3().flex_wrap().children(
        [
            (false, "浅色", theme.light_theme.clone()),
            (true, "深色", theme.dark_theme.clone()),
        ]
        .into_iter()
        .map(|(dark, label, config)| {
            let color = |value: &Option<SharedString>, fallback| {
                value
                    .as_ref()
                    .and_then(|v| gpui_kit::component::theme::try_parse_color(v).ok())
                    .unwrap_or(fallback)
            };
            let background = color(&config.colors.background, theme.background);
            let foreground = color(&config.colors.foreground, theme.foreground);
            let muted = color(&config.colors.muted, theme.muted);
            let sidebar = color(&config.colors.sidebar, theme.sidebar);
            Button::new(if dark { "theme-dark" } else { "theme-light" })
                .outline()
                .accessibility_label(label)
                .w_48()
                .h_32()
                .p_2()
                .border_color(if theme.mode.is_dark() == dark {
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
                                        .child(div().h_1().w_full().bg(foreground.opacity(0.3))),
                                )
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .p_3()
                                        .gap_2()
                                        .child(div().w_12().h_1().bg(foreground))
                                        .child(div().w_full().h_1().bg(foreground.opacity(0.3)))
                                        .child(div().w_full().h_5().bg(muted)),
                                ),
                        )
                        .child(
                            h_flex()
                                .justify_between()
                                .child(label)
                                .when(theme.mode.is_dark() == dark, |v| {
                                    v.child(Icon::new(IconName::Check).small())
                                }),
                        ),
                )
                .on_click(move |_, _, cx| {
                    Theme::change(
                        if dark {
                            ThemeMode::Dark
                        } else {
                            ThemeMode::Light
                        },
                        None,
                        cx,
                    );
                    crate::preferences::Preferences::set_dark(dark, cx);
                    crate::preferences::Preferences::apply_font(cx);
                })
        }),
    )
}
