//! Account model discovery and a bounded, credential-scoped metadata cache.
use std::{
    path::Path,
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result, ensure};
use orbit_model::{client::ApiFormat, types::ReasoningEffort};
use serde_json::{Value, json};

use crate::config::{FileConfig, ModelProfile};

pub async fn refresh(config: &mut FileConfig, force: bool) -> Vec<String> {
    let Some(directory) = config.auth_directory.clone() else {
        return Vec::new();
    };
    let mut diagnostics = Vec::new();
    for (name, provider) in &mut config.providers {
        if provider.managed_models {
            continue;
        }
        let Some(credential) = provider.credential.as_ref() else {
            continue;
        };
        if !supported(provider.api_format, &provider.base_url) {
            continue;
        }
        let reference = orbit_auth::CredentialRef {
            directory: directory.clone(),
            name: credential.clone(),
        };
        let result = async {
            let info = orbit_auth::AuthStore::new(directory.clone())?.list()?.into_iter()
                .find(|info| info.name == *credential).context("账号尚未登录")?;
            let binding = json!({ "name": credential, "account": info.account,
                "account_id": info.account_id, "project": info.project_id, "endpoint": provider.base_url });
            let cache_dir = directory.join("model-catalog");
            let filename = credential.bytes().map(|b| format!("{b:02x}")).collect::<String>();
            let path = cache_dir.join(format!("{filename}.json"));
            let cached = read_cache(&path, &binding);
            let fresh = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok())
                .and_then(|time| SystemTime::now().duration_since(time).ok())
                .is_some_and(|age| age < Duration::from_secs(900));
            let payload = if let Some(cached) = cached.as_ref().filter(|_| !force && fresh) {
                cached.clone()
            } else {
                match orbit_model::discovery::discover(&reference, provider.api_format, &provider.base_url).await {
                    Ok(payload) => {
                        normalize(provider.api_format, &payload)?;
                        std::fs::create_dir_all(&cache_dir)?;
                        let mut file = tempfile::NamedTempFile::new_in(&cache_dir)?;
                        use std::io::Write;
                        file.write_all(serde_json::to_string(&json!({ "binding": binding, "payload": payload }))?.as_bytes())?;
                        file.as_file().sync_all()?;
                        file.persist(&path)?;
                        payload
                    }
                    Err(error) => {
                        diagnostics.push(format!("{name}：模型刷新失败，保留缓存或现有目录：{error}"));
                        match cached { Some(payload) => payload, None => return Ok::<_, anyhow::Error>(()), }
                    }
                }
            };
            let (models, _) = normalize(provider.api_format, &payload)?;
            for model in models {
                if let Some(existing) = provider.models.iter_mut().find(|entry| entry.id == model.id) {
                    if existing.context_tokens.is_none() { existing.context_tokens = model.context_tokens; }
                    if existing.input_context_tokens.is_none() { existing.input_context_tokens = model.input_context_tokens; }
                    if existing.supports_vision.is_none() { existing.supports_vision = model.supports_vision; }
                    if existing.reasoning.is_none() { existing.reasoning = model.reasoning; }
                } else if !provider.managed_models { provider.models.push(model); }
            }
            Ok(())
        }.await;
        if let Err(error) = result {
            diagnostics.push(format!("{name}：模型目录更新失败：{error}"));
        }
    }
    diagnostics
}

fn supported(format: ApiFormat, base: &str) -> bool {
    matches!(
        (format, base.trim_end_matches('/')),
        (
            ApiFormat::GoogleCodeAssist,
            "https://daily-cloudcode-pa.googleapis.com"
        ) | (
            ApiFormat::ChatgptCodex,
            "https://chatgpt.com/backend-api/codex"
        ) | (
            ApiFormat::OpenaiChatCompletions,
            "https://api.deepseek.com/v1"
        ) | (
            ApiFormat::OpenaiChatCompletions,
            "https://open.bigmodel.cn/api/coding/paas/v4"
        )
    )
}

fn read_cache(path: &Path, binding: &Value) -> Option<Value> {
    if std::fs::metadata(path).ok()?.len() > 4 * 1024 * 1024 {
        return None;
    }
    let data: Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    (data.get("binding") == Some(binding)).then(|| data["payload"].clone())
}

pub(crate) fn normalize(
    format: ApiFormat,
    payload: &Value,
) -> Result<(Vec<ModelProfile>, Vec<String>)> {
    let mut models = Vec::new();
    let mut unavailable = Vec::new();
    if format == ApiFormat::OpenaiChatCompletions {
        let entries = payload["data"]
            .as_array()
            .context("模型目录缺少 data 数组")?;
        for entry in entries {
            let id = entry["id"].as_str().context("模型目录缺少 id")?;
            ensure!(
                !id.is_empty() && !id.chars().any(char::is_control),
                "模型 ID 无效"
            );
            models.push(serde_json::from_value(json!({ "id": id }))?);
        }
        ensure!(!models.is_empty(), "账号未返回可用模型");
    } else if format == ApiFormat::ChatgptCodex {
        let entries = payload
            .get("models")
            .or_else(|| payload.get("data"))
            .and_then(Value::as_array)
            .context("模型目录缺少 models/data 数组")?;
        for entry in entries {
            let Some(id) = entry
                .get("slug")
                .or_else(|| entry.get("id"))
                .and_then(Value::as_str)
            else {
                continue;
            };
            if matches!(entry["visibility"].as_str(), Some("hide" | "hidden")) {
                continue;
            }
            // OMP also keeps direct tools when its eval transport is unavailable
            // (resolveCodeMode). Do not hide catalog rows based on that preference.
            let efforts = entry["supported_reasoning_levels"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|level| level.get("effort").unwrap_or(level).as_str())
                .filter(|level| serde_json::from_value::<ReasoningEffort>(json!(level)).is_ok())
                .collect::<Vec<_>>();
            let default = entry["default_reasoning_level"]
                .as_str()
                .filter(|level| efforts.contains(level))
                .or_else(|| efforts.last().copied());
            models.push(profile(id, entry["context_window"].as_u64().unwrap_or(272000) as usize,
                entry["input_modalities"].as_array().is_some_and(|m| m.iter().any(|v| v == "image")),
                default.map(|default| json!({ "adapter": "openai_responses", "efforts": efforts, "default": default })))?);
        }
    } else {
        let entries = payload["models"]
            .as_object()
            .context("模型目录缺少 models 对象")?;
        let bundled = orbit_model::account_catalog::antigravity_models();
        for (wire, entry) in entries {
            if entry["isInternal"] == true || matches!(wire.as_str(), "chat_20706" | "chat_23310") {
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
                wire.strip_suffix("-tiered").unwrap_or(wire)
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
            if models.iter().any(|model| model.id == id) {
                continue;
            }
            let reasoning = logical.and_then(|(_, spec)| spec.thinking.as_ref()).map(|thinking|
                json!({ "adapter": "google", "efforts": thinking["efforts"], "default": "high" }));
            models.push(profile(
                id,
                entry["maxTokens"].as_u64().unwrap_or(200000) as usize,
                entry["supportsImages"] == true,
                reasoning,
            )?);
        }
    }
    Ok((models, unavailable))
}

fn profile(
    id: &str,
    context: usize,
    vision: bool,
    reasoning: Option<Value>,
) -> Result<ModelProfile> {
    ensure!(
        !id.is_empty() && id.len() <= 256 && !id.chars().any(char::is_control),
        "无效模型 ID"
    );
    ensure!(context >= 8192, "模型上下文容量过小：{id}");
    Ok(serde_json::from_value(
        json!({ "id": id, "context_tokens": context,
        "supports_vision": vision, "reasoning": reasoning }),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_discovery_preserves_new_models_and_advertised_efforts() {
        let payload = json!({ "models": [
            { "slug": "gpt-new", "tool_mode": "code_mode_only", "context_window": 1000000,
              "input_modalities": ["text", "image"], "default_reasoning_level": "high",
              "supported_reasoning_levels": [{ "effort": "low" }, { "effort": "high" }, { "effort": "unknown" }] },
            { "slug": "internal", "visibility": "hidden" }
        ] });

        let (models, unavailable) = normalize(ApiFormat::ChatgptCodex, &payload).unwrap();

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gpt-new");
        assert_eq!(models[0].context_tokens, Some(1000000));
        assert_eq!(models[0].supports_vision, Some(true));
        assert_eq!(
            models[0].reasoning.as_ref().unwrap().efforts,
            [ReasoningEffort::Low, ReasoningEffort::High]
        );
        assert!(unavailable.is_empty());
    }

    #[test]
    fn antigravity_keeps_unknown_models_and_collapses_known_routes() {
        let payload = json!({ "models": {
            "gemini-3.8-flash-low": { "maxTokens": 1048576, "supportsImages": true },
            "gemini-3.8-flash-high": { "maxTokens": 1048576, "supportsImages": true },
            "future-model": { "maxTokens": 128000 },
            "private-model": { "isInternal": true }
        } });

        let (models, _) = normalize(ApiFormat::GoogleCodeAssist, &payload).unwrap();

        assert_eq!(models.len(), 2);
        assert!(models.iter().any(|model| model.id == "future-model"));
        assert!(models.iter().any(|model| model.id == "gemini-3.8-flash"));
        assert!(normalize(ApiFormat::GoogleCodeAssist, &json!({})).is_err());
        assert!(
            normalize(ApiFormat::ChatgptCodex, &json!({ "models": [] }))
                .unwrap()
                .0
                .is_empty()
        );
    }

    #[test]
    fn deepseek_refresh_uses_advertised_models_without_inventing_capabilities() {
        let payload = json!({ "data": [{ "id": "future-model" }] });

        let (models, unavailable) = normalize(ApiFormat::OpenaiChatCompletions, &payload).unwrap();

        assert_eq!(models[0].id, "future-model");
        assert!(models[0].context_tokens.is_none());
        assert!(models[0].reasoning.is_none());
        assert!(unavailable.is_empty());
        assert!(supported(
            ApiFormat::OpenaiChatCompletions,
            orbit_auth::DEEPSEEK_BASE
        ));
    }

    #[test]
    fn cache_never_crosses_accounts() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("catalog.json");
        let binding = json!({ "account_id": "first" });
        std::fs::write(
            &path,
            serde_json::to_vec(&json!({ "binding": binding, "payload": { "models": [] } }))
                .unwrap(),
        )
        .unwrap();

        assert!(read_cache(&path, &binding).is_some());
        assert!(read_cache(&path, &json!({ "account_id": "second" })).is_none());
    }
}
