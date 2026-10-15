//! Account-scoped subscription model catalogs preserve server IDs and ordering.
use super::{MIN_CONTEXT_TOKENS, ModelProfile, ReasoningProfile};
use crate::types::{ReasoningAdapter, ReasoningEffort};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::collections::HashSet;

pub(super) fn parse(payload: &Value) -> Result<Vec<ModelProfile>> {
    let entries = payload
        .get("models")
        .or_else(|| payload.get("data"))
        .and_then(Value::as_array)
        .context("模型目录缺少 models/data 数组")?;
    let mut models = Vec::new();
    let mut seen = HashSet::new();
    for entry in entries {
        if entry["visibility"]
            .as_str()
            .is_some_and(|visibility| visibility != "list")
        {
            continue;
        }
        let id = entry
            .get("slug")
            .or_else(|| entry.get("id"))
            .and_then(Value::as_str)
            .context("模型目录缺少 slug/id")?;
        ensure!(super::valid_model_id(id), "模型 ID 无效");
        if !seen.insert(id) {
            continue;
        }
        let efforts = entry["supported_reasoning_levels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|level| {
                serde_json::from_value::<ReasoningEffort>(
                    level
                        .get("effort")
                        .unwrap_or(level)
                        .clone(),
                )
                .ok()
            })
            .collect::<Vec<_>>();
        let default = entry["default_reasoning_level"]
            .as_str()
            .and_then(|value| {
                efforts
                    .iter()
                    .find(|effort| effort.as_str() == value)
                    .copied()
            })
            .or_else(|| efforts.last().copied());
        let context_tokens = entry["context_window"]
            .as_u64()
            .map(usize::try_from)
            .transpose()?;
        ensure!(
            context_tokens.is_none_or(|tokens| tokens >= MIN_CONTEXT_TOKENS),
            "模型上下文容量过小：{id}"
        );
        let mut model = ModelProfile::new(id);
        model.context_tokens = context_tokens;
        model.supports_vision = entry["input_modalities"]
            .as_array()
            .map(|modalities| {
                modalities
                    .iter()
                    .any(|value| value == "image")
            });
        model.reasoning = default.map(|default| ReasoningProfile {
            adapter: ReasoningAdapter::OpenaiResponses,
            efforts,
            default,
            extra: toml::Table::new(),
        });
        models.push(model);
    }
    Ok(models)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn directory_uses_all_visible_server_slugs_without_a_client_model_allowlist() {
        let entries = (0..19)
            .map(|index| {
                json!({
                    "slug": format!("generated-family-{index}"), "visibility": "list",
                    "context_window": 96_000 + index,
                    "input_modalities": ["text", "image"],
                    "supported_reasoning_levels": [{ "effort": "low" }, { "effort": "high" }],
                    "default_reasoning_level": "low",
                })
            })
            .collect::<Vec<_>>();
        let models = parse(&json!({ "models": entries })).unwrap();
        assert_eq!(models.len(), 19);
        for (index, model) in models.iter().enumerate() {
            assert_eq!(model.id, format!("generated-family-{index}"));
            assert_eq!(
                model
                    .reasoning
                    .as_ref()
                    .unwrap()
                    .default,
                ReasoningEffort::Low
            );
        }
        assert!(
            parse(&json!({ "models": [] }))
                .unwrap()
                .is_empty()
        );
        assert!(
            parse(&json!({ "models": [{ "slug": "restricted", "visibility": "hide" }] }))
                .unwrap()
                .is_empty()
        );
        assert!(parse(&json!({ "models": [{ "visibility": "list" }] })).is_err());
    }
}
