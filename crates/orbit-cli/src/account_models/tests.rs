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
    let model: ModelProfile = serde_json::from_value(json!({"id": id, "context_tokens": 4000000,
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
        serde_json::to_vec(&json!({ "binding": binding, "payload": { "models": [] } })).unwrap(),
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
