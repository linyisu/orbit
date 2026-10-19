use super::*;

#[tokio::test]
async fn adding_another_official_key_keeps_existing_account_and_catalog() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    let auth = root.path().join("auth");
    for (secret, model) in [
        ("generated-first-key", "generated-first-model"),
        ("generated-second-key", "generated-second-model"),
    ] {
        save_api_key(
            &path,
            &auth,
            orbit_auth::ApiKeyProvider::Deepseek,
            secret.into(),
            vec![model.into()],
        )
        .await
        .unwrap();
    }
    let mut config = crate::config::FileConfig::load(&path, &auth).unwrap();
    crate::models::updater::load_cached(&mut config).await;
    assert_eq!(config.providers.len(), 2);
    let models = config.model_options().unwrap();
    assert_eq!(models.len(), 2);
    let store = orbit_auth::AuthStore::new(auth).unwrap();
    let mut seen = Vec::new();
    for model in models {
        let credential = model.config.credential.unwrap();
        let authorization = store
            .resolve(
                &credential.name,
                &url::Url::parse(orbit_auth::DEEPSEEK_BASE).unwrap(),
                orbit_auth::AuthTarget::Openai,
            )
            .await
            .unwrap();
        seen.push((
            model.model,
            authorization.headers["authorization"]
                .to_str()
                .unwrap()
                .to_owned(),
        ));
    }
    seen.sort();
    assert_eq!(
        seen,
        [
            (
                "generated-first-model".into(),
                "Bearer generated-first-key".into()
            ),
            (
                "generated-second-model".into(),
                "Bearer generated-second-key".into()
            )
        ]
    );
}

#[tokio::test]
async fn verified_api_capabilities_survive_save_and_offline_reload() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    let auth = root.path().join("auth");
    let catalog = orbit_model::catalog::parse(
        ApiFormat::OpenaiChatCompletions,
        &serde_json::json!({
            "data": [{
                "id": "generated-capable-model",
                "context_window": 262_144,
                "max_output_tokens": 32_768,
                "input_modalities": ["text", "image"],
                "effort": { "supported_levels": ["low", "high"], "default_level": "high" }
            }]
        }),
    )
    .unwrap();
    save_api_catalog(
        &path,
        &auth,
        orbit_auth::ApiKeyProvider::Deepseek,
        "generated-secret".into(),
        catalog,
    )
    .await
    .unwrap();
    let mut config = crate::config::FileConfig::load(&path, &auth).unwrap();
    crate::models::updater::load_cached(&mut config).await;
    let models = config.model_options().unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0].context_tokens, 262_144);
    assert!(models[0].supports_vision);
    assert_eq!(models[0].reasoning_efforts.len(), 2);
    let source = std::fs::read_to_string(path).unwrap();
    assert!(!source.contains("generated-secret"));
    assert!(!source.contains("generated-capable-model"));
}

#[tokio::test]
async fn verified_key_catalog_is_cached_without_becoming_an_explicit_selection() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    let auth = root.path().join("auth");
    let ids = (0..11)
        .map(|index| format!("generated-api-model-{index}"))
        .collect::<Vec<_>>();
    let saved = save_api_key(
        &path,
        &auth,
        orbit_auth::ApiKeyProvider::Deepseek,
        "generated-api-fixture-key".into(),
        ids.clone(),
    )
    .await
    .unwrap();
    assert!(saved.diagnostics.is_empty());
    assert_eq!(saved.models.len(), ids.len());
    let source = std::fs::read_to_string(&path).unwrap();
    assert!(!source.contains("generated-api-fixture-key"));
    assert!(!source.contains("generated-api-model"));
    let mut config = crate::config::FileConfig::load(&path, &auth).unwrap();
    assert!(
        config.providers["deepseek-account"]
            .spec()
            .models
            .is_auto()
    );
    let report = crate::models::updater::load_cached(&mut config).await;
    assert!(report.diagnostics.is_empty());
    assert_eq!(
        config.providers["deepseek-account"]
            .models()
            .iter()
            .map(|model| &model.id)
            .collect::<Vec<_>>(),
        ids.iter().collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn a_cache_write_failure_keeps_the_committed_connection_and_verified_models() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    let auth = root.path().join("auth");
    let store = orbit_auth::AuthStore::new(&auth).unwrap();
    store
        .set_api_key(
            "previous",
            "generated-previous-key".into(),
            "https://example.com/v1",
        )
        .await
        .unwrap();
    std::fs::write(auth.join("model-catalog"), "generated cache obstruction").unwrap();
    let saved = save_api_key(
        &path,
        &auth,
        orbit_auth::ApiKeyProvider::Deepseek,
        "generated-current-key".into(),
        vec!["generated-verified-model".into()],
    )
    .await
    .unwrap();
    assert!(!saved.diagnostics.is_empty());
    assert_eq!(saved.models.len(), 1);
    assert!(path.is_file());
    assert!(
        store
            .list()
            .unwrap()
            .iter()
            .any(|entry| entry.name == "deepseek")
    );
    let config = crate::config::FileConfig::load(&path, &auth).unwrap();
    assert!(
        config.providers["deepseek-account"]
            .spec()
            .models
            .is_auto()
    );
}

#[tokio::test]
async fn reauthorization_rebinds_only_the_matching_connection_and_keeps_declared_choices() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    let auth = root.path().join("auth");
    let store = orbit_auth::AuthStore::new(&auth).unwrap();
    let snapshot: orbit_auth::CredentialSnapshot = serde_json::from_value(serde_json::json!({
        "version": 1, "kind": "chatgpt_plan_o_auth", "endpoint": "https://api.openai.com/v1/",
        "access": "generated-access", "refresh": "generated-refresh", "expires_at": u64::MAX,
        "account": "generated-account", "account_id": "generated-subject", "project_id": null,
        "chatgpt": {
            "client_id": "generated-client",
            "subject": "generated-subject",
            "id_token": "generated-id-token",
            "scopes": ["chatgpt.tokens.use.direct"]
        },
    }))
    .unwrap();
    store
        .import_snapshot("registration-fixture", &snapshot)
        .await
        .unwrap();
    std::fs::write(
        &path,
        concat!(
            "[providers.selected]\napi_format = 'chatgpt_codex'\n",
            "base_url = 'https://chatgpt.com/backend-api/codex/'\n",
            "credential = 'registration-fixture'\nmodels = ['generated-selected-model']\n",
            "[providers.other]\nbase_url = 'https://gateway.example/v1'\n",
            "models = ['other-model']\n",
        ),
    )
    .unwrap();
    rebind_account(&path, &auth, "registration-fixture").unwrap();
    let config = crate::config::FileConfig::load(&path, &auth).unwrap();
    assert_eq!(
        config.providers["selected"].base_url,
        "https://api.openai.com/v1/"
    );
    assert_eq!(
        config.providers["selected"].models()[0].id,
        "generated-selected-model"
    );
    assert_eq!(
        config.providers["other"].base_url,
        "https://gateway.example/v1"
    );
}

#[tokio::test]
async fn editing_an_automatic_connection_preserves_unavailable_model_overrides() {
    use crate::test_support::{TestConfig, TestModel, TestProvider};
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    let auth = directory.path().join("auth");
    TestConfig::new()
        .provider(
            TestProvider::new("auto")
                .models([TestModel::new("temporarily-unavailable")
                    .vision(false)
                    .context(100_000)])
                .automatic(),
        )
        .write(&path)
        .unwrap();
    let mut draft = edit::ConnectionDraft::load(&path, Some("auto")).unwrap();
    assert_eq!(draft.models.len(), 1);
    draft.models[0].context_tokens = Some(200_000);
    edit::save(&path, &auth, &draft, edit::KeyChange::Keep)
        .await
        .unwrap();
    let config = crate::config::FileConfig::load(&path, &auth).unwrap();
    let provider = &config.providers["auto"];
    assert!(provider.spec.models.is_auto());
    assert_eq!(
        provider.spec.model_overrides["temporarily-unavailable"].context_tokens,
        Some(200_000)
    );
    assert_eq!(
        provider.spec.model_overrides["temporarily-unavailable"].supports_vision,
        Some(false)
    );
    assert!(provider.models.is_empty());
}

#[tokio::test]
async fn first_custom_connection_bootstraps_and_reloads_without_plaintext_key() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.toml");
    let auth = temp.path().join("auth");

    let models = save_custom(
        &path,
        &auth,
        "gateway",
        ApiFormat::OpenaiResponses,
        "http://127.0.0.1:8000/custom/v1",
        "private-test-key",
        &["test-model".into()],
    )
    .await
    .unwrap();
    let config = crate::config::FileConfig::load(&path, &auth).unwrap();
    let saved = std::fs::read_to_string(&path).unwrap();

    assert!(config.providers.contains_key("gateway"));
    assert!(!saved.contains("default_provider"));
    assert_eq!(models[0].config.base_url, "http://127.0.0.1:8000/custom/v1");
    assert_eq!(models[0].config.api_format, ApiFormat::OpenaiResponses);
    assert!(models[0].config.api_key.is_none());
    assert!(!saved.contains("private-test-key"));
    let store = orbit_auth::AuthStore::new(auth).unwrap();
    let credential = models[0]
        .config
        .credential
        .as_ref()
        .unwrap();
    let headers = store
        .resolve(
            &credential.name,
            &url::Url::parse("http://127.0.0.1:8000/custom/v1/responses").unwrap(),
            orbit_auth::AuthTarget::Openai,
        )
        .await
        .unwrap()
        .headers;

    assert_eq!(headers["authorization"], "Bearer private-test-key");
}

#[tokio::test]
async fn keyless_connection_and_rejected_drafts_do_not_create_credentials() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.toml");
    let auth = temp.path().join("auth");

    let result = save_custom(
        &path,
        &auth,
        "local",
        ApiFormat::OpenaiChatCompletions,
        "http://localhost:8080/v1?secret=x",
        "secret",
        &["model".into()],
    )
    .await;

    assert!(result.is_err());
    assert!(!path.exists());
    assert!(!auth.exists());

    let models = save_custom(
        &path,
        &auth,
        "local",
        ApiFormat::OpenaiChatCompletions,
        "http://localhost:8080/v1",
        "",
        &["model".into()],
    )
    .await
    .unwrap();
    let before = std::fs::read_to_string(&path).unwrap();
    let duplicate = save_custom(
        &path,
        &auth,
        "local",
        ApiFormat::OpenaiChatCompletions,
        "http://localhost:8080/v1",
        "secret",
        &["other".into()],
    )
    .await;

    assert!(models[0].config.api_key.is_none());
    assert!(models[0].config.credential.is_none());
    assert!(duplicate.is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    assert!(!auth.exists());
}

#[test]
fn concurrent_config_change_is_not_overwritten() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.toml");
    let original = read_config(&path).unwrap();
    std::fs::write(&path, "# modified externally\n").unwrap();

    let result = persist(&path, &original, "replacement");

    assert!(result.is_err());
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "# modified externally\n"
    );
}

#[test]
fn api_key_connection_persists_discovered_models_without_a_secret() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    let auth = directory.path().join("auth");
    std::fs::write(&path, "# preserve me\n[providers]\n").unwrap();
    let catalog = vec![
        ModelProfile::new("server-model-a"),
        ModelProfile::new("server-model-b"),
    ];

    let models = append_profile_models(
        &path,
        &auth,
        "deepseek",
        ApiFormat::OpenaiChatCompletions,
        orbit_auth::DEEPSEEK_BASE,
        &catalog,
    )
    .unwrap();

    assert_eq!(models.len(), 2);
    assert_eq!(models[0].model, "server-model-a");
    assert_eq!(models[1].model, "server-model-b");
    assert_eq!(
        models[0]
            .config
            .credential
            .as_ref()
            .unwrap()
            .name,
        "deepseek"
    );
    let saved = std::fs::read_to_string(path).unwrap();
    assert!(saved.starts_with("# preserve me"));
    assert!(!saved.contains("api_key"));
}

#[test]
fn zhipu_profile_uses_domestic_coding_endpoint_and_credential() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    let auth = directory.path().join("auth");
    std::fs::write(&path, "[providers]\n").unwrap();
    let provider = orbit_auth::ApiKeyProvider::ZhipuCodingPlan;
    let catalog = vec![ModelProfile::new("glm-server-model")];

    let models = append_profile_models(
        &path,
        &auth,
        provider.name(),
        ApiFormat::OpenaiChatCompletions,
        provider.base_url(),
        &catalog,
    )
    .unwrap();

    assert_eq!(models[0].model, "glm-server-model");
    assert_eq!(
        models[0]
            .config
            .credential
            .as_ref()
            .unwrap()
            .name,
        "zhipu-coding-plan"
    );
    let saved = std::fs::read_to_string(path).unwrap();
    assert!(saved.contains(orbit_auth::ZHIPU_CODING_BASE));
    assert!(!saved.contains("api_key"));
}

#[test]
fn connection_preserves_config_and_reloads_both_account_protocols() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    let auth = directory.path().join("auth");
    let original = "# user settings\n[providers.local]\nbase_url = \
        'http://localhost:8000/v1'\napi_key = 'test-key'\nmodels = [\"local-model\"]\n";
    std::fs::write(&path, original).unwrap();

    for (credential, format, endpoint) in [
        (
            "antigravity",
            ApiFormat::GoogleCodeAssist,
            "https://daily-cloudcode-pa.googleapis.com/",
        ),
        (
            "google",
            ApiFormat::GoogleCodeAssist,
            "https://cloudcode-pa.googleapis.com/",
        ),
        (
            "chatgpt",
            ApiFormat::ChatgptCodex,
            "https://chatgpt.com/backend-api/codex/",
        ),
    ] {
        let models = append_profile(&path, &auth, credential, format, endpoint).unwrap();
        let parsed = crate::config::FileConfig::load(&path, &auth).unwrap();
        let connected = &parsed.providers[&format!("{credential}-account")];
        assert_eq!(connected.credential.as_deref(), Some(credential));
        assert_eq!(connected.base_url, endpoint);
        assert!(connected.models().is_empty());
        assert!(matches!(
            connected.spec().models,
            ModelSelection::Automatic(_)
        ));
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .starts_with(original)
        );

        let before = std::fs::read_to_string(&path).unwrap();
        let reconnected = append_profile(&path, &auth, credential, format, endpoint).unwrap();

        assert_eq!(
            crate::config::FileConfig::load(&path, &auth)
                .unwrap()
                .model_options()
                .unwrap()
                .len(),
            models.len()
        );
        assert_eq!(reconnected.len(), models.len());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
    }

    assert!(!auth.exists());
}

#[test]
fn explicit_account_selection_is_preserved_when_connecting_another_account() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    let auth = directory.path().join("auth");
    let original = "[providers.account]\napi_format = 'google_code_assist'\nbase_url = \
        'https://daily-cloudcode-pa.googleapis.com/'\ncredential = 'antigravity'\nmodels = \
        [\"gemini-3-pro\", \"custom-model\"]\n[providers.account.model_overrides.\"gemini-3-pro\"]\
        \ncontext_tokens = 90000\n";
    std::fs::write(&path, original).unwrap();

    let config = crate::config::FileConfig::load(&path, &auth).unwrap();
    let options = config.model_options().unwrap();
    let reconnected = append_profile(
        &path,
        &auth,
        "antigravity",
        ApiFormat::GoogleCodeAssist,
        "https://daily-cloudcode-pa.googleapis.com/",
    )
    .unwrap();

    assert_eq!(
        options
            .iter()
            .map(|m| m.model.as_str())
            .collect::<Vec<_>>(),
        ["gemini-3-pro", "custom-model"]
    );
    assert_eq!(reconnected.len(), options.len());
    assert_eq!(options[0].context_tokens, 90_000);

    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);

    let connected = append_profile(
        &path,
        &auth,
        "chatgpt",
        ApiFormat::ChatgptCodex,
        "https://chatgpt.com/backend-api/codex/",
    )
    .unwrap();

    assert_eq!(connected.len(), options.len());
    assert!(
        connected
            .iter()
            .any(|model| model.model == "gemini-3-pro")
    );
    assert!(
        connected
            .iter()
            .all(|model| model.provider != "chatgpt-account")
    );
    assert_eq!(
        crate::config::FileConfig::load(&path, &auth)
            .unwrap()
            .model_options()
            .unwrap()
            .len(),
        connected.len()
    );
    assert!(!auth.exists());
}

#[test]
fn connection_keeps_existing_provider_and_rejects_invalid_config_without_writing() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    let auth = directory.path().join("auth");
    let original = "[providers.google-account]\nbase_url = 'http://localhost:8000/v1'\napi_key = \
        'test-key'\nmodels = [\"existing\"]\n";
    std::fs::write(&path, original).unwrap();

    let models = append_profile(
        &path,
        &auth,
        "google",
        ApiFormat::GoogleCodeAssist,
        "https://cloudcode-pa.googleapis.com/",
    )
    .unwrap();

    assert!(
        models
            .iter()
            .any(|model| model.qualified_name() == "google-account::existing")
    );
    let parsed = crate::config::FileConfig::load(&path, &auth).unwrap();
    assert!(
        parsed
            .providers
            .contains_key("google-account-2")
    );
    assert!(
        parsed.providers["google-account-2"]
            .models()
            .is_empty()
    );
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .starts_with(original)
    );

    std::fs::write(&path, "invalid = [").unwrap();
    let result = append_profile(
        &path,
        &auth,
        "google",
        ApiFormat::GoogleCodeAssist,
        "https://cloudcode-pa.googleapis.com/",
    );

    assert!(result.is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "invalid = [");
}

async fn save_custom(
    path: &std::path::Path,
    auth: &std::path::Path,
    name: &str,
    format: ApiFormat,
    endpoint: &str,
    key: &str,
    ids: &[String],
) -> Result<Vec<ModelOption>> {
    let mut draft = edit::ConnectionDraft::load(path, None)?;
    draft.name = name.into();
    draft.format = format;
    draft.endpoint = endpoint.into();
    draft.models = ids
        .iter()
        .map(|id| {
            serde_json::from_value(serde_json::json!({
                "id": id
            }))
        })
        .collect::<Result<_, _>>()?;
    edit::save(
        path,
        auth,
        &draft,
        if key.is_empty() {
            edit::KeyChange::None
        } else {
            edit::KeyChange::Replace(key)
        },
    )
    .await
}

#[tokio::test]
async fn editing_keeps_credentials_and_independent_model_overrides() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.toml");
    let auth = temp.path().join("auth");
    let original = "# keep this comment\n[providers.gateway]\napi_format = \
        'openai_responses'\nbase_url = 'http://localhost:8080/v1'\nauto_refresh_models = \
        false\ncredential = 'existing'\ntimeout_seconds = 123\nmodels = [\"a\", \"b\"]\n";
    std::fs::write(&path, original).unwrap();
    let store = orbit_auth::AuthStore::new(auth.clone()).unwrap();
    store
        .set_api_key("existing", "secret".into(), "http://localhost:8080/v1")
        .await
        .unwrap();
    let mut draft = edit::ConnectionDraft::load(&path, Some("gateway")).unwrap();
    draft.models[0].max_output_tokens = Some(4_096);
    draft.models[0].context_tokens = Some(65_536);
    draft.models[0].supports_vision = Some(true);
    draft.models[1].max_output_tokens = Some(8_192);
    draft.models[1].context_tokens = Some(131_072);
    draft.models[1].supports_vision = Some(false);
    draft.models.reverse();

    let options = edit::save(&path, &auth, &draft, edit::KeyChange::Keep)
        .await
        .unwrap();
    let saved = std::fs::read_to_string(&path).unwrap();
    let mut reloaded = crate::config::FileConfig::load(&path, &auth).unwrap();
    let _ = crate::models::updater::refresh(&mut reloaded, true).await;

    assert!(saved.starts_with("# keep this comment"));
    assert!(!saved.contains("secret"));
    assert_eq!(options[0].model, "b");
    assert_eq!(options[0].max_output_tokens, 8_192);
    assert_eq!(options[1].max_output_tokens, 4_096);
    assert_eq!(options[0].input_context_tokens, 131_072 - 8_192);
    assert_eq!(options[1].input_context_tokens, 65_536 - 4_096);
    assert!(!options[0].supports_vision);
    assert!(options[1].supports_vision);
    assert_eq!(
        options[0]
            .config
            .credential
            .as_ref()
            .unwrap()
            .name,
        "existing"
    );
    assert_eq!(reloaded.providers["gateway"].timeout_seconds, 123);
    assert_eq!(
        reloaded.providers["gateway"]
            .models
            .len(),
        2
    );
    assert_eq!(store.list().unwrap().len(), 1);
}

#[tokio::test]
async fn stale_draft_cannot_overwrite_config_or_create_credentials() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("config.toml");
    let auth = temp.path().join("auth");
    let mut draft = edit::ConnectionDraft::load(&path, None).unwrap();
    draft.name = "local".into();
    draft.endpoint = "http://localhost/v1".into();
    draft.models = vec![
        serde_json::from_value(serde_json::json!({
            "id": "a"
        }))
        .unwrap(),
    ];
    std::fs::write(&path, "# changed\n").unwrap();

    let result = edit::save(&path, &auth, &draft, edit::KeyChange::Replace("new-secret")).await;

    assert!(result.is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "# changed\n");
    assert!(!auth.exists());
}

#[tokio::test]
async fn invalid_configuration_does_not_replace_official_api_key() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    let auth = root.path().join("auth");
    let provider = orbit_auth::ApiKeyProvider::Deepseek;
    let store = orbit_auth::AuthStore::new(&auth).unwrap();
    store
        .set_api_key(
            provider.name(),
            "original-fixture-key".into(),
            provider.base_url(),
        )
        .await
        .unwrap();
    std::fs::write(&path, "[invalid").unwrap();
    assert!(
        save_api_key(
            &path,
            &auth,
            provider,
            "replacement-fixture-key".into(),
            vec!["fixture-model".into()]
        )
        .await
        .is_err()
    );
    let auth = store
        .resolve(
            provider.name(),
            &url::Url::parse(provider.base_url()).unwrap(),
            orbit_auth::AuthTarget::Openai,
        )
        .await
        .unwrap();
    assert_eq!(auth.headers["authorization"], "Bearer original-fixture-key");
}

#[tokio::test]
async fn stale_prepared_change_rolls_back_key_and_preserves_new_configuration() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    let auth = root.path().join("auth");
    let provider = orbit_auth::ApiKeyProvider::Deepseek;
    let mut spec = ProviderSpec::new(
        ApiFormat::OpenaiChatCompletions,
        provider.base_url(),
        ModelSelection::Explicit(vec!["fixture-model".into()]),
    );
    spec.credential = Some(provider.name().into());
    let prepared = prepare_account(&path, &auth, provider.name(), &spec).unwrap();
    let store = orbit_auth::AuthStore::new(&auth).unwrap();
    store
        .set_api_key(
            provider.name(),
            "old-fixture-key".into(),
            provider.base_url(),
        )
        .await
        .unwrap();
    std::fs::write(&path, "# concurrent edit\n[providers]\n").unwrap();
    let result = store
        .commit_api_key(
            provider.name(),
            "new-fixture-key".into(),
            provider.base_url(),
            || prepared.commit(),
        )
        .await;
    assert!(matches!(
        result,
        Err(orbit_auth::CredentialCommitError::Commit { rollback: None, .. })
    ));
    assert!(
        std::fs::read_to_string(&path)
            .unwrap()
            .starts_with("# concurrent edit")
    );
    let auth = store
        .resolve(
            provider.name(),
            &url::Url::parse(provider.base_url()).unwrap(),
            orbit_auth::AuthTarget::Openai,
        )
        .await
        .unwrap();
    assert_eq!(auth.headers["authorization"], "Bearer old-fixture-key");
}

#[tokio::test]
async fn committed_configuration_survives_followup_catalog_read_failure() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("config.toml");
    let auth = root.path().join("auth");
    let spec = ProviderSpec::new(
        ApiFormat::OpenaiChatCompletions,
        "https://sample.invalid/v1",
        ModelSelection::Explicit(vec!["fixture-model".into()]),
    );
    let models = save_provider(&path, &auth, "sample", &spec).unwrap();
    let saved = std::fs::read_to_string(&path).unwrap();
    let outcome = refresh_saved(&root.path().join("missing.toml"), &auth, models).await;
    assert_eq!(outcome.models[0].model, "fixture-model");
    assert!(!outcome.diagnostics.is_empty());
    assert_eq!(std::fs::read_to_string(path).unwrap(), saved);
}
