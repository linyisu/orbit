//! One searchable settings surface; credentials stay in the account entity,
//! appearance in Shell, and task operations in the workspace dispatcher.
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
        self.settings_page = Some(cx.new(|_| SettingsPage {
            owner,
            accounts,
            appearance,
            accounts_first,
        }));
        cx.notify();
    }
}
pub(super) struct SettingsPage {
    owner: WeakEntity<Workspace>,
    accounts: Entity<crate::accounts::Accounts>,
    appearance: Entity<gpui_shell::ShellRoot>,
    accounts_first: bool,
}
impl Render for SettingsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let accounts = self.accounts.clone();
        let appearance = self.appearance.clone();
        let owner = self.owner.clone();
        let resources = self.owner.clone();
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
                        .default_selected_index(SelectIndex { page_ix: usize::from(self.accounts_first), group_ix: None })
                        .page(SettingPage::new("外观与布局").icon(IconName::Settings)
                            .group(SettingGroup::new().title("外观")
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
                                ).default_value(false)).description("对话内容与输入框使用相同宽度；关闭时随窗口展开").keywords(["宽度", "对话", "布局"]))))
                        .page(SettingPage::new("账号与模型").icon(IconName::User).resettable(false)
                            .group(SettingGroup::new().item(SettingItem::render(move |_, _, _| accounts.clone()).keywords(["账号", "API", "连接", "模型", "密钥"])))
                            .group(SettingGroup::new().item(SettingItem::render(move |_, _, _| {
                                let owner = owner.clone();
                                Button::new("refresh-settings-models").label("刷新模型列表").on_click(move |_, window, cx| {

                                    let _ = owner.update(cx, |view, cx| view.execute(WorkspaceCommand::RefreshModels, window, cx));
                                })
                            }))))
                        .page(SettingPage::new("Agent 资源").icon(IconName::Wrench).resettable(false)
                            .group(SettingGroup::new().items([
                                ("技能", "查看与加载工作区技能", WorkspaceCommand::Skills),
                                ("MCP 工具", "查看工具服务器与连接状态", WorkspaceCommand::Mcp),
                                ("记忆", "查看、添加与搜索项目记忆", WorkspaceCommand::Memory),
                                ("插件", "管理当前工作区的插件", WorkspaceCommand::Plugins),
                            ].into_iter().map(|(label, description, command)| {
                                let resources = resources.clone();
                                SettingItem::render(move |_, _, cx| {
                                    let resources = resources.clone();
                                    h_flex().gap_3().child(v_flex().flex_1().gap_1().child(label)
                                        .child(div().text_sm().text_color(cx.theme().muted_foreground).child(description)))
                                        .child(Button::new(label).small().label("管理…").on_click(move |_, window, cx| {

                                            let _ = resources.update(cx, |view, cx| { view.settings_page = None; view.execute(command, window, cx) });
                                        }))
                                }).keywords([label, description])
                            }))))
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
