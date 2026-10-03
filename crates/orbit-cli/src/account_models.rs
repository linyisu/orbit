//! Provider model discovery and a bounded, credential-scoped metadata cache.
use std::{
    path::Path,
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result, ensure};
use futures_util::{StreamExt, stream};
use orbit_model::{
    client::ApiFormat,
    types::{ReasoningAdapter, ReasoningEffort},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::config::{FileConfig, ModelProfile, ProviderConfig, ReasoningProfile};

pub async fn refresh(config: &mut FileConfig, force: bool) -> Vec<String> {
    let Some(directory) = config.auth_directory.as_ref() else {
        return Vec::new();
    };
    let mut jobs = Vec::new();
    for (name, provider) in &mut config.providers {
        if provider.auto_refresh_models && supported(provider.api_format, &provider.base_url) {
            jobs.push(refresh_connection(name, provider, directory, force));
        }
    }
    let mut diagnostics = stream::iter(jobs)
        .buffer_unordered(4)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    diagnostics.sort();
    diagnostics
}

async fn refresh_connection(
    name: &str,
    provider: &mut ProviderConfig,
    directory: &Path,
    force: bool,
) -> Vec<String> {
    let mut diagnostics = Vec::new();
    if let Err(error) = refresh_provider(name, provider, directory, force, &mut diagnostics).await {
        diagnostics.push(format!("{name}：模型目录更新失败：{error}"));
    }
    diagnostics
}

async fn refresh_provider(
    name: &str,
    provider: &mut ProviderConfig,
    directory: &Path,
    force: bool,
    diagnostics: &mut Vec<String>,
) -> Result<()> {
    let reference = provider
        .credential
        .as_ref()
        .map(|name| orbit_auth::CredentialRef {
            directory: directory.to_path_buf(),
            name: name.clone(),
        });
    let identity = if let Some(reference) = &reference {
        let info = orbit_auth::AuthStore::new(directory.to_path_buf())?
            .list()?
            .into_iter()
            .find(|info| info.name == reference.name)
            .context("账号尚未登录")?;
        let revision = if info.kind == orbit_auth::CredentialKind::ApiKey {
            std::fs::metadata(directory.join(format!("{}.json", reference.name)))
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos().to_string())
        } else {
            None
        };
        json!({ "name": reference.name, "kind": info.kind, "endpoint": info.endpoint,
            "account": info.account, "account_id": info.account_id, "project": info.project_id,
            "revision": revision })
    } else {
        json!({ "key_fingerprint": fingerprint(&provider.api_key) })
    };
    let binding = json!({ "version": 2, "identity": identity,
        "format": provider.api_format, "endpoint": provider.base_url.trim_end_matches('/') });
    let cache_dir = directory.join("model-catalog");
    let path = cache_dir.join(format!("{}.json", fingerprint(name)));
    let cached = read_cache(&path, &binding);
    let fresh = std::fs::metadata(&path)
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|time| SystemTime::now().duration_since(time).ok())
        .is_some_and(|age| age < Duration::from_secs(900));
    let payload = if let Some(cached) = cached.as_ref().filter(|_| !force && fresh) {
        cached.clone()
    } else {
        let discovered = async {
            let payload = if matches!(
                provider.api_format,
                ApiFormat::GoogleCodeAssist | ApiFormat::ChatgptCodex
            ) {
                orbit_model::discovery::discover(
                    reference.as_ref().context("套餐账号需要已保存的认证")?,
                    provider.api_format,
                    &provider.base_url,
                )
                .await?
            } else if let Some(reference) = &reference {
                orbit_model::discovery::discover_saved_api_catalog(
                    reference,
                    &provider.base_url,
                    provider.api_format,
                )
                .await?
            } else {
                orbit_model::discovery::discover_api_catalog(
                    &provider.base_url,
                    provider.api_format,
                    &provider.api_key,
                )
                .await?
            };
            normalize(provider.api_format, &payload)?;
            Ok::<_, anyhow::Error>(payload)
        }
        .await;
        match discovered {
            Ok(payload) => {
                let write = (|| -> Result<()> {
                    let mut directories = std::fs::DirBuilder::new();
                    directories.recursive(true);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::DirBuilderExt;
                        directories.mode(0o700);
                    }
                    directories.create(&cache_dir)?;
                    let mut file = tempfile::NamedTempFile::new_in(&cache_dir)?;
                    use std::io::Write;
                    file.write_all(
                        serde_json::to_string(&json!({ "binding": binding, "payload": payload }))?
                            .as_bytes(),
                    )?;
                    file.as_file().sync_all()?;
                    file.persist(&path)?;
                    Ok(())
                })();
                if let Err(error) = write {
                    diagnostics.push(format!("{name}：模型缓存写入失败：{error}"));
                }
                payload
            }
            Err(error) => {
                diagnostics.push(format!("{name}：模型刷新失败，保留缓存或现有目录：{error}"));
                match cached {
                    Some(payload) => payload,
                    None => return Ok(()),
                }
            }
        }
    };
    let (models, _) = normalize(provider.api_format, &payload)?;
    merge_discovered_models(provider, models);
    Ok(())
}

fn merge_discovered_models(provider: &mut ProviderConfig, models: Vec<ModelProfile>) {
    if models.is_empty() {
        return;
    }
    let configured = provider
        .configured_models
        .get_or_insert_with(|| provider.models.clone());
    let quota_catalog = provider.api_format == ApiFormat::GoogleCodeAssist
        && provider.base_url.trim_end_matches('/') == "https://cloudcode-pa.googleapis.com";
    if !provider.managed_models && !quota_catalog {
        provider
            .models
            .retain(|existing| models.iter().any(|model| model.id == existing.id));
    }
    for mut model in models {
        if let Some(explicit) = configured.iter().find(|entry| entry.id == model.id) {
            model.max_output_tokens = explicit.max_output_tokens.or(model.max_output_tokens);
            model.context_tokens = explicit.context_tokens.or(model.context_tokens);
            model.input_context_tokens =
                explicit.input_context_tokens.or(model.input_context_tokens);
            model.supports_vision = explicit.supports_vision.or(model.supports_vision);
            model.reasoning = explicit.reasoning.clone().or(model.reasoning);
        }
        if let Some(existing) = provider
            .models
            .iter_mut()
            .find(|entry| entry.id == model.id)
        {
            *existing = model;
        } else if !provider.managed_models {
            provider.models.push(model);
        }
    }
}

fn supported(format: ApiFormat, base: &str) -> bool {
    match format {
        ApiFormat::OpenaiChatCompletions
        | ApiFormat::OpenaiResponses
        | ApiFormat::AnthropicMessages => true,
        ApiFormat::GoogleCodeAssist => matches!(
            base.trim_end_matches('/'),
            "https://daily-cloudcode-pa.googleapis.com" | "https://cloudcode-pa.googleapis.com"
        ),
        ApiFormat::ChatgptCodex => {
            base.trim_end_matches('/') == "https://chatgpt.com/backend-api/codex"
        }
    }
}

fn fingerprint(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
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
    if matches!(
        format,
        ApiFormat::OpenaiChatCompletions
            | ApiFormat::OpenaiResponses
            | ApiFormat::AnthropicMessages
    ) {
        let entries = payload["data"]
            .as_array()
            .context("模型目录缺少 data 数组")?;
        for entry in entries {
            let mut model: ModelProfile = serde_json::from_value(entry.clone())?;
            if format == ApiFormat::AnthropicMessages {
                anthropic_capabilities(&mut model, entry)?;
            }
            ensure!(
                !model.id.trim().is_empty()
                    && model.id.len() <= 256
                    && !model.id.chars().any(char::is_control),
                "模型 ID 无效"
            );
            if models
                .iter()
                .any(|existing: &ModelProfile| existing.id == model.id)
            {
                continue;
            }
            ensure!(
                model.context_tokens.is_none_or(|tokens| tokens >= 8192)
                    && model
                        .input_context_tokens
                        .is_none_or(|tokens| tokens >= 8192)
                    && model.max_output_tokens.is_none_or(|tokens| tokens > 0),
                "模型容量无效：{}",
                model.id
            );
            if let Some(reasoning) = &model.reasoning {
                let adapter = match format {
                    ApiFormat::OpenaiResponses => ReasoningAdapter::OpenaiResponses,
                    ApiFormat::AnthropicMessages => ReasoningAdapter::AnthropicMessages,
                    _ => ReasoningAdapter::OpenaiChat,
                };
                ensure!(
                    reasoning.adapter == adapter && reasoning.efforts.contains(&reasoning.default),
                    "模型思考能力无效：{}",
                    model.id
                );
                crate::config::validate_reasoning_extra(&model.id, format, &reasoning.extra)?;
            }
            models.push(model);
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
    } else if let Some(buckets) = payload["buckets"].as_array() {
        // Quota buckets identify models, but omission or zero quota does not retire them.
        // https://github.com/google-gemini/gemini-cli/blob/main/packages/core/src/code_assist/types.ts
        for bucket in buckets {
            if let Some(id) = bucket["modelId"].as_str()
                && !models.iter().any(|model| model.id == id)
            {
                ensure!(
                    !id.trim().is_empty() && id.len() <= 256 && !id.chars().any(char::is_control),
                    "模型 ID 无效"
                );
                models.push(serde_json::from_value(json!({ "id": id }))?);
            }
        }
        ensure!(!models.is_empty(), "配额接口未返回模型 ID，保留现有目录");
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

/// Official capability metadata absent from the provider's ID-only model list.
/// https://api-docs.deepseek.com/guides/vision/ (checked 2026-09-30).
pub(crate) fn default_vision(format: ApiFormat, base: &str, id: &str) -> bool {
    matches!(
        format,
        ApiFormat::OpenaiChatCompletions | ApiFormat::OpenaiResponses
    ) && matches!(
        base.trim_end_matches('/'),
        "https://api.deepseek.com" | "https://api.deepseek.com/v1"
    ) && matches!(
        id,
        "deepseek-flash" | "deepseek-v4-flash" | "deepseek-v4-flash-vision-exp"
    )
}

/// Defaults for official models whose discovery response contains only IDs.
/// https://api-docs.deepseek.com/api/create-chat-completion/ (checked 2026-10-03).
pub(crate) fn default_reasoning(
    format: ApiFormat,
    base: &str,
    id: &str,
) -> Option<ReasoningProfile> {
    if !matches!(
        base.trim_end_matches('/'),
        "https://api.deepseek.com" | "https://api.deepseek.com/v1"
    ) || !matches!(id, "deepseek-flash" | "deepseek-v4-pro")
    {
        return None;
    }
    let adapter = match format {
        ApiFormat::OpenaiChatCompletions => ReasoningAdapter::OpenaiChat,
        ApiFormat::OpenaiResponses => ReasoningAdapter::OpenaiResponses,
        _ => return None,
    };
    Some(ReasoningProfile {
        adapter,
        efforts: vec![
            ReasoningEffort::None,
            ReasoningEffort::Low,
            ReasoningEffort::High,
            ReasoningEffort::Max,
        ],
        default: ReasoningEffort::High,
        extra: toml::Table::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(format: ApiFormat, managed: bool) -> ProviderConfig {
        serde_json::from_value(
            json!({"api_format": format, "base_url": "https://orbit.example/v1",
            "managed_models": managed, "models": [
                {"id": "orbit-live", "context_tokens": 100000, "supports_vision": false},
                {"id": "orbit-retired"}
            ]}),
        )
        .unwrap()
    }

    #[test]
    fn refresh_updates_remote_capabilities_but_preserves_explicit_fields_and_choices() {
        for managed in [false, true] {
            let mut provider = provider(ApiFormat::OpenaiResponses, managed);
            for (effort, output) in [("high", 32000), ("low", 64000)] {
                let (models, _) = normalize(ApiFormat::OpenaiResponses, &json!({"data": [
                    {"id": "orbit-live", "context_tokens": 200000, "supports_vision": true,
                     "max_output_tokens": output,
                     "reasoning": {"adapter": "openai_responses", "efforts": [effort], "default": effort}},
                    {"id": "orbit-new"}
                ]})).unwrap();
                merge_discovered_models(&mut provider, models);
                let live = &provider.models[0];
                assert_eq!(live.context_tokens, Some(100000));
                assert_eq!(live.supports_vision, Some(false));
                assert_eq!(live.max_output_tokens, Some(output));
                assert_eq!(live.reasoning.as_ref().unwrap().default.as_str(), effort);
                assert_eq!(provider.models.len(), 2);
                assert_eq!(
                    provider.models[1].id,
                    if managed {
                        "orbit-retired"
                    } else {
                        "orbit-new"
                    }
                );
            }
        }
    }

    #[test]
    fn bundled_defaults_yield_to_discovered_capacity() {
        let mut config = FileConfig::parse(
            r#"default_provider = "orbit"
[providers.orbit]
api_format = "google_code_assist"
base_url = "https://daily-cloudcode-pa.googleapis.com"
credential = "orbit-fixture"
models = [{id = "orbit-configured"}]
"#,
            Path::new("/tmp/orbit-fixture-auth"),
        )
        .unwrap();
        let provider = config.providers.get_mut("orbit").unwrap();
        let id = provider.models[1].id.clone();
        let model: ModelProfile =
            serde_json::from_value(json!({"id": id, "context_tokens": 4000000,
            "supports_vision": false}))
            .unwrap();
        merge_discovered_models(provider, vec![model]);
        assert_eq!(provider.models.len(), 1);
        assert_eq!(provider.models[0].context_tokens, Some(4000000));
        assert_eq!(provider.models[0].supports_vision, Some(false));
    }

    #[test]
    fn anthropic_discovery_uses_advertised_capacity_vision_and_adaptive_efforts() {
        let (models, _) = normalize(
            ApiFormat::AnthropicMessages,
            &json!({"data": [{
            "id": "orbit-adaptive", "max_input_tokens": 1000000, "max_tokens": 64000,
            "capabilities": {
                "image_input": {"supported": true},
                "thinking": {"types": {"adaptive": {"supported": true}}},
                "effort": {"supported": true, "low": {"supported": true},
                    "medium": {"supported": false}, "high": {"supported": true},
                    "max": {"supported": true}, "future": {"supported": true}}
            }
        }, {"id": "orbit-unknown"}]}),
        )
        .unwrap();
        let profile = &models[0];
        assert_eq!(profile.input_context_tokens, Some(1000000));
        assert_eq!(profile.max_output_tokens, Some(64000));
        assert_eq!(profile.supports_vision, Some(true));
        let reasoning = profile.reasoning.as_ref().unwrap();
        assert_eq!(reasoning.adapter, ReasoningAdapter::AnthropicMessages);
        assert_eq!(reasoning.default, ReasoningEffort::High);
        assert_eq!(
            reasoning.efforts,
            [
                ReasoningEffort::Low,
                ReasoningEffort::High,
                ReasoningEffort::Max
            ]
        );
        assert!(models[1].reasoning.is_none());
        assert!(models[1].supports_vision.is_none());
    }

    #[test]
    fn google_quota_models_do_not_retire_unlisted_or_exhausted_models() {
        let mut provider = provider(ApiFormat::GoogleCodeAssist, false);
        provider.base_url = "https://cloudcode-pa.googleapis.com".into();
        let (models, _) = normalize(
            ApiFormat::GoogleCodeAssist,
            &json!({"buckets": [
                {"modelId": "orbit-live", "remainingFraction": 0},
                {"modelId": "orbit-new", "remainingFraction": 1},
                {"modelId": "orbit-new", "tokenType": "REQUESTS"}
            ]}),
        )
        .unwrap();
        assert_eq!(models.len(), 2);
        assert!(models.iter().all(|model| model.reasoning.is_none()));
        merge_discovered_models(&mut provider, models);
        assert_eq!(provider.models.len(), 3);
        assert_eq!(provider.models[0].id, "orbit-live");
        assert_eq!(provider.models[1].id, "orbit-retired");
        assert_eq!(provider.models[2].id, "orbit-new");
        assert!(supported(provider.api_format, &provider.base_url));
    }

    async fn serve_catalog(listener: tokio::net::TcpListener, payload: Value) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(socket.read_u8().await.unwrap());
        }
        let body = payload.to_string();
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        String::from_utf8(request).unwrap().to_ascii_lowercase()
    }

    #[tokio::test]
    async fn all_api_connections_refresh_with_scoped_caches_and_independent_failures() {
        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("auth");
        let store = orbit_auth::AuthStore::new(directory.clone()).unwrap();
        let mut providers = serde_json::Map::new();
        let mut servers = Vec::new();
        for (name, format, saved, key, managed) in [
            (
                "orbit-chat",
                ApiFormat::OpenaiChatCompletions,
                false,
                "orbit-inline",
                false,
            ),
            (
                "orbit-responses",
                ApiFormat::OpenaiResponses,
                true,
                "orbit-saved",
                true,
            ),
            (
                "orbit-anthropic",
                ApiFormat::AnthropicMessages,
                false,
                "orbit-anthropic-key",
                false,
            ),
            (
                "orbit-local",
                ApiFormat::OpenaiChatCompletions,
                false,
                "",
                false,
            ),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let endpoint = format!("http://{}/orbit/v1", listener.local_addr().unwrap());
            let mut connection = json!({"api_format": format, "base_url": endpoint,
                "managed_models": managed, "models": [{"id": "orbit-live"}]});
            if saved {
                store
                    .set_api_key(name, key.into(), &endpoint)
                    .await
                    .unwrap();
                connection["credential"] = json!(name);
            } else {
                connection["api_key"] = json!(key);
            }
            providers.insert(name.into(), connection);
            servers.push((
                name,
                format,
                key,
                tokio::spawn(serve_catalog(
                    listener,
                    json!({"data": [
                        {"id": "orbit-live", "context_tokens": 200000}, {"id": "orbit-new"}
                    ]}),
                )),
            ));
        }
        providers.insert(
            "orbit-failure".into(),
            json!({"base_url": "http://127.0.0.1:0/v1",
            "models": [{"id": "orbit-fallback"}]}),
        );
        providers.insert(
            "orbit-disabled".into(),
            json!({"base_url": "http://127.0.0.1:0/v1",
            "auto_refresh_models": false, "models": [{"id": "orbit-manual"}]}),
        );
        let value = json!({"default_provider": "orbit-chat", "providers": providers});
        let mut config: FileConfig = serde_json::from_value(value.clone()).unwrap();
        config.auth_directory = Some(directory.clone());
        let diagnostics = refresh(&mut config, false).await;
        assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
        assert!(diagnostics[0].starts_with("orbit-failure："));
        for (name, format, key, task) in servers {
            let request = task.await.unwrap();
            assert!(request.starts_with("get /orbit/v1/models "));
            if format == ApiFormat::AnthropicMessages {
                assert!(request.contains(&format!("x-api-key: {key}")));
                assert!(request.contains("anthropic-version: 2023-06-01"));
            } else if !key.is_empty() {
                assert!(request.contains(&format!("authorization: bearer {key}")));
            } else {
                assert!(!request.contains("authorization:") && !request.contains("x-api-key:"));
            }
            assert_eq!(
                config.providers[name].models[0].context_tokens,
                Some(200000)
            );
            assert_eq!(
                config.providers[name].models.len(),
                if name == "orbit-responses" { 1 } else { 2 }
            );
        }
        let mut reloaded: FileConfig = serde_json::from_value(value.clone()).unwrap();
        reloaded.auth_directory = Some(directory.clone());
        reloaded
            .providers
            .get_mut("orbit-failure")
            .unwrap()
            .auto_refresh_models = false;
        assert!(refresh(&mut reloaded, false).await.is_empty());
        assert_eq!(
            reloaded.providers["orbit-chat"].models[0].context_tokens,
            Some(200000)
        );
        assert_eq!(
            reloaded.providers["orbit-disabled"].models[0].id,
            "orbit-manual"
        );
        let files = std::fs::read_dir(directory.join("model-catalog"))
            .unwrap()
            .collect::<Vec<_>>();
        assert_eq!(files.len(), 4);
        for file in files {
            let text = std::fs::read_to_string(file.unwrap().path()).unwrap();
            assert!(
                !text.contains("orbit-inline")
                    && !text.contains("orbit-saved")
                    && !text.contains("orbit-anthropic-key")
            );
        }
        let mut rotated: FileConfig = serde_json::from_value(value).unwrap();
        rotated.auth_directory = Some(directory.clone());
        rotated
            .providers
            .get_mut("orbit-failure")
            .unwrap()
            .auto_refresh_models = false;
        rotated.providers.get_mut("orbit-chat").unwrap().api_key = "orbit-rotated-inline".into();
        let endpoint = rotated.providers["orbit-responses"].base_url.clone();
        store
            .set_api_key("orbit-responses", "orbit-rotated-saved".into(), &endpoint)
            .await
            .unwrap();
        let diagnostics = refresh(&mut rotated, false).await;
        assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
        assert!(
            rotated.providers["orbit-chat"].models[0]
                .context_tokens
                .is_none()
        );
        assert!(
            rotated.providers["orbit-responses"].models[0]
                .context_tokens
                .is_none()
        );
    }

    #[test]
    fn reasoning_defaults_require_a_verified_model_endpoint_and_protocol() {
        for (format, adapter) in [
            (
                ApiFormat::OpenaiChatCompletions,
                ReasoningAdapter::OpenaiChat,
            ),
            (
                ApiFormat::OpenaiResponses,
                ReasoningAdapter::OpenaiResponses,
            ),
        ] {
            for base in ["https://api.deepseek.com", "https://api.deepseek.com/v1/"] {
                for id in ["deepseek-flash", "deepseek-v4-pro"] {
                    let profile = default_reasoning(format, base, id).unwrap();
                    assert_eq!(profile.adapter, adapter);
                    assert_eq!(profile.default, ReasoningEffort::High);
                    assert_eq!(
                        profile.efforts,
                        vec![
                            ReasoningEffort::None,
                            ReasoningEffort::Low,
                            ReasoningEffort::High,
                            ReasoningEffort::Max
                        ]
                    );
                }
            }
        }
        assert!(
            default_reasoning(
                ApiFormat::OpenaiChatCompletions,
                "https://orbit.example/v1",
                "deepseek-flash"
            )
            .is_none()
        );
        assert!(
            default_reasoning(
                ApiFormat::OpenaiChatCompletions,
                "https://api.deepseek.com/v1",
                "orbit-unreviewed"
            )
            .is_none()
        );
        assert!(
            default_reasoning(
                ApiFormat::AnthropicMessages,
                "https://api.deepseek.com/v1",
                "deepseek-flash"
            )
            .is_none()
        );
    }

    #[test]
    fn antigravity_discovery_removes_retired_models_and_preserves_available_settings() {
        let mut provider: crate::config::ProviderConfig = serde_json::from_value(json!({
            "api_format": "google_code_assist",
            "base_url": "https://daily-cloudcode-pa.googleapis.com",
            "credential": "antigravity",
            "models": [
                { "id": "claude-sonnet-4-5" },
                { "id": "claude-sonnet-4-6", "context_tokens": 123456 }
            ]
        }))
        .unwrap();
        let (models, _) = normalize(
            ApiFormat::GoogleCodeAssist,
            &json!({
                "models": {
                    "claude-sonnet-4-6": { "maxTokens": 250000 },
                    "claude-opus-4-6-thinking": { "maxTokens": 250000 }
                }
            }),
        )
        .unwrap();

        merge_discovered_models(&mut provider, models);

        assert_eq!(provider.models.len(), 2);
        assert_eq!(provider.models[0].id, "claude-sonnet-4-6");
        assert_eq!(provider.models[0].context_tokens, Some(123456));
        assert_eq!(provider.models[1].id, "claude-opus-4-6");
        assert!(provider.models[0].reasoning.is_some());
    }

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
    #[test]
    fn flash_vision_is_scoped_to_verified_official_models_and_endpoints() {
        for format in [ApiFormat::OpenaiChatCompletions, ApiFormat::OpenaiResponses] {
            assert!(default_vision(
                format,
                "https://api.deepseek.com/v1/",
                "deepseek-flash"
            ));
            assert!(default_vision(
                format,
                "https://api.deepseek.com",
                "deepseek-v4-flash"
            ));
            assert!(!default_vision(
                format,
                "https://proxy.example/v1",
                "deepseek-flash"
            ));
            assert!(!default_vision(
                format,
                "https://api.deepseek.com/v1",
                "deepseek-v4-pro"
            ));
        }
    }
}
