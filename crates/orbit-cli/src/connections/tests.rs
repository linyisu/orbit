use super::*;
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

    assert_eq!(config.default_provider, "gateway");
    assert_eq!(models[0].config.base_url, "http://127.0.0.1:8000/custom/v1");
    assert_eq!(models[0].config.api_format, ApiFormat::OpenaiResponses);
    assert!(models[0].config.api_key.is_none());
    assert!(!saved.contains("private-test-key"));
    let store = orbit_auth::AuthStore::new(auth).unwrap();
    let credential = models[0].config.credential.as_ref().unwrap();
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
    std::fs::write(
        &path,
        "# preserve me\ndefault_provider = 'deepseek-account'\n[providers]\n",
    )
    .unwrap();
    let catalog = serde_json::json!([{ "id": "server-model-a" }, { "id": "server-model-b" }]);

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
        models[0].config.credential.as_ref().unwrap().name,
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
    std::fs::write(
        &path,
        "default_provider = 'zhipu-coding-plan-account'\n[providers]\n",
    )
    .unwrap();
    let provider = orbit_auth::ApiKeyProvider::ZhipuCodingPlan;
    let catalog = serde_json::json!([{ "id": "glm-server-model" }]);

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
        models[0].config.credential.as_ref().unwrap().name,
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
    let original = "# user settings\ndefault_provider = 'local'\n[providers.local]\nbase_url = 'http://localhost:8000/v1'\napi_key = 'test-key'\nmodels = [{ id = 'local-model' }]\n";
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
        let connected = models
            .iter()
            .find(|entry| entry.provider == format!("{credential}-account"))
            .unwrap();

        assert_eq!(
            connected.config.credential.as_ref().unwrap().directory,
            auth
        );
        assert_eq!(
            connected.config.credential.as_ref().unwrap().name,
            credential
        );
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
fn existing_antigravity_catalog_expands_without_rewriting_user_settings() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    let auth = directory.path().join("auth");
    let original = "default_provider = 'account'\n[providers.account]\napi_format = 'google_code_assist'\nbase_url = 'https://daily-cloudcode-pa.googleapis.com/'\ncredential = 'antigravity'\nmodels = [{ id = 'gemini-3-pro', context_tokens = 90000 }, { id = 'custom-model' }]\n";
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

    assert_eq!(options.len(), 18);
    assert_eq!(reconnected.len(), options.len());
    assert_eq!(options[0].context_tokens, 90000);
    for id in [
        "gemini-3.8-flash",
        "claude-opus-4-6",
        "gpt-oss-120b",
        "custom-model",
    ] {
        assert!(
            options.iter().any(|model| model.model == id),
            "missing {id}"
        );
    }
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);

    let connected = append_profile(
        &path,
        &auth,
        "chatgpt",
        ApiFormat::ChatgptCodex,
        "https://chatgpt.com/backend-api/codex/",
    )
    .unwrap();

    assert_eq!(connected.len(), 19);
    assert!(
        connected
            .iter()
            .any(|model| model.model == "gemini-3.8-flash")
    );
    assert!(connected.iter().any(|model| model.model == "gpt-5.5"));
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
    let original = "default_provider = 'google-account'\n[providers.google-account]\nbase_url = 'http://localhost:8000/v1'\napi_key = 'test-key'\nmodels = [{ id = 'existing' }]\n";
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
    assert!(
        models
            .iter()
            .any(|model| model.provider == "google-account-2")
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
        .map(|id| serde_json::from_value(serde_json::json!({ "id": id })))
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
    let original = "# keep this comment\ndefault_provider = 'gateway'\n[providers.gateway]\napi_format = 'openai_responses'\nbase_url = 'http://localhost:8080/v1'\ncredential = 'existing'\ntimeout_seconds = 123\nmodels = [{ id = 'a' }, { id = 'b' }]\n";
    std::fs::write(&path, original).unwrap();
    let store = orbit_auth::AuthStore::new(auth.clone()).unwrap();
    store
        .set_api_key("existing", "secret".into(), "http://localhost:8080/v1")
        .await
        .unwrap();
    let mut draft = edit::ConnectionDraft::load(&path, Some("gateway")).unwrap();
    draft.models[0].max_output_tokens = Some(4096);
    draft.models[0].context_tokens = Some(65536);
    draft.models[0].supports_vision = Some(true);
    draft.models[1].max_output_tokens = Some(8192);
    draft.models[1].context_tokens = Some(131072);
    draft.models[1].supports_vision = Some(false);
    draft.models.reverse();

    let options = edit::save(&path, &auth, &draft, edit::KeyChange::Keep)
        .await
        .unwrap();
    let saved = std::fs::read_to_string(&path).unwrap();
    let mut reloaded = crate::config::FileConfig::load(&path, &auth).unwrap();
    crate::account_models::refresh(&mut reloaded, true).await;

    assert!(saved.starts_with("# keep this comment"));
    assert!(!saved.contains("secret"));
    assert_eq!(options[0].model, "b");
    assert!(options[0].default_for_provider);
    assert_eq!(options[0].max_output_tokens, 8192);
    assert_eq!(options[1].max_output_tokens, 4096);
    assert_eq!(options[0].input_context_tokens, 131072 - 8192);
    assert_eq!(options[1].input_context_tokens, 65536 - 4096);
    assert!(!options[0].supports_vision);
    assert!(options[1].supports_vision);
    assert_eq!(
        options[0].config.credential.as_ref().unwrap().name,
        "existing"
    );
    assert_eq!(reloaded.providers["gateway"].timeout_seconds, 123);
    assert_eq!(reloaded.providers["gateway"].models.len(), 2);
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
    draft.models = vec![serde_json::from_value(serde_json::json!({ "id": "a" })).unwrap()];
    std::fs::write(&path, "# changed\n").unwrap();

    let result = edit::save(&path, &auth, &draft, edit::KeyChange::Replace("new-secret")).await;

    assert!(result.is_err());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "# changed\n");
    assert!(!auth.exists());
}
