use super::*;
use crate::types::{ReasoningAdapter, ReasoningEffort};

#[test]
fn advertised_api_metadata_survives_discovery_without_pinned_model_names() {
    let catalog = parse(
        ApiFormat::OpenaiChatCompletions,
        &json!({
            "data": [{
                "id": "generated-vision-reasoner",
                "context_window": 262_144,
                "max_output_tokens": 32_768,
                "input_modalities": ["text", "image"],
                "effort": {
                    "supported_levels": ["low", "high", "future-level"],
                    "default_level": "high"
                }
            }, {
                "id": "generated-text-only",
                "input_modalities": ["text"]
            }]
        }),
    )
    .unwrap();
    let model = &catalog.models[0];
    assert_eq!(model.context_tokens, Some(262_144));
    assert_eq!(model.max_output_tokens, Some(32_768));
    assert_eq!(model.supports_vision, Some(true));
    let reasoning = model.reasoning.as_ref().unwrap();
    assert_eq!(
        reasoning.efforts,
        [ReasoningEffort::Low, ReasoningEffort::High]
    );
    assert_eq!(reasoning.default, ReasoningEffort::High);
    assert_eq!(catalog.models[1].supports_vision, Some(false));
    assert!(catalog.models[1].reasoning.is_none());
}

#[test]
fn partial_observations_preserve_known_models_but_complete_empty_catalogs_clear_them() {
    let previous = vec![
        ModelProfile::new("generated-first"),
        ModelProfile::new("generated-updated"),
    ];
    let mut updated = ModelProfile::new("generated-updated");
    updated.context_tokens = Some(128_000);
    let mut catalog = ModelCatalog {
        models: vec![updated, ModelProfile::new("generated-next")],
        unavailable: Vec::new(),
        coverage: CatalogCoverage::Partial,
    };
    let resolved = catalog.resolve(previous);
    assert_eq!(
        resolved
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["generated-first", "generated-updated", "generated-next"]
    );
    assert_eq!(resolved[1].context_tokens, Some(128_000));
    catalog.models.clear();
    assert_eq!(catalog.resolve(resolved.clone()).len(), 3);
    catalog.coverage = CatalogCoverage::Complete;
    assert!(catalog.resolve(resolved).is_empty());
}

#[test]
fn anthropic_discovery_uses_advertised_capacity_vision_and_adaptive_efforts() {
    let (models, _) = normalize(
        ApiFormat::AnthropicMessages,
        &json!({
            "data": [
                {
                    "id": "orbit-adaptive",
                    "max_input_tokens": 1_000_000,
                    "max_tokens": 64_000,
                    "capabilities": {
                        "image_input": {
                            "supported": true
                        },
                        "thinking": {
                            "types": {
                                "adaptive": {
                                    "supported": true
                                }
                            }
                        },
                        "effort": {
                            "supported": true,
                            "low": {
                                "supported": true
                            },
                            "medium": {
                                "supported": false
                            },
                            "high": {
                                "supported": true
                            },
                            "max": {
                                "supported": true
                            },
                            "future": {
                                "supported": true
                            }
                        }
                    }
                },
                {
                    "id": "orbit-unknown"
                }
            ]
        }),
    )
    .unwrap();
    let profile = &models[0];
    assert_eq!(profile.input_context_tokens, Some(1_000_000));
    assert_eq!(profile.max_output_tokens, Some(64_000));
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
fn codex_discovery_preserves_new_models_and_advertised_efforts() {
    let payload = json!({
        "models": [
            {
                "slug": "gpt-new",
                "tool_mode": "code_mode_only",
                "context_window": 1_000_000,
                "input_modalities": ["text", "image"],
                "default_reasoning_level": "high",
                "supported_reasoning_levels": [
                    {
                        "effort": "low"
                    },
                    {
                        "effort": "high"
                    },
                    {
                        "effort": "unknown"
                    }
                ]
            },
            {
                "slug": "internal",
                "visibility": "hidden"
            }
        ]
    });

    let (models, unavailable) = normalize(ApiFormat::ChatgptCodex, &payload).unwrap();

    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "gpt-new");
    assert_eq!(models[0].context_tokens, Some(1_000_000));
    assert_eq!(models[0].supports_vision, Some(true));
    assert_eq!(
        models[0]
            .reasoning
            .as_ref()
            .unwrap()
            .efforts,
        [ReasoningEffort::Low, ReasoningEffort::High]
    );
    assert!(unavailable.is_empty());
}

#[test]
fn antigravity_preserves_server_model_ids_without_collapsing_routes() {
    let payload = json!({
        "models": {
            "gemini-3.8-flash-low": {
                "maxTokens": 1_048_576,
                "supportsImages": true
            },
            "gemini-3.8-flash-high": {
                "maxTokens": 1_048_576,
                "supportsImages": true
            },
            "future-model": {
                "maxTokens": 128_000
            },
            "private-model": {
                "isInternal": true
            }
        }
    });

    let (models, _) = normalize(ApiFormat::GoogleCodeAssist, &payload).unwrap();

    assert_eq!(models.len(), 3);
    assert!(
        models
            .iter()
            .any(|model| model.id == "future-model")
    );
    assert!(
        models
            .iter()
            .any(|model| model.id == "gemini-3.8-flash-high")
    );
    assert!(normalize(ApiFormat::GoogleCodeAssist, &json!({})).is_err());
    assert!(
        normalize(
            ApiFormat::ChatgptCodex,
            &json!({
                "models": []
            })
        )
        .unwrap()
        .0
        .is_empty()
    );
}

#[test]
fn antigravity_empty_agent_selectors_clear_the_catalog_but_missing_selectors_remain_compatible() {
    let mut payload = json!({
        "models": {
            "future-model": {},
            "disabled-model": {
                "disabled": true
            }
        },
        "agentModelSorts": []
    });
    assert!(
        normalize(ApiFormat::GoogleCodeAssist, &payload)
            .unwrap()
            .0
            .is_empty()
    );
    payload
        .as_object_mut()
        .unwrap()
        .remove("agentModelSorts");
    let models = normalize(ApiFormat::GoogleCodeAssist, &payload)
        .unwrap()
        .0;
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].id, "future-model");
    payload["agentModelSorts"] = json!([{
        "groups": [{
            "modelIds": "malformed"
        }]
    }]);
    assert!(normalize(ApiFormat::GoogleCodeAssist, &payload).is_err());
}

#[test]
fn antigravity_uses_every_selector_group_and_excludes_deprecated_models() {
    let payload = json!({
        "models": {
            "a": {},
            "b": {},
            "retired": {},
            "defined-only": {}
        },
        "deprecatedModelIds": {
            "retired": {
                "newModelId": "b"
            }
        },
        "agentModelSorts": [
            {
                "groups": [{
                    "modelIds": ["a", "retired"]
                }]
            },
            {
                "groups": [
                    {
                        "modelIds": ["b"]
                    },
                    {
                        "modelIds": ["a"]
                    }
                ]
            }
        ]
    });
    let models = normalize(ApiFormat::GoogleCodeAssist, &payload)
        .unwrap()
        .0;
    assert_eq!(
        models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
}

#[test]
fn deepseek_refresh_uses_advertised_models_without_inventing_capabilities() {
    let payload = json!({
        "data": [{
            "id": "future-model"
        }]
    });

    let (models, unavailable) = normalize(ApiFormat::OpenaiChatCompletions, &payload).unwrap();

    assert_eq!(models[0].id, "future-model");
    assert!(models[0].context_tokens.is_none());
    assert!(models[0].reasoning.is_none());
    assert!(unavailable.is_empty());
    assert!(crate::discovery::supports(
        ApiFormat::OpenaiChatCompletions,
        orbit_auth::DEEPSEEK_BASE
    ));
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
