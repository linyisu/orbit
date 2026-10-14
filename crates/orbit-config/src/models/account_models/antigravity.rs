//! Antigravity returns model definitions separately from its Agent selectors.
use super::profile;
use crate::config::ModelProfile;
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::collections::HashSet;

pub(super) fn normalize(payload: &Value) -> Result<(Vec<ModelProfile>, Vec<String>)> {
    let mut models: Vec<ModelProfile> = Vec::new();
    let mut unavailable = Vec::new();
    let entries = payload["models"]
        .as_object()
        .context("模型目录缺少 models 对象")?;
    let selectable = agent_ids(payload)?;
    let bundled = orbit_model::account_catalog::antigravity_models();
    for (wire, entry) in entries {
        if entry["isInternal"] == true
            || entry["disabled"] == true
            || matches!(wire.as_str(), "chat_20706" | "chat_23310")
            || payload["deprecatedModelIds"]
                .get(wire)
                .is_some()
            || selectable
                .as_ref()
                .is_some_and(|ids| !ids.contains(wire.as_str()))
        {
            continue;
        }
        if payload["imageGenerationModelIds"]
            .as_array()
            .is_some_and(|ids| ids.contains(&json!(wire)))
            || wire.contains("-image")
            || wire.starts_with("tab_")
        {
            unavailable.push(wire.clone());
            continue;
        }
        // OMP taxonomy/_collapse.kdl: tiered selectors are aliases;
        // gemini-3.1-pro-high was retired in favor of gemini-pro-agent.
        let alias = if wire == "gemini-3.1-pro-high" {
            "gemini-3.1-pro"
        } else {
            wire.strip_suffix("-tiered")
                .unwrap_or(wire)
        };
        let logical = bundled.iter().find(|(id, spec)| {
            *id == alias
                || spec.request_model_id.as_ref() == Some(wire)
                || spec
                    .thinking
                    .as_ref()
                    .and_then(|t| t["effortRouting"].as_object())
                    .is_some_and(|routes| routes.values().any(|v| v == wire))
        });
        let id = logical.map_or(wire.as_str(), |(id, _)| id.as_str());
        if models
            .iter()
            .any(|model| model.id == id)
        {
            continue;
        }
        let reasoning = logical
            .and_then(|(_, spec)| spec.thinking.as_ref())
            .map(|thinking| {
                json!({
                    "adapter": "google",
                    "efforts": thinking["efforts"],
                    "default": "high"
                })
            });
        models.push(profile(
            id,
            entry["maxTokens"]
                .as_u64()
                .unwrap_or(200_000) as usize,
            entry["supportsImages"] == true,
            reasoning,
        )?);
    }
    Ok((models, unavailable))
}

fn agent_ids(payload: &Value) -> Result<Option<HashSet<&str>>> {
    let Some(sorts) = payload.get("agentModelSorts") else {
        // Older discovery responses contain only model definitions.
        return Ok(None);
    };
    let mut ids = HashSet::new();
    for sort in sorts
        .as_array()
        .context("agentModelSorts 必须是数组")?
    {
        for group in sort["groups"]
            .as_array()
            .context("模型分组缺少 groups 数组")?
        {
            for id in group["modelIds"]
                .as_array()
                .context("模型分组缺少 modelIds 数组")?
            {
                ids.insert(
                    id.as_str()
                        .context("modelIds 必须包含模型 ID 字符串")?,
                );
            }
        }
    }
    // An explicitly empty selector is authoritative; do not restore the
    // definition table or bootstrap models after all Agent access is removed.
    Ok(Some(ids))
}
