//! Shared connection persistence for official and custom providers.
pub mod edit;
pub mod mcp;
use std::io::Write;

use anyhow::{Context, Result, ensure};
use orbit_model::client::ApiFormat;

use crate::{
    config::{ModelProfile, ModelSelection, ProviderSpec},
    models::ModelOption,
};
pub mod document;

pub fn append_profile(
    path: &std::path::Path,
    auth_directory: &std::path::Path,
    credential: &str,
    format: ApiFormat,
    endpoint: &str,
) -> Result<Vec<ModelOption>> {
    let mut provider = ProviderSpec::new(format, endpoint, ModelSelection::auto());
    provider.credential = Some(credential.into());
    append_account(path, auth_directory, credential, &provider)
}

pub fn append_profile_models(
    path: &std::path::Path,
    auth_directory: &std::path::Path,
    credential: &str,
    format: ApiFormat,
    endpoint: &str,
    models: &[ModelProfile],
) -> Result<Vec<ModelOption>> {
    let mut provider = ProviderSpec::new(format, endpoint, ModelSelection::Explicit(Vec::new()));
    provider.credential = Some(credential.into());
    provider.set_models(models);
    append_account(path, auth_directory, credential, &provider)
}

fn append_account(
    path: &std::path::Path,
    auth_directory: &std::path::Path,
    credential: &str,
    spec: &ProviderSpec,
) -> Result<Vec<ModelOption>> {
    let original = read_config(path)?;
    let mut config: crate::config::FileConfig = toml::from_str(&original)?;
    config.auth_directory = Some(auth_directory.to_path_buf());

    if config.providers.values().any(|provider| {
        provider.credential.as_deref() == Some(credential)
            && provider.base_url.trim_end_matches('/') == spec.base_url.trim_end_matches('/')
            && provider.api_format == spec.api_format
    }) {
        return config.model_options();
    }

    let mut name = format!("{credential}-account");
    let mut suffix = 2;
    while config.providers.contains_key(&name) {
        name = format!("{credential}-account-{suffix}");
        suffix += 1;
    }

    save_provider(path, auth_directory, &name, spec)
}

fn read_config(path: &std::path::Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok("[providers]\n".into()),
        Err(error) => Err(error.into()),
    }
}

/// Build and validate the complete configuration before writing any credentials.
pub fn prepare_provider(
    path: &std::path::Path,
    auth_directory: &std::path::Path,
    name: &str,
    provider: &ProviderSpec,
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

    let mut document = original.parse::<toml_edit::DocumentMut>()?;
    document["providers"][name] = document::provider(provider)?;
    let updated = document.to_string();
    let parsed = crate::config::FileConfig::parse(&updated, auth_directory)?;
    for option in parsed.model_options()? {
        orbit_model::client::ModelClient::new(option.config)?;
    }

    Ok((original, updated))
}

pub fn save_provider(
    path: &std::path::Path,
    auth_directory: &std::path::Path,
    name: &str,
    provider: &ProviderSpec,
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

pub async fn save_account(
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
    let diagnostics = crate::models::updater::refresh(&mut config, true)
        .await
        .diagnostics;
    if !diagnostics.is_empty() {
        anyhow::bail!("{}", diagnostics.join("\n"));
    }
    config.model_options()
}

pub async fn save_api_key(
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
    let models = ids.into_iter().map(ModelProfile::new).collect::<Vec<_>>();
    append_profile_models(
        path,
        auth_directory,
        provider.name(),
        ApiFormat::OpenaiChatCompletions,
        provider.base_url(),
        &models,
    )
}

#[cfg(test)]
mod tests;
