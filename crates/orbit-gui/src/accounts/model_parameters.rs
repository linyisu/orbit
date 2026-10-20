//! Model limits are connection drafts until the containing form is saved.
use anyhow::Result;
use gpui_kit::component::{
    ActiveTheme,
    button::Button,
    h_flex,
    input::{Input, InputState},
    menu::{DropdownMenu, PopupMenuItem},
    v_flex,
};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use orbit_config::config::{ModelProfile, connections::models::limits};

struct ModelFields {
    profile: ModelProfile,
    limits: [Entity<InputState>; 3],
}

pub(super) struct ModelParameters {
    models: Vec<ModelFields>,
    defaults: (usize, u32),
    selected: usize,
    error: Option<String>,
}

impl ModelParameters {
    pub(super) fn new(
        profiles: Vec<ModelProfile>,
        defaults: (usize, u32),
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let models = profiles
            .into_iter()
            .map(|profile| {
                let values = [
                    profile
                        .context_tokens
                        .map(|value| value.to_string()),
                    profile
                        .input_context_tokens
                        .map(|value| value.to_string()),
                    profile
                        .max_output_tokens
                        .map(|value| value.to_string()),
                ];
                ModelFields {
                    profile,
                    limits: values.map(|value| {
                        cx.new(|cx| {
                            InputState::new(window, cx)
                                .placeholder("使用默认值")
                                .default_value(value.unwrap_or_default())
                        })
                    }),
                }
            })
            .collect();
        Self {
            models,
            defaults,
            selected: 0,
            error: None,
        }
    }

    pub(super) fn values(&mut self, cx: &mut Context<Self>) -> Option<Vec<ModelProfile>> {
        let result: Result<Vec<_>> = self
            .models
            .iter()
            .map(|model| {
                let values = model
                    .limits
                    .each_ref()
                    .map(|field| field.read(cx).value().to_string());
                limits(
                    &model.profile,
                    values.each_ref().map(String::as_str),
                    Some(self.defaults),
                )
                .map_err(|error| anyhow::anyhow!("{}：{error}", model.profile.id))
            })
            .collect();
        match result {
            Ok(values) => {
                self.error = None;
                Some(values)
            }
            Err(error) => {
                self.error = Some(error.to_string());
                cx.notify();
                None
            }
        }
    }

    pub(super) fn set_error(&mut self, error: &str, cx: &mut Context<Self>) {
        self.error = Some(error.into());
        cx.notify();
    }

    pub(super) fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.models[self.selected].limits[0].update(cx, |input, cx| input.focus(window, cx));
    }
}

impl Render for ModelParameters {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let owner = cx.weak_entity();
        let model = &self.models[self.selected];
        let narrow = window.viewport_size().width < window.rem_size() * 40.;
        v_flex()
            .w_full()
            .gap_4()
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("参数先应用到连接表单；保存连接后生效。留空可恢复模型或连接的默认值。"),
            )
            .child(
                v_flex().gap_1().child("模型").child(
                    Button::new("parameter-model")
                        .w_full()
                        .label(model.profile.id.clone())
                        .dropdown_menu(move |mut menu, _, cx| {
                            if let Some(owner) = owner.upgrade() {
                                let selected = owner.read(cx).selected;
                                for (ix, model) in owner.read(cx).models.iter().enumerate() {
                                    let owner = owner.downgrade();
                                    menu = menu.item(
                                        PopupMenuItem::new(model.profile.id.clone())
                                            .checked(ix == selected)
                                            .on_click(move |_, _, cx| {
                                                let _ = owner.update(cx, |view, cx| {
                                                    view.selected = ix;
                                                    view.error = None;
                                                    cx.notify();
                                                });
                                            }),
                                    );
                                }
                            }
                            menu
                        }),
                ),
            )
            .children(
                [
                    ("上下文容量（Token）", "输入与输出共享的总预算"),
                    (
                        "独立输入窗口（Token）",
                        "仅用于服务端将输入和输出分开计量的模型；留空时从上下文容量预留输出",
                    ),
                    (
                        "最大输出 Token",
                        "限制单次生成长度；实际输出也受模型自身限制",
                    ),
                ]
                .into_iter()
                .zip(&model.limits)
                .map(|((label, help), input)| {
                    v_flex()
                        .gap_1()
                        .child(
                            h_flex()
                                .gap_3()
                                .when(narrow, |row| row.flex_col().items_stretch())
                                .child(div().flex_1().child(label))
                                .child(
                                    div()
                                        .when(narrow, |input| input.w_full())
                                        .when(!narrow, |input| input.w_40())
                                        .child(Input::new(input).aria_label(label)),
                                ),
                        )
                        .child(
                            div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(help),
                        )
                }),
            )
            .when_some(self.error.clone(), |view, error| {
                view.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(
                            orbit_client::directories::display_diagnostic(error.as_ref())
                                .into_owned(),
                        ),
                )
            })
    }
}
