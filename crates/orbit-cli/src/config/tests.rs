use super::*;

#[test]
fn managed_deepseek_models_inherit_defaults_and_preserve_explicit_reasoning() {
    let input = r#"default_provider = "orbit"
[providers.orbit]
base_url = "https://api.deepseek.com/v1/"
credential = "orbit-fixture"
managed_models = true
[[providers.orbit.models]]
id = "deepseek-flash"
[[providers.orbit.models]]
id = "deepseek-v4-pro"
reasoning = { adapter = "openai_chat", efforts = ["low", "max"], default = "low" }
"#;
    let config = FileConfig::parse(input, Path::new("/tmp/orbit-fixture-auth")).unwrap();
    assert!(config.providers["orbit"].models[0].reasoning.is_none());
    let choices = config.model_options().unwrap();
    assert_eq!(choices.len(), 2);
    assert_eq!(
        choices[0].config.reasoning_effort,
        Some(ReasoningEffort::High)
    );
    assert_eq!(
        choices[0].config.reasoning_adapter,
        Some(ReasoningAdapter::OpenaiChat)
    );
    assert_eq!(
        choices[0].reasoning_efforts,
        vec![
            ReasoningEffort::None,
            ReasoningEffort::Low,
            ReasoningEffort::High,
            ReasoningEffort::Max
        ]
    );
    assert_eq!(
        choices[1].config.reasoning_effort,
        Some(ReasoningEffort::Low)
    );
    assert_eq!(
        choices[1].reasoning_efforts,
        vec![ReasoningEffort::Low, ReasoningEffort::Max]
    );

    let mut discovered: FileConfig = toml::from_str(input).unwrap();
    discovered.auth_directory = config.auth_directory.clone();
    discovered.providers.get_mut("orbit").unwrap().models[0].reasoning = None;
    assert_eq!(
        discovered.model_options().unwrap()[0]
            .config
            .reasoning_effort,
        Some(ReasoningEffort::High)
    );
}

#[test]
fn provider_debug_redacts_credentials() {
    let provider = ProviderConfig {
        managed_models: false,
        auto_refresh_models: true,
        api_format: ApiFormat::OpenaiChatCompletions,
        base_url: "https://example.com/v1".into(),
        api_key: "test-secret".into(),
        credential: None,
        models: Vec::new(),
        configured_models: None,
        timeout_seconds: 30,
        max_retries: 0,
    };

    let rendered = format!("{provider:?}");

    assert!(rendered.contains("[redacted]"));
    assert!(!rendered.contains("test-secret"));
}

#[test]
fn omitted_limits_use_working_defaults_and_explicit_limits_survive() {
    let provider = r#"[providers.test]
base_url = "http://localhost"
api_key = "test"
[[providers.test.models]]
id = "test"
"#;
    let input = format!("default_provider = \"test\"\n{provider}");
    let config: FileConfig = toml::from_str(&input).unwrap();

    assert_eq!(config.context_tokens, 128000);
    assert!(config.auto_compact);

    let disabled: FileConfig = toml::from_str(&format!(
        "default_provider = \"test\"\nauto_compact = false\n{provider}"
    ))
    .unwrap();

    assert!(!disabled.agent_config().auto_compact);
    assert_eq!(config.max_output_tokens, 32000);
    assert_eq!(config.providers["test"].timeout_seconds, 600);
    assert_eq!(config.providers["test"].max_retries, 2);
    assert_eq!(config.agent_config().max_turns, 0);
    assert_eq!(config.agent_config().max_tool_calls, 2000);
    assert_eq!(config.command.default_timeout_seconds, 120);
    assert_eq!(config.command.max_timeout_seconds, 600);

    let config: FileConfig = toml::from_str(&format!("default_provider = \"test\"\ncontext_tokens = 1000000\nmax_output_tokens = 64000\nmax_turns = 75\nmax_tool_calls = 500\n[command]\ndefault_timeout_seconds = 300\nmax_timeout_seconds = 1800\n{provider}")).unwrap();

    assert_eq!(config.agent_config().max_turns, 75);
    assert_eq!(config.agent_config().max_tool_calls, 500);
    assert_eq!(config.max_output_tokens, 64000);
    assert_eq!(config.context_tokens, 1000000);
    assert_eq!(config.command.max_timeout_seconds, 1800);
    assert!(config.command.validate().is_ok());
}

#[test]
fn configured_provider_models_select_their_provider_connection() {
    let config: FileConfig = toml::from_str(
        r#"default_provider = "deepseek"
[providers.deepseek]
api_format = "openai_chat_completions"
base_url = "https://deepseek.example/v1"
api_key = "deepseek-key"
[[providers.deepseek.models]]
id = "deepseek-chat"
[[providers.deepseek.models]]
id = "deepseek-reasoner"
"#,
    )
    .unwrap();
    let options = config.model_options().unwrap();

    assert_eq!(options.len(), 2);
    assert_eq!(options[0].model, "deepseek-chat");
    assert!(options[0].default_for_provider);
    assert_eq!(options[0].config.model, "deepseek-chat");
    assert_eq!(options[1].model, "deepseek-reasoner");
    assert_eq!(options[1].config.model, "deepseek-reasoner");
    assert_eq!(options[1].config.base_url, "https://deepseek.example/v1");
}

#[test]
fn model_reasoning_profile_sets_the_default_request_effort() {
    let config: FileConfig = toml::from_str(
        r#"default_provider = "openai"
[providers.openai]
api_format = "openai_responses"
base_url = "https://openai.example/v1"
api_key = "openai-key"
[[providers.openai.models]]
id = "reasoning-model"
reasoning = { adapter = "openai_responses", efforts = ["low", "high"], default = "high" }
"#,
    )
    .unwrap();
    let option = config.default_model_option().unwrap();

    assert_eq!(
        option.reasoning_efforts,
        vec![ReasoningEffort::Low, ReasoningEffort::High]
    );
    assert_eq!(option.config.reasoning_effort, Some(ReasoningEffort::High));
    assert_eq!(
        option.config.reasoning_adapter,
        Some(ReasoningAdapter::OpenaiResponses)
    );
}

#[test]
fn anthropic_reasoning_profile_accepts_max_effort() {
    let config: FileConfig = toml::from_str(
        r#"default_provider = "anthropic"
[providers.anthropic]
api_format = "anthropic_messages"
base_url = "https://anthropic.example"
api_key = "test-key"
[[providers.anthropic.models]]
id = "claude-test"
reasoning = { adapter = "anthropic_messages", efforts = ["low", "high", "max"], default = "max", extra = { thinking = { type = "enabled" } } }
"#,
    )
    .unwrap();

    let option = config.default_model_option().unwrap();

    assert_eq!(
        option.reasoning_efforts,
        vec![
            ReasoningEffort::Low,
            ReasoningEffort::High,
            ReasoningEffort::Max
        ]
    );
    assert_eq!(option.config.reasoning_effort, Some(ReasoningEffort::Max));
    assert_eq!(
        option.config.reasoning_adapter,
        Some(ReasoningAdapter::AnthropicMessages)
    );
}

#[test]
fn reasoning_extra_cannot_replace_standard_request_fields() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    fs::write(
        &path,
        r#"default_provider = "openai"
[providers.openai]
api_format = "openai_responses"
base_url = "https://openai.example/v1"
api_key = "openai-key"
[[providers.openai.models]]
id = "reasoning-model"
reasoning = { adapter = "openai_responses", efforts = ["low", "high"], default = "high", extra = { stream = false } }
"#,
    )
    .unwrap();
    let error = FileConfig::load(&path, &path.with_extension("auth"))
        .unwrap_err()
        .to_string();

    assert!(error.contains("reasoning.extra") && error.contains("stream"));
}

#[test]
fn model_context_window_overrides_the_default_budget() {
    let config: FileConfig = toml::from_str(
        r#"default_provider = "test"
context_tokens = 128000
max_output_tokens = 32000
[providers.test]
base_url = "https://test.example/v1"
api_key = "test-key"
[[providers.test.models]]
id = "small-window"
context_tokens = 64000
[[providers.test.models]]
id = "default-window"
[[providers.test.models]]
id = "separate-input-window"
input_context_tokens = 224000
"#,
    )
    .unwrap();
    let options = config.model_options().unwrap();

    assert_eq!(options[0].context_tokens, 64_000);
    assert_eq!(options[0].input_context_tokens, 32_000);
    assert_eq!(options[1].context_tokens, 128_000);
    assert_eq!(options[1].input_context_tokens, 96_000);
    assert_eq!(options[2].input_context_tokens, 224_000);
    assert_eq!(options[2].context_tokens, 256_000);
    assert_eq!(
        config
            .agent_config_for(options[0].context_tokens)
            .context_tokens,
        64_000
    );
}

#[test]
fn saved_provider_credentials_are_injected_as_references_and_cannot_mix_with_keys() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    let content = r#"default_provider = "test"
[providers.test]
api_format = "anthropic_messages"
base_url = "https://api.anthropic.com/v1"
credential = "anthropic"
[[providers.test.models]]
id = "test-model"
"#;

    fs::write(&path, content).unwrap();
    let auth_directory = directory.path().join("auth");

    let config = FileConfig::load(&path, &auth_directory).unwrap();
    let models = config.model_options().unwrap();

    assert!(models[0].config.api_key.is_none());
    assert_eq!(
        models[0].config.credential.as_ref().unwrap().directory,
        auth_directory
    );
    assert_eq!(
        models[0].config.credential.as_ref().unwrap().name,
        "anthropic"
    );

    fs::write(
        &path,
        content.replace("credential =", "api_key = \"secret\"\ncredential ="),
    )
    .unwrap();

    assert!(FileConfig::load(&path, &auth_directory).is_err());
}

#[test]
fn account_configuration_example_connects_both_provider_protocols() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    let readme = include_str!("../../../orbit-auth/README.md");
    let example = readme
        .split("```toml\n")
        .nth(1)
        .unwrap()
        .split("```")
        .next()
        .unwrap();

    fs::write(&path, example).unwrap();

    let config = FileConfig::load(&path, &directory.path().join("auth")).unwrap();
    let models = config.model_options().unwrap();

    assert_eq!(models.len(), 2);
    assert!(
        models
            .iter()
            .any(|model| model.config.api_format == ApiFormat::GoogleCodeAssist)
    );
    assert!(
        models
            .iter()
            .any(|model| model.config.api_format == ApiFormat::ChatgptCodex)
    );
    for model in models {
        assert!(model.config.credential.is_some());
        ModelClient::new(model.config).unwrap();
    }
}

#[test]
fn example_configuration_uses_valid_model_capability_profiles() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    fs::write(&path, include_str!("../../../../config.example.toml")).unwrap();
    let config = FileConfig::load(&path, &path.with_extension("auth")).unwrap();
    let models = config.model_options().unwrap();

    assert_eq!(models.len(), 3);
    assert_eq!(config.default_provider, "deepseek");
    assert!(models.iter().any(|model| model.supports_vision));
    assert!(
        models
            .iter()
            .any(|model| !model.reasoning_efforts.is_empty())
    );
}

#[test]
fn models_must_be_present_and_unique_with_a_real_default_provider() {
    let duplicate = r#"default_provider = "other"
[providers.other]
base_url = "https://other.example/v1"
api_key = "other-key"
[[providers.other.models]]
id = "other-model"
[[providers.other.models]]
id = "other-model"
"#;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("config.toml");
    fs::write(&path, duplicate).unwrap();

    assert!(FileConfig::load(&path, &path.with_extension("auth")).is_err());

    fs::write(
        &path,
        r#"default_provider = "missing"
[providers.other]
base_url = "https://other.example/v1"
api_key = "other-key"
[[providers.other.models]]
id = "other-model"
"#,
    )
    .unwrap();

    assert!(FileConfig::load(&path, &path.with_extension("auth")).is_err());

    fs::write(
        &path,
        r#"default_provider = "other"
[providers.other]
base_url = "https://other.example/v1"
api_key = "other-key"
[[providers.other.models]]
id = "other-model"
reasoning = { adapter = "openai_chat", efforts = ["high", "high"], default = "high" }
"#,
    )
    .unwrap();

    assert!(FileConfig::load(&path, &path.with_extension("auth")).is_err());
}

#[test]
fn config_parses_permissions() {
    let input = r#"
default_provider = "test"
context_tokens = 100000
max_output_tokens = 4000
max_turns = 10
max_tool_calls = 50

[permissions]
read = ["/tmp/data"]
read_write = ["/tmp/out"]

[providers.test]
base_url = "https://test.example/v1"
api_key = "test-key"
[[providers.test.models]]
id = "model-1"
"#;
    let config: FileConfig = toml::from_str(input).unwrap();

    assert_eq!(config.permissions.read, vec![PathBuf::from("/tmp/data")]);
    assert_eq!(
        config.permissions.read_write,
        vec![PathBuf::from("/tmp/out")]
    );
}

#[test]
fn deepseek_flash_vision_defaults_apply_to_existing_config_without_overriding_user_choice() {
    for (setting, expected) in [
        ("", true),
        ("supports_vision = false", false),
        ("supports_vision = true", true),
    ] {
        let config: FileConfig = toml::from_str(&format!(
            r#"
default_provider = "deepseek"
[providers.deepseek]
base_url = "https://api.deepseek.com/v1"
api_key = "test"
[[providers.deepseek.models]]
id = "deepseek-flash"
{setting}
"#
        ))
        .unwrap();
        let options = config.model_options().unwrap();
        assert_eq!(options[0].supports_vision, expected);
        assert_eq!(options[0].config.supports_vision, expected);
    }
}

#[test]
fn discovered_output_limits_do_not_raise_the_configured_request_budget() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = FileConfig::parse(
        r#"default_provider = "test"
max_output_tokens = 32000
[providers.test]
api_format = "openai_responses"
base_url = "https://api.openai.com/v1"
models = [{id = "fixture"}]
"#,
        directory.path(),
    )
    .unwrap();
    let provider = config.providers.get_mut("test").unwrap();
    provider.configured_models = Some(provider.models.clone());
    provider.models[0].max_output_tokens = Some(64000);
    assert_eq!(config.model_options().unwrap()[0].max_output_tokens, 32000);
    let provider = config.providers.get_mut("test").unwrap();
    provider.configured_models.as_mut().unwrap()[0].max_output_tokens = Some(48000);
    assert_eq!(config.model_options().unwrap()[0].max_output_tokens, 48000);
    config.providers.get_mut("test").unwrap().models[0].max_output_tokens = Some(16000);
    assert_eq!(config.model_options().unwrap()[0].max_output_tokens, 16000);
}
