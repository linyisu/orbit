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

use crate::config::{
    FileConfig, MIN_CONTEXT_TOKENS, ModelProfile, ProviderConfig, ReasoningProfile,
};

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
                model
                    .context_tokens
                    .is_none_or(|tokens| tokens >= MIN_CONTEXT_TOKENS)
                    && model
                        .input_context_tokens
                        .is_none_or(|tokens| tokens >= MIN_CONTEXT_TOKENS)
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
    ensure!(context >= MIN_CONTEXT_TOKENS, "模型上下文容量过小：{id}");
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
mod tests;
