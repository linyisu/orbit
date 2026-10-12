//! Model limits are connection drafts until the containing form is saved.
use anyhow::{Result, ensure};
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
use orbit_config::config::{MIN_CONTEXT_TOKENS, ModelProfile};
use std::collections::HashSet;

pub(super) fn profiles(ids: &str, existing: &[ModelProfile]) -> Result<Vec<ModelProfile>> {
    let mut seen = HashSet::new();
    ids.split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(|id| {
            ensure!(
                id.len() <= 256 && !id.chars().any(char::is_control),
                "模型 ID 无效"
            );
            ensure!(seen.insert(id), "模型 ID“{id}”重复");
            Ok(existing
                .iter()
                .find(|profile| profile.id == id)
                .cloned()
                .unwrap_or_else(|| ModelProfile::new(id)))
        })
        .collect()
}

fn number<T: std::str::FromStr>(value: &str, label: &str) -> Result<Option<T>> {
    if value.trim().is_empty() {
        return Ok(None);
    }
    value
        .trim()
        .parse()
        .map(Some)
        .map_err(|_| anyhow::anyhow!("{label}应为正整数，或留空使用默认值。"))
}

fn limits(profile: &ModelProfile, values: [&str; 3]) -> Result<ModelProfile> {
    let mut updated = profile.clone();
    updated.context_tokens = number(values[0], "上下文容量")?;
    updated.input_context_tokens = number(values[1], "独立输入窗口")?;
    updated.max_output_tokens = number(values[2], "最大输出 Token")?;
    ensure!(
        updated
            .context_tokens
            .is_none_or(|tokens| tokens >= MIN_CONTEXT_TOKENS),
        "上下文容量不能小于 {MIN_CONTEXT_TOKENS} Token。"
    );
    ensure!(
        updated
            .input_context_tokens
            .is_none_or(|tokens| tokens >= MIN_CONTEXT_TOKENS),
        "独立输入窗口不能小于 {MIN_CONTEXT_TOKENS} Token。"
    );
    ensure!(
        updated.max_output_tokens != Some(0),
        "最大输出 Token 必须大于 0。"
    );
    if updated.input_context_tokens.is_none()
        && let (Some(context), Some(output)) = (updated.context_tokens, updated.max_output_tokens)
    {
        ensure!(
            context.saturating_sub(output as usize) >= MIN_CONTEXT_TOKENS,
            "预留输出后，可用输入预算不能小于 {MIN_CONTEXT_TOKENS} Token。"
        );
    }
    Ok(updated)
}

struct ModelFields {
    profile: ModelProfile,
    limits: [Entity<InputState>; 3],
}

pub(super) struct ModelParameters {
    models: Vec<ModelFields>,
    selected: usize,
    error: Option<String>,
}
impl ModelParameters {
    pub(super) fn new(
        profiles: Vec<ModelProfile>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let models = profiles
            .into_iter()
            .map(|profile| {
                let values = [
                    profile.context_tokens.map(|value| value.to_string()),
                    profile.input_context_tokens.map(|value| value.to_string()),
                    profile.max_output_tokens.map(|value| value.to_string()),
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
                limits(&model.profile, values.each_ref().map(String::as_str))
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
                view.child(div().text_sm().text_color(cx.theme().danger).child(error))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[::core::prelude::v1::test]
    fn editing_limits_preserves_other_model_settings_and_blank_restores_defaults() {
        let mut profile = ModelProfile::new("model");
        profile.supports_vision = Some(true);
        profile.reasoning = Some(orbit_config::config::ReasoningProfile {
            adapter: orbit_model::types::ReasoningAdapter::OpenaiResponses,
            efforts: vec![orbit_model::types::ReasoningEffort::High],
            default: orbit_model::types::ReasoningEffort::High,
            extra: Default::default(),
        });
        profile.context_tokens = Some(65536);
        profile.input_context_tokens = Some(32768);
        profile.max_output_tokens = Some(4096);
        let updated = limits(&profile, ["131072", "", "8192"]).unwrap();
        assert_eq!(updated.context_tokens, Some(131072));
        assert_eq!(updated.input_context_tokens, None);
        assert_eq!(updated.max_output_tokens, Some(8192));
        assert_eq!(updated.supports_vision, Some(true));
        assert_eq!(
            serde_json::to_value(&updated.reasoning).unwrap(),
            serde_json::to_value(&profile.reasoning).unwrap()
        );
        assert_eq!(updated.id, profile.id);
        let defaults = limits(&updated, ["", "", ""]).unwrap();
        assert!(defaults.context_tokens.is_none());
        assert!(defaults.input_context_tokens.is_none());
        assert!(defaults.max_output_tokens.is_none());
        assert_eq!(defaults.supports_vision, Some(true));
        assert_eq!(
            serde_json::to_value(&defaults.reasoning).unwrap(),
            serde_json::to_value(&profile.reasoning).unwrap()
        );
    }

    #[::core::prelude::v1::test]
    fn invalid_limits_and_duplicate_models_are_rejected_without_replacing_profiles() {
        let profile = ModelProfile::new("known");
        for values in [
            ["-1", "", ""],
            ["8000", "", ""],
            ["", "8191", ""],
            ["", "", "0"],
            ["", "", "4294967296"],
            ["16384", "", "16384"],
        ] {
            assert!(limits(&profile, values).is_err(), "{values:?}");
        }
        let mut configured = profile.clone();
        configured.max_output_tokens = Some(8192);
        let profiles = profiles(" new, known ", &[configured]).unwrap();
        assert_eq!(profiles[0].id, "new");
        assert_eq!(profiles[1].max_output_tokens, Some(8192));
        assert!(super::profiles("known, known", &profiles).is_err());
        assert!(profile.max_output_tokens.is_none());
    }
}
