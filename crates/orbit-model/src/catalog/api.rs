//! Public API model-list decoding and advertised capabilities.
use super::{ModelProfile, ReasoningProfile};
use crate::{
    client::ApiFormat,
    types::{ReasoningAdapter, ReasoningEffort},
};
use anyhow::{Context, Result, ensure};
use serde_json::Value;

pub(super) fn parse(format: ApiFormat, payload: &Value) -> Result<Vec<ModelProfile>> {
    let mut models = Vec::new();
    let entries = payload["data"]
        .as_array()
        .context("模型目录缺少 data 数组")?;
    for entry in entries {
        let mut model: ModelProfile = serde_json::from_value(entry.clone())?;
        advertised_capabilities(&mut model, entry, format)?;
        if format == ApiFormat::AnthropicMessages {
            anthropic_capabilities(&mut model, entry)?;
        }
        ensure!(super::valid_model_id(&model.id), "模型 ID 无效");
        if models
            .iter()
            .any(|existing: &ModelProfile| existing.id == model.id)
        {
            continue;
        }

        models.push(model);
    }

    Ok(models)
}

// Decode advertised metadata without inferring capabilities from a model name.
fn advertised_capabilities(
    model: &mut ModelProfile,
    entry: &Value,
    format: ApiFormat,
) -> Result<()> {
    if model.context_tokens.is_none() {
        model.context_tokens = entry["context_window"]
            .as_u64()
            .map(usize::try_from)
            .transpose()?;
    }
    if model.supports_vision.is_none()
        && let Some(modalities) = entry["input_modalities"].as_array()
    {
        model.supports_vision = Some(
            modalities
                .iter()
                .any(|value| value == "image"),
        );
    }
    if model.reasoning.is_none()
        && let Some(levels) = entry["effort"]["supported_levels"].as_array()
    {
        let efforts = levels
            .iter()
            .filter_map(Value::as_str)
            .filter_map(|level| level.parse::<ReasoningEffort>().ok())
            .collect::<Vec<_>>();
        let default = entry["effort"]["default_level"]
            .as_str()
            .and_then(|level| level.parse::<ReasoningEffort>().ok())
            .filter(|level| efforts.contains(level));
        if let Some(default) = default.or_else(|| efforts.first().copied()) {
            model.reasoning = Some(ReasoningProfile {
                adapter: format.reasoning_adapter(),
                efforts,
                default,
                extra: toml::Table::new(),
            });
        }
    }
    Ok(())
}

// https://platform.claude.com/docs/en/api/models/list
fn anthropic_capabilities(model: &mut ModelProfile, entry: &Value) -> Result<()> {
    if model.input_context_tokens.is_none() {
        model.input_context_tokens = entry["max_input_tokens"]
            .as_u64()
            .map(usize::try_from)
            .transpose()?;
    }

    if model.max_output_tokens.is_none() {
        model.max_output_tokens = entry["max_tokens"]
            .as_u64()
            .map(u32::try_from)
            .transpose()?;
    }

    let capabilities = &entry["capabilities"];
    if model.supports_vision.is_none() {
        model.supports_vision = capabilities["image_input"]["supported"].as_bool();
    }
    if model.reasoning.is_none()
        && capabilities["effort"]["supported"] == true
        && capabilities["thinking"]["types"]["adaptive"]["supported"] == true
    {
        let efforts = [
            ReasoningEffort::Low,
            ReasoningEffort::Medium,
            ReasoningEffort::High,
            ReasoningEffort::Xhigh,
            ReasoningEffort::Max,
        ]
        .into_iter()
        .filter(|effort| capabilities["effort"][effort.as_str()]["supported"] == true)
        .collect::<Vec<_>>();

        if let Some(default) = efforts
            .iter()
            .find(|&&effort| effort == ReasoningEffort::High)
            .or_else(|| efforts.last())
            .copied()
        {
            model.reasoning = Some(ReasoningProfile {
                adapter: ReasoningAdapter::AnthropicMessages,
                efforts,
                default,
                extra: toml::Table::new(),
            });
        }
    }

    Ok(())
}
