use super::*;

fn fixture() -> Catalog {
    normalize(json!({
        "openai": {
            "models": {
                "fixture": {
                    "limit": {
                        "context": 200_000,
                        "input": 180_000,
                        "output": 64_000
                    },
                    "modalities": {
                        "input": ["text", "image"]
                    },
                    "reasoning_options": [{
                        "type": "effort",
                        "values": ["low", "high", "future"]
                    }]
                },
                "unavailable": {
                    "limit": {
                        "context": 1_000_000
                    }
                }
            }
        },
        "other": {
            "api": "https://other.example/v1",
            "models": {
                "fixture": {
                    "limit": {
                        "context": 120_000,
                        "output": 16_000
                    },
                    "modalities": {
                        "input": ["text"]
                    },
                    "reasoning_options": [{
                        "type": "effort",
                        "values": ["high"]
                    }]
                }
            }
        }
    }))
    .unwrap()
}

#[tokio::test]
async fn repeated_refresh_replaces_inherited_specs_without_rewriting_declarations() {
    use crate::test_support::{TestConfig, TestModel, TestProvider};
    let directory = tempfile::tempdir().unwrap();
    let mut config = TestConfig::new()
        .auth(directory.path())
        .provider(
            TestProvider::responses("test")
                .endpoint("https://api.openai.com/v1")
                .models([TestModel::new("fixture").vision(false)]),
        )
        .build()
        .unwrap();
    let declared = toml::to_string(&config).unwrap();
    let path = directory
        .path()
        .join("model-metadata/catalog.json");
    let mut catalog = fixture();
    save(&path, &catalog).unwrap();
    assert!(
        crate::models::updater::refresh(&mut config, false)
            .await
            .diagnostics
            .is_empty()
    );
    assert_eq!(
        config.providers["test"].models[0].context_tokens,
        Some(200_000)
    );
    let spec = catalog
        .providers
        .get_mut("openai")
        .unwrap()
        .models
        .get_mut("fixture")
        .unwrap();
    spec.limit.context = Some(300_000);
    spec.limit.input = None;
    save(&path, &catalog).unwrap();
    assert!(
        crate::models::updater::refresh(&mut config, false)
            .await
            .diagnostics
            .is_empty()
    );
    let model = &config.providers["test"].models[0];
    assert_eq!(model.context_tokens, Some(300_000));
    assert!(model.input_context_tokens.is_none());
    assert_eq!(model.supports_vision, Some(false));
    assert_eq!(toml::to_string(&config).unwrap(), declared);
}

fn provider(base: &str, format: ApiFormat) -> ProviderConfig {
    crate::test_support::TestProvider::new("test")
        .endpoint(base)
        .format(format)
        .models(["fixture", "unknown"])
        .build()
}

#[test]
fn exact_endpoint_and_model_metadata_fills_missing_fields_without_adding_models() {
    let mut provider = provider("https://api.openai.com/v1/", ApiFormat::OpenaiResponses);
    fixture().supplement(
        provider.api_format,
        &provider.spec.base_url,
        &mut provider.models,
    );
    assert_eq!(provider.models.len(), 2);
    let model = &provider.models[0];
    assert_eq!(model.context_tokens, Some(200_000));
    assert_eq!(model.input_context_tokens, Some(180_000));
    assert_eq!(model.max_output_tokens, Some(64_000));
    assert_eq!(model.supports_vision, Some(true));
    let reasoning = model.reasoning.as_ref().unwrap();
    assert_eq!(reasoning.adapter, ReasoningAdapter::OpenaiResponses);
    assert_eq!(
        reasoning.efforts,
        [ReasoningEffort::Low, ReasoningEffort::High]
    );
    assert_eq!(reasoning.default, ReasoningEffort::High);
    assert!(
        provider.models[1]
            .context_tokens
            .is_none()
    );
    assert!(provider.spec.model_overrides.is_empty());

    for base in [
        "https://relay.example/v1",
        "https://api.openai.com/custom/v1",
        "http://api.openai.com/v1",
    ] {
        let mut unknown = self::provider(base, ApiFormat::OpenaiResponses);
        fixture().supplement(
            unknown.api_format,
            &unknown.spec.base_url,
            &mut unknown.models,
        );
        assert!(
            unknown.models[0]
                .context_tokens
                .is_none(),
            "{base}"
        );
    }
}

#[test]
fn explicit_and_discovered_fields_take_precedence_and_unknown_protocols_are_not_inferred() {
    let mut provider = provider("https://api.openai.com", ApiFormat::OpenaiResponses);
    let profile = &mut provider.models[0];
    profile.context_tokens = Some(100_000);
    profile.input_context_tokens = Some(80_000);
    profile.max_output_tokens = Some(8_000);
    profile.supports_vision = Some(false);
    profile.reasoning = Some(ReasoningProfile {
        adapter: ReasoningAdapter::OpenaiResponses,
        efforts: vec![ReasoningEffort::Low],
        default: ReasoningEffort::Low,
        extra: toml::Table::new(),
    });
    let before = serde_json::to_value(&*profile).unwrap();
    fixture().supplement(
        provider.api_format,
        &provider.spec.base_url,
        &mut provider.models,
    );
    assert_eq!(serde_json::to_value(&provider.models[0]).unwrap(), before);

    let mut other = self::provider("https://other.example/v1", ApiFormat::OpenaiChatCompletions);
    fixture().supplement(other.api_format, &other.spec.base_url, &mut other.models);
    assert_eq!(other.models[0].context_tokens, Some(120_000));
    assert_eq!(other.models[0].supports_vision, Some(false));
    assert!(other.models[0].reasoning.is_none());

    let mut account = self::provider("https://api.openai.com/v1", ApiFormat::ChatgptCodex);
    fixture().supplement(
        account.api_format,
        &account.spec.base_url,
        &mut account.models,
    );
    assert_eq!(account.models[0].context_tokens, Some(200_000));

    let mut legacy = self::provider(
        "https://chatgpt.com/backend-api/codex",
        ApiFormat::ChatgptCodex,
    );
    fixture().supplement(legacy.api_format, &legacy.spec.base_url, &mut legacy.models);
    assert!(
        legacy.models[0]
            .context_tokens
            .is_none()
    );
}

#[test]
fn shared_endpoint_conflicts_and_invalid_limits_do_not_supply_ambiguous_specs() {
    let mut catalog = fixture();
    let mut conflicting = catalog.providers["other"].clone();
    conflicting
        .models
        .get_mut("fixture")
        .unwrap()
        .limit
        .context = Some(300_000);
    catalog
        .providers
        .insert("other-plan".into(), conflicting);
    let mut provider = provider("https://other.example/v1", ApiFormat::AnthropicMessages);
    catalog.supplement(
        provider.api_format,
        &provider.spec.base_url,
        &mut provider.models,
    );
    assert!(
        provider.models[0]
            .context_tokens
            .is_none()
    );

    let mut model: ModelProfile = serde_json::from_value(json!({
        "id": "fixture"
    }))
    .unwrap();
    let spec: Spec = serde_json::from_value(json!({
        "limit": {
            "context": 10_000,
            "input": 20_000,
            "output": 0
        }
    }))
    .unwrap();
    spec.supplement(&mut model, "other", ApiFormat::AnthropicMessages);
    assert_eq!(model.context_tokens, Some(10_000));
    assert!(model.input_context_tokens.is_none());
    assert!(model.max_output_tokens.is_none());
}

#[tokio::test]
async fn disabled_refresh_uses_cached_specs_without_network_access() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = FileConfig::parse(
        r#"[providers.test]
api_format = "openai_responses"
base_url = "https://api.openai.com/v1"
auto_refresh_models = false
models = ["fixture"]
[web]
proxy = "http://invalid.invalid:1"
"#,
        directory.path(),
    )
    .unwrap();
    let path = directory
        .path()
        .join("model-metadata/catalog.json");
    save(&path, &fixture()).unwrap();
    assert!(
        update(&mut config, false, true)
            .await
            .error
            .is_none()
    );
    assert_eq!(
        config.providers["test"].models[0].context_tokens,
        Some(200_000)
    );
}

#[test]
fn snapshots_round_trip_and_invalid_cache_sources_are_rejected() {
    let catalog = bundled();
    assert!(catalog.providers.len() > 100);
    assert!(
        catalog
            .providers
            .values()
            .map(|p| p.models.len())
            .sum::<usize>()
            > 1_000
    );
    assert!(
        normalize(json!({
            "broken": {
                "models": {
                    "": {},
                    "bad": {
                        "limit": {
                            "output": -1
                        }
                    }
                }
            }
        }))
        .is_err()
    );
    let directory = tempfile::tempdir().unwrap();
    let path = directory
        .path()
        .join("nested/catalog.json");
    save(&path, &fixture()).unwrap();
    let cached = read_cache(&path).unwrap();
    assert_eq!(
        cached.providers["openai"].models["fixture"],
        fixture().providers["openai"].models["fixture"]
    );
    let mut invalid = fixture();
    invalid.source["url"] = json!("https://other.example/api.json");
    save(&path, &invalid).unwrap();
    assert!(read_cache(&path).is_none());
    fs::write(&path, "broken JSON").unwrap();
    assert!(read_cache(&path).is_none());
    let file = fs::File::create(&path).unwrap();
    file.set_len(MAX_CATALOG_BYTES as u64 + 1)
        .unwrap();
    assert!(read_cache(&path).is_none());
}
