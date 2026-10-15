//! Antigravity returns model definitions separately from its Agent selectors.
use super::ModelProfile;
use super::profile;
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::collections::HashSet;

pub(super) fn normalize(payload: &Value) -> Result<(Vec<ModelProfile>, Vec<String>)> {
    let mut models: Vec<ModelProfile> = Vec::new();
    let mut unavailable = Vec::new();
    let entries = payload["models"]
        .as_object()
        .context("模型目录缺少 models 对象")?;
    let ids = agent_ids(payload)?.unwrap_or_else(|| {
        entries
            .keys()
            .map(String::as_str)
            .collect()
    });
    let bundled = crate::account_catalog::antigravity_models();
    for wire in ids {
        let entry = entries
            .get(wire)
            .unwrap_or(&Value::Null);
        if entry["isInternal"] == true
            || entry["disabled"] == true
            || payload["deprecatedModelIds"]
                .get(wire)
                .is_some()
        {
            continue;
        }
        if payload["imageGenerationModelIds"]
            .as_array()
            .is_some_and(|ids| ids.contains(&json!(wire)))
        {
            unavailable.push(wire.to_owned());
            continue;
        }
        let logical = bundled.get(wire);
        let id = wire;
        if models
            .iter()
            .any(|model| model.id == id)
        {
            continue;
        }
        let reasoning = logical
            .and_then(|spec| spec.thinking.as_ref())
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
                .map(usize::try_from)
                .transpose()?
                .or_else(|| logical.map(|model| model.context_window)),
            entry["supportsImages"].as_bool(),
            reasoning,
        )?);
    }
    Ok((models, unavailable))
}

fn agent_ids(payload: &Value) -> Result<Option<Vec<&str>>> {
    let Some(sorts) = payload.get("agentModelSorts") else {
        // Older discovery responses contain only model definitions.
        return Ok(None);
    };
    let mut ids = Vec::new();
    let mut seen = HashSet::new();
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
                let id = id
                    .as_str()
                    .context("modelIds 必须包含模型 ID 字符串")?;
                if seen.insert(id) {
                    ids.push(id);
                }
            }
        }
    }
    // An explicitly empty selector is authoritative; do not restore the
    // definition table or bootstrap models after all Agent access is removed.
    Ok(Some(ids))
}
