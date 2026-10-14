//! Installed-font choices share the application preference owner. The native
//! select owns search, virtualized results, focus, and keyboard navigation.
use crate::preferences::Preferences;
use gpui_kit::component::{
    ActiveTheme, Disableable, Sizable,
    button::Button,
    h_flex,
    searchable_list::{SearchableListItem, SearchableVec},
    select::{Select, SelectEvent, SelectState},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;

#[derive(Clone)]
struct FontChoice {
    value: Option<SharedString>,
    family: SharedString,
}

impl SearchableListItem for FontChoice {
    type Value = Option<SharedString>;

    fn title(&self) -> SharedString {
        match &self.value {
            Some(family) => family.clone(),
            None => format!("默认（{}）", self.family).into(),
        }
    }

    fn value(&self) -> &Self::Value {
        &self.value
    }

    fn render(&self, _: &mut Window, _: &mut App) -> impl IntoElement {
        div()
            .font_family(self.family.clone())
            .child(self.title())
    }
}

type FontPicker = SelectState<SearchableVec<FontChoice>>;

pub(super) struct Fonts {
    ui: Entity<FontPicker>,
    code: Entity<FontPicker>,
    _subscriptions: Vec<Subscription>,
}

impl Fonts {
    pub(super) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (ui_default, code_default) = crate::theme::default_fonts(cx);
        let ui = picker(ui_default, Preferences::ui_font(cx), window, cx);
        let code = picker(code_default, Preferences::code_font(cx), window, cx);
        let ui_events = cx.subscribe(&ui, |_, _, event, cx| {
            let SelectEvent::Confirm(value) = event;
            Preferences::set_ui_font(value.clone().flatten(), cx);
        });
        let code_events = cx.subscribe(&code, |_, _, event, cx| {
            let SelectEvent::Confirm(value) = event;
            Preferences::set_code_font(value.clone().flatten(), cx);
        });
        let preferences = cx.observe_global_in::<Preferences>(window, |view, window, cx| {
            view.sync(window, cx);
        });
        Self {
            ui,
            code,
            _subscriptions: vec![ui_events, code_events, preferences],
        }
    }

    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (picker, configured) in [
            (&self.ui, Preferences::ui_font(cx)),
            (&self.code, Preferences::code_font(cx)),
        ] {
            let value = installed_choice(configured, cx);
            if picker.read(cx).selected_value() != Some(&value) {
                picker.update(cx, |picker, cx| {
                    picker.set_selected_value(&value, window, cx);
                });
            }
        }
        cx.notify();
    }

    fn font_section(&self, code: bool, cx: &App) -> impl IntoElement + use<> {
        let configured = if code {
            Preferences::code_font(cx)
        } else {
            Preferences::ui_font(cx)
        };
        let unavailable = configured
            .as_ref()
            .filter(|family| !crate::theme::installed_fonts(cx).contains(family));
        let family = if code {
            cx.theme().mono_font_family.clone()
        } else {
            cx.theme().font_family.clone()
        };
        let title = if code { "代码字体" } else { "界面字体" };
        let description = if code {
            "用于代码块、行内代码与编辑器，建议选择等宽字体"
        } else {
            "用于应用界面、对话正文与输入框"
        };
        let picker = if code { &self.code } else { &self.ui };
        v_flex()
            .w_full()
            .min_w_0()
            .gap_3()
            .when(code, |v| {
                v.pt_5().border_t_1().border_color(cx.theme().border)
            })
            .child(
                h_flex()
                    .w_full()
                    .gap_3()
                    .flex_wrap()
                    .child(
                        v_flex()
                            .flex_1()
                            .flex_basis(rems(24.))
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
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .min_w_0()
                            .max_w_full()
                            .flex_shrink_0()
                            .child(
                                // Select's behavioral root fills its parent;
                                // the form owns its frame instead of refining
                                // only the inner trigger width.
                                div().w_64().max_w_full().h_8().child(
                                    Select::new(picker)
                                        .accessibility_label(title)
                                        .menu_width(rems(24.))
                                        .menu_max_h(rems(20.))
                                        .search_placeholder("搜索已安装字体…")
                                        .empty(|_, cx| {
                                            div()
                                                .p_4()
                                                .text_color(cx.theme().muted_foreground)
                                                .child("没有匹配的字体")
                                        }),
                                ),
                            )
                            .child(
                                Button::new(if code {
                                    "reset-code-font"
                                } else {
                                    "reset-ui-font"
                                })
                                .outline()
                                .small()
                                .label("重置")
                                .accessibility_label(if code {
                                    "重置代码字体"
                                } else {
                                    "重置界面字体"
                                })
                                .disabled(configured.is_none())
                                .on_click(move |_, _, cx| {
                                    if code {
                                        Preferences::set_code_font(None, cx);
                                    } else {
                                        Preferences::set_ui_font(None, cx);
                                    }
                                }),
                            ),
                    ),
            )
            .when_some(unavailable, |v, family| {
                v.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("字体「{family}」未安装，当前使用默认字体。")),
                )
            })
            .child(
                v_flex()
                    .gap_2()
                    .p_4()
                    .bg(cx.theme().muted.opacity(0.45))
                    .rounded(cx.theme().radius)
                    .font_family(family)
                    .when(code, |v| {
                        v.text_size(rems(0.875)).line_height(relative(1.5))
                    })
                    .child(if code {
                        "fn main() { println!(\"Hello, Orbit!\"); }"
                    } else {
                        "你好，Orbit。让想法变成现实。"
                    })
                    .child(if code {
                        "// 中文注释 · 0O 1lI {} [] () => != 0123456789 · 📦 🚀 ✅"
                    } else {
                        "The quick brown fox jumps over the lazy dog. 0123456789 · 📦 🚀 ✅"
                    }),
            )
            .when(!crate::theme::has_emoji_font(cx), |v| {
                v.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("未发现 emoji 字体，部分表情可能无法显示。系统文字与符号回退仍生效，原文与复制内容保持完整。"),
                )
            })
    }
}

impl Render for Fonts {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .min_w_0()
            .gap_5()
            .child(self.font_section(false, cx))
            .child(self.font_section(true, cx))
    }
}

fn installed_choice(configured: Option<SharedString>, cx: &App) -> Option<SharedString> {
    configured.filter(|family| crate::theme::installed_fonts(cx).contains(family))
}

fn picker(
    default: SharedString,
    configured: Option<SharedString>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<FontPicker> {
    let choices = std::iter::once(FontChoice {
        value: None,
        family: default,
    })
    .chain(
        crate::theme::installed_fonts(cx)
            .iter()
            .map(|family| FontChoice {
                value: Some(family.clone()),
                family: family.clone(),
            }),
    )
    .collect::<Vec<_>>();
    let selected = installed_choice(configured, cx);
    let selected_ix = choices
        .iter()
        .position(|choice| choice.value == selected)
        .unwrap_or(0);
    cx.new(|cx| {
        SelectState::new(
            SearchableVec::new(choices),
            Some(gpui_kit::component::IndexPath::new(selected_ix)),
            window,
            cx,
        )
        .searchable(true)
    })
}
