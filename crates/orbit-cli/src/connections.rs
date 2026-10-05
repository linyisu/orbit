//! Shared connection persistence for official and custom providers.
pub(crate) mod edit;
use std::io::Write;

use anyhow::{Context, Result, ensure};
use orbit_model::client::ApiFormat;

use crate::tui::ModelOption;

pub(crate) fn append_profile(
    path: &std::path::Path,
    auth_directory: &std::path::Path,
    credential: &str,
    format: ApiFormat,
    endpoint: &str,
) -> Result<Vec<ModelOption>> {
    let catalog: serde_json::Value = serde_json::from_str(include_str!("tui/account_models.json"))?;
    let antigravity;
    let models = if credential == "antigravity" {
        antigravity = serde_json::to_value(crate::config::antigravity_profiles()?)?;
        &antigravity
    } else {
        catalog.get(credential).context("账号缺少内置模型目录")?
    };

    append_profile_models(path, auth_directory, credential, format, endpoint, models)
}

pub(crate) fn append_profile_models(
    path: &std::path::Path,
    auth_directory: &std::path::Path,
    credential: &str,
    format: ApiFormat,
    endpoint: &str,
    models: &serde_json::Value,
) -> Result<Vec<ModelOption>> {
    let original = read_config(path)?;
    let mut config: crate::config::FileConfig = toml::from_str(&original)?;
    config.auth_directory = Some(auth_directory.to_path_buf());
    config.merge_account_models()?;

    if config.providers.values().any(|provider| {
        provider.credential.as_deref() == Some(credential)
            && provider.base_url.trim_end_matches('/') == endpoint.trim_end_matches('/')
            && provider.api_format == format
    }) {
        return config.model_options();
    }

    let mut name = format!("{credential}-account");
    let mut suffix = 2;
    while config.providers.contains_key(&name) {
        name = format!("{credential}-account-{suffix}");
        suffix += 1;
    }

    let provider = serde_json::json!({
        "api_format": format,
        "base_url": endpoint,
        "credential": credential,
        "models": models
    });
    save_provider(path, auth_directory, &name, &provider)
}

fn read_config(path: &std::path::Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok("default_provider = \"\"\n[providers]\n".into())
        }
        Err(error) => Err(error.into()),
    }
}

/// Build and validate the complete configuration before writing any credentials.
pub(crate) fn prepare_provider(
    path: &std::path::Path,
    auth_directory: &std::path::Path,
    name: &str,
    provider: &serde_json::Value,
) -> Result<(String, String)> {
    ensure!(
        !name.trim().is_empty() && name.len() <= 128 && !name.chars().any(char::is_control),
        "连接名称无效"
    );
    let original = read_config(path)?;
    let config: crate::config::FileConfig = toml::from_str(&original)?;
    ensure!(
        !config.providers.contains_key(name),
        "连接名称已存在，请使用其他名称"
    );

    let addition = serde_json::json!({ "providers": { name: provider } });
    let base = if config.default_provider.is_empty() {
        // The empty bootstrap has no user settings. Existing empty files may
        // have settings, so retain them when setting the first provider.
        let mut value: toml::Value = toml::from_str(&original)?;
        value["default_provider"] = toml::Value::String(name.to_owned());
        toml::to_string_pretty(&value)?
    } else {
        original.clone()
    };
    let updated = format!(
        "{}\n\n{}",
        base.trim_end(),
        toml::to_string_pretty(&addition)?
    );
    let parsed = crate::config::FileConfig::parse(&updated, auth_directory)?;
    for option in parsed.model_options()? {
        orbit_model::client::ModelClient::new(option.config)?;
    }

    Ok((original, updated))
}

pub(crate) fn save_provider(
    path: &std::path::Path,
    auth_directory: &std::path::Path,
    name: &str,
    provider: &serde_json::Value,
) -> Result<Vec<ModelOption>> {
    let (original, updated) = prepare_provider(path, auth_directory, name, provider)?;
    persist(path, &original, &updated)?;
    crate::config::FileConfig::load(path, auth_directory)?.model_options()
}

fn persist(path: &std::path::Path, original: &str, updated: &str) -> Result<()> {
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    if let Ok(metadata) = std::fs::metadata(path) {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())?;
    }

    temporary.write_all(updated.as_bytes())?;
    temporary.as_file().sync_all()?;
    ensure!(
        read_config(path)? == original,
        "配置已被其他程序修改，请重试"
    );
    temporary.persist(path).context("无法保存连接配置")?;

    Ok(())
}

pub(crate) async fn save_account(
    path: &std::path::Path,
    auth_directory: &std::path::Path,
    provider: orbit_auth::OAuthProvider,
) -> Result<Vec<ModelOption>> {
    use orbit_auth::OAuthProvider;
    let format = match provider {
        OAuthProvider::Antigravity | OAuthProvider::Google => ApiFormat::GoogleCodeAssist,
        OAuthProvider::Chatgpt => ApiFormat::ChatgptCodex,
        OAuthProvider::Anthropic => ApiFormat::AnthropicMessages,
    };
    let endpoint = match provider {
        OAuthProvider::Antigravity => "https://daily-cloudcode-pa.googleapis.com/",
        OAuthProvider::Google => "https://cloudcode-pa.googleapis.com/",
        OAuthProvider::Chatgpt => "https://chatgpt.com/backend-api/codex/",
        OAuthProvider::Anthropic => unreachable!(),
    };

    append_profile(path, auth_directory, provider.name(), format, endpoint)?;
    let mut config = crate::config::FileConfig::load(path, auth_directory)?;
    let diagnostics = crate::account_models::refresh(&mut config, true).await;
    if !diagnostics.is_empty() {
        anyhow::bail!("{}", diagnostics.join("\n"));
    }
    config.model_options()
}

pub(crate) async fn save_api_key(
    path: &std::path::Path,
    auth_directory: &std::path::Path,
    provider: orbit_auth::ApiKeyProvider,
    secret: String,
    ids: Vec<String>,
) -> Result<Vec<ModelOption>> {
    let store = orbit_auth::AuthStore::new(auth_directory.to_path_buf())?;
    store
        .set_api_key(provider.name(), secret, provider.base_url())
        .await?;
    let models = ids
        .into_iter()
        .map(|id| serde_json::json!({ "id": id }))
        .collect::<Vec<_>>();
    append_profile_models(
        path,
        auth_directory,
        provider.name(),
        ApiFormat::OpenaiChatCompletions,
        provider.base_url(),
        &serde_json::Value::Array(models),
    )
}

#[cfg(test)]
mod tests;
