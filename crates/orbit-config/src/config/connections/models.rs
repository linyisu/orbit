//! Model draft validation and discovery shared by every client.
use super::edit::{ConnectionDraft, KeyChange};
use crate::config::{MIN_CONTEXT_TOKENS, ModelProfile};
use anyhow::{Result, ensure};
use std::collections::HashSet;

const MAX_MODEL_ID_BYTES: usize = 256;

/// Validate an opaque provider model identifier without interpreting delimiters.
pub fn validate_id(id: &str) -> Result<()> {
    ensure!(
        !id.trim().is_empty()
            && id.len() <= MAX_MODEL_ID_BYTES
            && !id.chars().any(char::is_control),
        "模型 ID 无效"
    );
    Ok(())
}

/// Validate one ordered enabled-model list before discovery results or edits are saved.
pub fn validate_profiles(profiles: &[ModelProfile]) -> Result<()> {
    let mut seen = HashSet::new();
    for profile in profiles {
        validate_id(&profile.id)?;
        ensure!(seen.insert(&profile.id), "模型 ID“{}”重复", profile.id);
    }
    Ok(())
}

/// Retain protocol-specific settings only when the reasoning adapter is unchanged.
pub fn reasoning(
    current: Option<&crate::config::ReasoningProfile>,
    format: orbit_model::client::ApiFormat,
    efforts: Vec<orbit_model::types::ReasoningEffort>,
    default: orbit_model::types::ReasoningEffort,
) -> Result<crate::config::ReasoningProfile> {
    ensure!(efforts.contains(&default), "默认强度必须在已启用的强度中");
    let adapter = crate::config::reasoning_adapter(format);
    Ok(crate::config::ReasoningProfile {
        adapter,
        efforts,
        default,
        extra: current
            .filter(|profile| profile.adapter == adapter)
            .map_or_else(Default::default, |profile| profile.extra.clone()),
    })
}

/// Parse enabled IDs in order, retaining overrides and rejecting duplicate or invalid IDs.
pub fn profiles(ids: &str, existing: &[ModelProfile]) -> Result<Vec<ModelProfile>> {
    let mut seen = HashSet::new();
    ids.split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(|id| {
            validate_id(id)?;
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

/// Parse optional token overrides without changing other capabilities.
/// Known connection defaults also participate in input-budget validation.
pub fn limits(
    profile: &ModelProfile,
    values: [&str; 3],
    defaults: Option<(usize, u32)>,
) -> Result<ModelProfile> {
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
    if let Some((context, output)) = defaults {
        let output = updated
            .max_output_tokens
            .unwrap_or(output);
        ensure!(output > 0, "最大输出 Token 必须大于 0。");
        let input = updated
            .input_context_tokens
            .unwrap_or_else(|| {
                updated
                    .context_tokens
                    .unwrap_or(context)
                    .saturating_sub(output as usize)
            });
        ensure!(
            input >= MIN_CONTEXT_TOKENS,
            "预留输出后，可用输入预算不能小于 {MIN_CONTEXT_TOKENS} Token。"
        );
    }
    Ok(updated)
}

/// Discover models with the draft authentication choice without persisting credentials.
pub async fn discover(
    draft: &ConnectionDraft,
    key: &KeyChange<'_>,
    directory: &std::path::Path,
) -> Result<Vec<ModelProfile>> {
    draft.validate_service(key)?;
    let ids = if let KeyChange::Keep = key
        && let Some(provider) = draft.provider()
    {
        if let Some(name) = &provider.credential {
            let reference = orbit_auth::CredentialRef {
                directory: directory.to_path_buf(),
                name: name.clone(),
            };

            if draft.account() {
                let payload =
                    orbit_model::discovery::discover(&reference, draft.format, &draft.endpoint)
                        .await?;
                return Ok(crate::models::account_models::normalize(draft.format, &payload)?.0);
            }

            orbit_model::discovery::discover_saved_api_models(
                &reference,
                &draft.endpoint,
                draft.format,
            )
            .await?
        } else {
            orbit_model::discovery::discover_api_models(
                &draft.endpoint,
                draft.format,
                &provider.api_key,
            )
            .await?
        }
    } else {
        orbit_model::discovery::discover_api_models(
            &draft.endpoint,
            draft.format,
            match key {
                KeyChange::Replace(secret) => secret,
                _ => "",
            },
        )
        .await?
    };
    Ok(ids
        .iter()
        .map(|id| {
            draft
                .models
                .iter()
                .find(|model| &model.id == id)
                .cloned()
                .unwrap_or_else(|| ModelProfile::new(id))
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn discovery_retains_overrides_and_saved_credentials_reload() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        let auth = root.path().join("auth");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let mut draft = ConnectionDraft::load(&config, None).unwrap();
        draft.name = "generated-gateway".into();
        draft.endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let mut existing = ModelProfile::new("generated-model");
        existing.context_tokens = Some(65_536);
        existing.supports_vision = Some(false);
        draft.models = vec![existing];
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") {
                request.push(stream.read_u8().await.unwrap());
                assert!(request.len() < 16 * 1_024);
            }
            let request = String::from_utf8(request).unwrap();
            assert!(request.starts_with("GET /v1/models "));
            assert!(
                request
                    .to_ascii_lowercase()
                    .contains("authorization: bearer generated-key")
            );
            let body = serde_json::json!({
                "object": "list",
                "data": [{"id": "generated-model"}, {"id": "generated-second-model"}],
            })
            .to_string();
            stream.write_all(format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            ).as_bytes()).await.unwrap();
        });
        draft.models = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            discover(&draft, &KeyChange::Replace("generated-key"), &auth),
        )
        .await
        .unwrap()
        .unwrap();
        server.await.unwrap();
        assert!(!config.exists());
        assert_eq!(draft.models.len(), 2);
        assert_eq!(draft.models[0].context_tokens, Some(65_536));
        assert_eq!(draft.models[0].supports_vision, Some(false));
        super::super::edit::save(&config, &auth, &draft, KeyChange::Replace("generated-key"))
            .await
            .unwrap();
        let reloaded = ConnectionDraft::load(&config, Some("generated-gateway")).unwrap();
        assert_eq!(reloaded.models.len(), 2);
        assert_eq!(reloaded.models[0].context_tokens, Some(65_536));
        assert!(
            !std::fs::read_to_string(config)
                .unwrap()
                .contains("generated-key")
        );
    }

    #[test]
    fn connection_defaults_participate_in_the_same_budget_validation() {
        let profile = ModelProfile::new("sample-model");
        assert!(limits(&profile, ["12288", "", ""], Some((65_536, 8_192))).is_err());
        assert!(limits(&profile, ["", "", ""], Some((65_536, 8_192))).is_ok());
        assert!(limits(&profile, ["", "", "0"], Some((65_536, 8_192))).is_err());
        assert!(limits(&profile, ["", "8192", "16384"], Some((16_384, 8_192))).is_ok());
        assert!(validate_id("   ").is_err());
        assert!(validate_id("opaque,identifier").is_ok());
    }

    #[test]
    fn editing_limits_preserves_other_model_settings_and_blank_restores_defaults() {
        let mut profile = ModelProfile::new("model");
        profile.supports_vision = Some(true);
        profile.reasoning = Some(crate::config::ReasoningProfile {
            adapter: orbit_model::types::ReasoningAdapter::OpenaiResponses,
            efforts: vec![orbit_model::types::ReasoningEffort::High],
            default: orbit_model::types::ReasoningEffort::High,
            extra: Default::default(),
        });
        profile.context_tokens = Some(65_536);
        profile.input_context_tokens = Some(32_768);
        profile.max_output_tokens = Some(4_096);
        let updated = limits(&profile, ["131072", "", "8192"], None).unwrap();
        assert_eq!(updated.context_tokens, Some(131_072));
        assert_eq!(updated.input_context_tokens, None);
        assert_eq!(updated.max_output_tokens, Some(8_192));
        assert_eq!(updated.supports_vision, Some(true));
        assert_eq!(
            serde_json::to_value(&updated.reasoning).unwrap(),
            serde_json::to_value(&profile.reasoning).unwrap()
        );
        assert_eq!(updated.id, profile.id);
        let defaults = limits(&updated, ["", "", ""], None).unwrap();
        assert!(defaults.context_tokens.is_none());
        assert!(defaults.input_context_tokens.is_none());
        assert!(defaults.max_output_tokens.is_none());
        assert_eq!(defaults.supports_vision, Some(true));
        assert_eq!(
            serde_json::to_value(&defaults.reasoning).unwrap(),
            serde_json::to_value(&profile.reasoning).unwrap()
        );
    }

    #[test]
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
            assert!(limits(&profile, values, None).is_err(), "{values:?}");
        }
        let mut configured = profile.clone();
        configured.max_output_tokens = Some(8_192);
        let profiles = profiles(" new, known ", &[configured]).unwrap();
        assert_eq!(profiles[0].id, "new");
        assert_eq!(profiles[1].max_output_tokens, Some(8_192));
        assert!(super::profiles("known, known", &profiles).is_err());
        assert!(profile.max_output_tokens.is_none());
    }
}
