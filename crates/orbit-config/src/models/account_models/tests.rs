use super::*;
async fn refresh(config: &mut FileConfig, force: bool) -> Vec<String> {
    refresh_connections(config, force, force, true)
        .await
        .into_iter()
        .flat_map(|refreshed| refreshed.diagnostics)
        .collect()
}

#[tokio::test]
async fn manual_update_is_reused_offline_when_automatic_refresh_is_disabled() {
    use crate::test_support::{TestConfig, TestProvider};
    let directory = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(serve_catalog(
        listener,
        json!({
            "data": [{
                "id": "fresh-model",
                "context_tokens": 200_000
            }]
        }),
    ));
    let baseline = TestConfig::new()
        .auth(directory.path())
        .provider(
            TestProvider::new("manual")
                .endpoint(endpoint)
                .automatic(),
        )
        .build()
        .unwrap();
    let mut manual = baseline.clone();
    assert!(
        refresh(&mut manual, true)
            .await
            .is_empty()
    );
    server.await.unwrap();
    assert_eq!(manual.providers["manual"].models[0].id, "fresh-model");
    // A new reader must get the manual update without contacting the stopped server.
    let mut reloaded = baseline;
    assert!(
        refresh(&mut reloaded, false)
            .await
            .is_empty()
    );
    assert_eq!(
        reloaded.providers["manual"].models[0].context_tokens,
        Some(200_000)
    );
}

#[tokio::test]
async fn cached_open_reuses_stale_account_catalog_and_manual_refresh_replaces_it() {
    use crate::test_support::{TestConfig, TestProvider};
    let root = tempfile::tempdir().unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let mut config = TestConfig::new()
        .auth(root.path())
        .provider(
            TestProvider::new("auto")
                .endpoint(endpoint.clone())
                .automatic()
                .refresh(true),
        )
        .build()
        .unwrap();
    let first = tokio::spawn(serve_catalog(
        listener,
        json!({
            "data": [
                {
                    "id": "old"
                },
                {
                    "id": "kept"
                }
            ]
        }),
    ));
    // Discover only the account catalog so this fixture never accesses public metadata.
    assert!(
        refresh(&mut config, true)
            .await
            .is_empty()
    );
    first.await.unwrap();
    let cache = root
        .path()
        .join("model-catalog")
        .join(format!("{}.json", fingerprint("auto")));
    std::fs::File::open(&cache)
        .unwrap()
        .set_times(std::fs::FileTimes::new().set_modified(SystemTime::UNIX_EPOCH))
        .unwrap();
    let mut cached = TestConfig::new()
        .auth(root.path())
        .provider(
            TestProvider::new("auto")
                .endpoint(endpoint.clone())
                .automatic()
                .refresh(true),
        )
        .build()
        .unwrap();
    assert!(
        crate::models::updater::load_cached(&mut cached)
            .await
            .diagnostics
            .is_empty()
    );
    assert_eq!(
        cached.providers["auto"]
            .models
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        ["old", "kept"]
    );
    let listener = tokio::net::TcpListener::bind(
        endpoint
            .trim_start_matches("http://")
            .trim_end_matches("/v1"),
    )
    .await
    .unwrap();
    let second = tokio::spawn(serve_catalog(
        listener,
        json!({
            "data": [{
                "id": "kept"
            }]
        }),
    ));
    assert!(
        refresh(&mut cached, true)
            .await
            .is_empty()
    );
    second.await.unwrap();
    assert_eq!(
        cached.providers["auto"]
            .models
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>(),
        ["kept"]
    );
}

fn provider(format: ApiFormat, managed: bool) -> ProviderConfig {
    use crate::test_support::{TestModel, TestProvider};
    let builder = TestProvider::new("test")
        .format(format)
        .endpoint("https://orbit.example/v1")
        .models([
            TestModel::new("orbit-live")
                .context(100_000)
                .vision(false),
            TestModel::new("orbit-retired"),
        ]);
    let mut provider = if managed {
        builder
    } else {
        builder.automatic()
    }
    .build();
    provider.discovered = Some(vec![
        ModelProfile::new("orbit-live"),
        ModelProfile::new("orbit-retired"),
    ]);
    provider.resolve();
    provider
}

#[test]
fn refresh_updates_remote_capabilities_but_preserves_explicit_fields_and_choices() {
    for managed in [false, true] {
        let mut provider = provider(ApiFormat::OpenaiResponses, managed);
        for (effort, output) in [("high", 32_000), ("low", 64_000)] {
            let (models, _) = normalize(
                ApiFormat::OpenaiResponses,
                &json!({
                    "data": [
                        {
                            "id": "orbit-live",
                            "context_tokens": 200_000,
                            "supports_vision": true,
                            "max_output_tokens": output,
                            "reasoning": {
                                "adapter": "openai_responses",
                                "efforts": [effort],
                                "default": effort
                            }
                        },
                        {
                            "id": "orbit-new"
                        }
                    ]
                }),
            )
            .unwrap();

            merge_discovered_models(&mut provider, models);
            let live = &provider.models[0];
            assert_eq!(live.context_tokens, Some(100_000));
            assert_eq!(live.supports_vision, Some(false));
            assert_eq!(live.max_output_tokens, Some(output));
            assert_eq!(
                live.reasoning
                    .as_ref()
                    .unwrap()
                    .default
                    .as_str(),
                effort
            );
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
fn automatic_catalog_uses_discovered_capacity() {
    let mut config = FileConfig::parse(
        r#"[providers.orbit]
api_format = "google_code_assist"
base_url = "https://daily-cloudcode-pa.googleapis.com"
credential = "orbit-fixture"
models = "auto"
"#,
        Path::new("/tmp/orbit-fixture-auth"),
    )
    .unwrap();
    let provider = config
        .providers
        .get_mut("orbit")
        .unwrap();
    assert!(provider.models.is_empty());
    let id = "discovered-capacity-model";
    let model = crate::test_support::TestModel::new(id)
        .context(4_000_000)
        .vision(false)
        .build();
    merge_discovered_models(provider, vec![model]);
    assert_eq!(provider.models.len(), 1);
    assert_eq!(provider.models[0].context_tokens, Some(4_000_000));
    assert_eq!(provider.models[0].supports_vision, Some(false));
}

#[test]
fn google_quota_models_do_not_retire_unlisted_or_exhausted_models() {
    let mut provider = crate::test_support::TestProvider::new("quota")
        .format(ApiFormat::GoogleCodeAssist)
        .endpoint("https://cloudcode-pa.googleapis.com")
        .automatic()
        .build();
    provider.set_discovered(vec![
        ModelProfile::new("orbit-live"),
        ModelProfile::new("orbit-retired"),
    ]);
    let (models, _) = normalize(
        ApiFormat::GoogleCodeAssist,
        &json!({
            "buckets": [
                {
                    "modelId": "orbit-live",
                    "remainingFraction": 0
                },
                {
                    "modelId": "orbit-new",
                    "remainingFraction": 1
                },
                {
                    "modelId": "orbit-new",
                    "tokenType": "REQUESTS"
                }
            ]
        }),
    )
    .unwrap();

    assert_eq!(models.len(), 2);
    assert!(
        models
            .iter()
            .all(|model| model.reasoning.is_none())
    );
    merge_catalog(
        &mut provider,
        orbit_model::catalog::ModelCatalog {
            models,
            unavailable: Vec::new(),
            coverage: orbit_model::catalog::CatalogCoverage::Partial,
        },
    );
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
    String::from_utf8(request)
        .unwrap()
        .to_ascii_lowercase()
}

#[tokio::test]
async fn all_api_connections_refresh_with_scoped_caches_and_independent_failures() {
    let temporary = tempfile::tempdir().unwrap();
    let directory = temporary.path().join("auth");
    let store = orbit_auth::AuthStore::new(directory.clone()).unwrap();
    use crate::test_support::{TestConfig, TestProvider};
    let mut fixture = TestConfig::new().auth(&directory);
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
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let endpoint = format!("http://{}/orbit/v1", listener.local_addr().unwrap());
        let connection = TestProvider::new(name)
            .format(format)
            .endpoint(&endpoint)
            .models(["orbit-live"])
            .refresh(true);
        let mut connection = if managed {
            connection
        } else {
            connection.automatic()
        };
        if saved {
            store
                .set_api_key(name, key.into(), &endpoint)
                .await
                .unwrap();
            connection = connection.credential(name);
        } else {
            connection = connection.key(key);
        }

        fixture = fixture.provider(connection);
        servers.push((
            name,
            format,
            key,
            tokio::spawn(serve_catalog(
                listener,
                json!({
                    "data": [
                        {
                            "id": "orbit-live",
                            "context_tokens": 200_000
                        },
                        {
                            "id": "orbit-new"
                        }
                    ]
                }),
            )),
        ));
    }

    fixture = fixture
        .provider(
            TestProvider::new("orbit-failure")
                .endpoint("http://127.0.0.1:0/v1")
                .models(["orbit-fallback"])
                .refresh(true),
        )
        .provider(
            TestProvider::new("orbit-disabled")
                .endpoint("http://127.0.0.1:0/v1")
                .models(["orbit-manual"]),
        );
    let baseline = fixture.build().unwrap();
    let mut config = baseline.clone();
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
            Some(200_000)
        );
        assert_eq!(
            config.providers[name].models.len(),
            if name == "orbit-responses" { 1 } else { 2 }
        );
    }

    let mut reloaded = baseline.clone();
    reloaded.auth_directory = Some(directory.clone());
    let provider = reloaded
        .providers
        .get_mut("orbit-failure")
        .unwrap();
    let mut spec = provider.spec().clone();
    spec.auto_refresh_models = false;
    provider.replace_spec(spec).unwrap();
    assert!(
        refresh(&mut reloaded, false)
            .await
            .is_empty()
    );
    assert_eq!(
        reloaded.providers["orbit-chat"].models[0].context_tokens,
        Some(200_000)
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

    let mut rotated = baseline;
    rotated.auth_directory = Some(directory.clone());
    let provider = rotated
        .providers
        .get_mut("orbit-failure")
        .unwrap();
    let mut spec = provider.spec().clone();
    spec.auto_refresh_models = false;
    provider.replace_spec(spec).unwrap();
    let provider = rotated
        .providers
        .get_mut("orbit-chat")
        .unwrap();
    let mut spec = provider.spec().clone();
    spec.api_key = "orbit-rotated-inline".into();
    provider.replace_spec(spec).unwrap();
    let endpoint = rotated.providers["orbit-responses"]
        .base_url
        .clone();
    store
        .set_api_key("orbit-responses", "orbit-rotated-saved".into(), &endpoint)
        .await
        .unwrap();
    let diagnostics = refresh(&mut rotated, false).await;
    assert_eq!(diagnostics.len(), 2, "{diagnostics:?}");
    assert!(
        rotated.providers["orbit-chat"]
            .models
            .is_empty()
    );
    assert!(
        rotated.providers["orbit-responses"].models[0]
            .context_tokens
            .is_none()
    );
}

#[test]
fn antigravity_refresh_uses_agent_selectors_instead_of_the_full_definition_table() {
    let mut provider = crate::test_support::TestProvider::new("test")
        .format(ApiFormat::GoogleCodeAssist)
        .endpoint("https://daily-cloudcode-pa.googleapis.com")
        .models([crate::test_support::TestModel::new("claude-sonnet-4-6").context(123_456)])
        .automatic()
        .build();
    let mut payload = json!({
        "models": {
            "gemini-3.7-flash-high": {
                "maxTokens": 1_048_576
            },
            "gemini-3.8-flash-high": {
                "maxTokens": 1_048_576
            },
            "gemini-3.8-flash-medium": {
                "maxTokens": 1_048_576
            },
            "gemini-3.8-flash-low": {
                "maxTokens": 1_048_576
            },
            "claude-sonnet-4-6": {
                "maxTokens": 250_000
            },
            "claude-opus-4-6-thinking": {
                "maxTokens": 250_000
            },
            "gpt-oss-120b-medium": {
                "maxTokens": 131_072
            },
            "gemini-4-argon-disabled": {
                "disabled": true
            },
            "gemini-2.5-pro": {
                "maxTokens": 1_048_576
            }
        },
        "agentModelSorts": [{
            "groups": [{
                "modelIds": [
                    "gemini-3.7-flash-high",
                    "gemini-3.8-flash-high",
                    "gemini-3.8-flash-medium",
                    "gemini-3.8-flash-low",
                    "claude-sonnet-4-6",
                    "claude-opus-4-6-thinking",
                    "gpt-oss-120b-medium",
                    "gemini-4-argon-disabled"
                ]
            }]
        }]
    });
    merge_discovered_models(
        &mut provider,
        normalize(ApiFormat::GoogleCodeAssist, &payload)
            .unwrap()
            .0,
    );
    assert_eq!(provider.models.len(), 7);
    // Retired models remain defined for other workflows, but disappear from
    // the account's Agent selectors. A successful refresh must remove them.
    payload["agentModelSorts"][0]["groups"][0]["modelIds"]
        .as_array_mut()
        .unwrap()
        .remove(0);
    let models = normalize(ApiFormat::GoogleCodeAssist, &payload)
        .unwrap()
        .0;
    merge_discovered_models(&mut provider, models);
    let ids = provider
        .models
        .iter()
        .map(|model| model.id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        ids,
        [
            "claude-opus-4-6-thinking",
            "claude-sonnet-4-6",
            "gemini-3.8-flash-high",
            "gemini-3.8-flash-medium",
            "gemini-3.8-flash-low",
            "gpt-oss-120b-medium"
        ]
        .into_iter()
        .collect()
    );
    assert_eq!(
        provider
            .models
            .iter()
            .find(|model| model.id == "claude-sonnet-4-6")
            .unwrap()
            .context_tokens,
        Some(123_456)
    );
    provider.resolve();
    assert_eq!(provider.models.len(), 6);
}

#[tokio::test]
async fn antigravity_cached_refresh_does_not_restore_unselectable_definitions() {
    let directory = tempfile::tempdir().unwrap();
    let mut provider = crate::test_support::TestProvider::new("account")
        .format(ApiFormat::GoogleCodeAssist)
        .endpoint("https://daily-cloudcode-pa.googleapis.com")
        .automatic()
        .build();
    let binding = json!({
        "version": 2,
        "identity": {
            "key_fingerprint": fingerprint("")
        },
        "format": ApiFormat::GoogleCodeAssist,
        "endpoint": provider.base_url
    });
    let cache_dir = directory.path().join("model-catalog");
    std::fs::create_dir(&cache_dir).unwrap();
    let cache = cache_dir.join(format!("{}.json", fingerprint("account")));
    let mut payload = json!({
        "models": {
            "live-model": {},
            "retired-model": {},
            "disabled-model": {
                "disabled": true
            }
        },
        "agentModelSorts": [{
            "groups": [{
                "modelIds": ["live-model", "disabled-model"]
            }]
        }]
    });
    let mut diagnostics = Vec::new();
    for expected in [vec!["live-model"], vec![]] {
        std::fs::write(
            &cache,
            serde_json::to_vec(&json!({
                "binding": binding,
                "payload": payload
            }))
            .unwrap(),
        )
        .unwrap();
        // Exercise the shared cache/resolve path without credentials or any
        // network access, including an authoritative empty account catalogue.
        refresh_provider(
            "account",
            &mut provider,
            directory.path(),
            false,
            false,
            &mut diagnostics,
        )
        .await
        .unwrap();
        assert_eq!(
            provider
                .models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            expected
        );
        assert!(provider.spec.models.is_auto());
        payload["agentModelSorts"] = json!([]);
    }
    assert!(diagnostics.is_empty());
}

#[test]
fn cache_never_crosses_accounts() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("catalog.json");
    let binding = json!({
        "account_id": "first"
    });
    std::fs::write(
        &path,
        serde_json::to_vec(&json!({
            "binding": binding,
            "payload": {
                "models": []
            }
        }))
        .unwrap(),
    )
    .unwrap();

    assert!(read_cache(&path, &binding, ApiFormat::ChatgptCodex).is_some());
    assert!(
        read_cache(
            &path,
            &json!({
                "account_id": "second"
            }),
            ApiFormat::ChatgptCodex,
        )
        .is_none()
    );
}
