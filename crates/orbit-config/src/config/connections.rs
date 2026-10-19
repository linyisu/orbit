//! Shared connection persistence for official and custom providers.
pub mod edit;
pub mod mcp;
pub mod models;
use std::io::Write;

use anyhow::{Context, Result, ensure};
use orbit_model::client::ApiFormat;

use crate::{
    config::{ModelProfile, ModelSelection, ProviderSpec},
    models::ModelOption,
};
pub mod document;

const MAX_CONNECTION_NAME_BYTES: usize = 128;

/// A committed connection plus non-fatal catalog refresh diagnostics.
#[derive(Debug)]
pub struct SaveOutcome {
    pub models: Vec<ModelOption>,
    pub diagnostics: Vec<String>,
}

/// A validated replacement that retains the source used for conflict detection.
pub struct PreparedConnectionChange {
    path: std::path::PathBuf,
    original: String,
    updated: String,
    models: Vec<ModelOption>,
}

impl PreparedConnectionChange {
    fn validate(
        path: &std::path::Path,
        original: String,
        updated: String,
        auth: &std::path::Path,
    ) -> Result<Self> {
        let parsed = crate::config::FileConfig::parse(&updated, auth)?;
        let models = parsed.model_options()?;
        for option in &models {
            orbit_model::client::ModelClient::new(option.config.clone())?;
        }
        Ok(Self {
            path: path.to_owned(),
            original,
            updated,
            models,
        })
    }

    /// Commit only while the reviewed source is current, returning its validated model choices.
    pub fn commit(self) -> Result<Vec<ModelOption>> {
        if self.original == self.updated {
            ensure!(
                read_config(&self.path)? == self.original,
                "配置已被其他程序修改，请重试"
            );
        } else {
            persist(&self.path, &self.original, &self.updated)?;
        }
        Ok(self.models)
    }
}

fn valid_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.len() <= MAX_CONNECTION_NAME_BYTES
        && !name.chars().any(char::is_control)
}

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
    prepare_account(path, auth_directory, credential, spec)?.commit()
}

fn prepare_account(
    path: &std::path::Path,
    auth_directory: &std::path::Path,
    credential: &str,
    spec: &ProviderSpec,
) -> Result<PreparedConnectionChange> {
    let original = read_config(path)?;
    let config = crate::config::FileConfig::parse(&original, auth_directory)?;
    if config
        .providers
        .values()
        .any(|provider| {
            provider.credential.as_deref() == Some(credential)
                && provider.base_url.trim_end_matches('/') == spec.base_url.trim_end_matches('/')
                && provider.api_format == spec.api_format
        })
    {
        return PreparedConnectionChange::validate(
            path,
            original.clone(),
            original,
            auth_directory,
        );
    }
    let mut name = format!("{credential}-account");
    let mut suffix = 2;
    while config.providers.contains_key(&name) {
        name = format!("{credential}-account-{suffix}");
        suffix += 1;
    }
    prepare_provider(path, auth_directory, &name, spec)
}

pub(super) fn read_config(path: &std::path::Path) -> Result<String> {
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
) -> Result<PreparedConnectionChange> {
    ensure!(valid_name(name), "连接名称无效");
    let original = read_config(path)?;
    let config: crate::config::FileConfig = toml::from_str(&original)?;
    ensure!(
        !config.providers.contains_key(name),
        "连接名称已存在，请使用其他名称"
    );

    let mut document = original.parse::<toml_edit::DocumentMut>()?;
    document["providers"][name] = document::provider(provider)?;
    let updated = document.to_string();
    PreparedConnectionChange::validate(path, original, updated, auth_directory)
}

pub fn save_provider(
    path: &std::path::Path,
    auth_directory: &std::path::Path,
    name: &str,
    provider: &ProviderSpec,
) -> Result<Vec<ModelOption>> {
    prepare_provider(path, auth_directory, name, provider)?.commit()
}

pub(super) fn persist(path: &std::path::Path, original: &str, updated: &str) -> Result<()> {
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
    temporary
        .persist(path)
        .context("无法保存配置")?;

    Ok(())
}

pub async fn save_account(
    path: &std::path::Path,
    auth_directory: &std::path::Path,
    provider: orbit_auth::OAuthProvider,
    credential: &str,
) -> Result<SaveOutcome> {
    let format = ApiFormat::for_oauth(provider);
    let endpoint = provider.endpoint();

    let models = append_profile(path, auth_directory, credential, format, endpoint)?;
    Ok(refresh_saved(path, auth_directory, models).await)
}

/// Rebind saved account connections after explicit reauthorization changes the issued route.
/// Catalog failures remain diagnostics after a successful configuration commit.
pub async fn refresh_account(
    path: &std::path::Path,
    auth_directory: &std::path::Path,
    credential: &str,
) -> Result<SaveOutcome> {
    let models = rebind_account(path, auth_directory, credential)?;
    Ok(refresh_saved(path, auth_directory, models).await)
}

fn rebind_account(
    path: &std::path::Path,
    auth_directory: &std::path::Path,
    credential: &str,
) -> Result<Vec<ModelOption>> {
    let info = orbit_auth::AuthStore::new(auth_directory)?
        .list()?
        .into_iter()
        .find(|entry| entry.name == credential)
        .context("账号尚未登录")?;
    let provider = info
        .kind
        .oauth_provider()
        .context("该账号不是 OAuth 连接")?;
    let format = ApiFormat::for_oauth(provider);
    let original = read_config(path)?;
    let config = crate::config::FileConfig::parse(&original, auth_directory)?;
    let mut document = original.parse::<toml_edit::DocumentMut>()?;
    let mut matched = false;
    for (name, connection) in &config.providers {
        if connection.credential.as_deref() == Some(credential) {
            matched = true;
            ensure!(connection.api_format == format, "账号协议与现有连接不匹配");
            document["providers"][name]["base_url"] = toml_edit::value(info.endpoint.clone());
        }
    }
    ensure!(matched, "账号对应的连接已不存在");
    PreparedConnectionChange::validate(path, original, document.to_string(), auth_directory)?
        .commit()
}

/// Refresh a committed configuration without turning catalog failure into a failed save.
pub async fn refresh_saved(
    path: &std::path::Path,
    auth_directory: &std::path::Path,
    models: Vec<ModelOption>,
) -> SaveOutcome {
    let mut config = match crate::config::FileConfig::load(path, auth_directory) {
        Ok(config) => config,
        Err(error) => {
            return SaveOutcome {
                models,
                diagnostics: vec![format!("{error:#}")],
            };
        }
    };
    let mut diagnostics = crate::models::updater::refresh(&mut config, true)
        .await
        .diagnostics;
    let models = match config.model_options() {
        Ok(updated) => updated,
        Err(error) => {
            diagnostics.push(format!("{error:#}"));
            models
        }
    };
    SaveOutcome {
        models,
        diagnostics,
    }
}

/// Commit a verified key and cache its live catalog without pinning discovered IDs in config.
/// Cache persistence failures are diagnostics after the credential/configuration commit.
pub async fn save_api_key(
    path: &std::path::Path,
    auth_directory: &std::path::Path,
    provider: orbit_auth::ApiKeyProvider,
    secret: String,
    ids: Vec<String>,
) -> Result<SaveOutcome> {
    let catalog = orbit_model::catalog::ModelCatalog {
        models: ids
            .into_iter()
            .map(ModelProfile::new)
            .collect(),
        unavailable: Vec::new(),
        coverage: orbit_model::catalog::CatalogCoverage::Complete,
    };
    save_api_catalog(path, auth_directory, provider, secret, catalog).await
}

/// Commit a verified key with the full advertised catalog, including model capabilities.
pub async fn save_api_catalog(
    path: &std::path::Path,
    auth_directory: &std::path::Path,
    provider: orbit_auth::ApiKeyProvider,
    secret: String,
    catalog: orbit_model::catalog::ModelCatalog,
) -> Result<SaveOutcome> {
    catalog.validate(ApiFormat::OpenaiChatCompletions)?;
    let models = catalog.models;
    let existing = crate::config::FileConfig::parse(&read_config(path)?, auth_directory)?;
    let store = orbit_auth::AuthStore::new(auth_directory.to_path_buf())?;
    let credentials = store.list()?;
    let mut credential = provider.name().to_owned();
    let mut suffix = 2;
    while credentials
        .iter()
        .any(|entry| entry.name == credential)
        || existing
            .providers
            .values()
            .any(|entry| entry.credential.as_deref() == Some(&credential))
    {
        credential = format!("{}-{suffix}", provider.name());
        suffix += 1;
    }
    let mut spec = ProviderSpec::new(
        ApiFormat::OpenaiChatCompletions,
        provider.base_url(),
        ModelSelection::auto(),
    );
    spec.credential = Some(credential.clone());
    let prepared = prepare_account(path, auth_directory, &credential, &spec)?;
    let mut config = crate::config::FileConfig::parse(&prepared.updated, auth_directory)?;
    for connection in config.providers.values_mut() {
        if connection.credential.as_deref() == Some(&credential)
            && connection
                .spec
                .base_url
                .trim_end_matches('/')
                == provider
                    .base_url()
                    .trim_end_matches('/')
        {
            connection.set_discovered(models.clone());
        }
    }
    crate::models::model_metadata::update(&mut config, false, false).await;
    let verified_models = config.model_options()?;
    store
        .register_api_key(&credential, secret, provider.base_url(), || {
            prepared.commit()
        })
        .await
        .map_err(anyhow::Error::new)?;
    let mut diagnostics = crate::models::updater::load_cached(&mut config)
        .await
        .diagnostics;
    for (name, connection) in &mut config.providers {
        if connection.credential.as_deref() == Some(&credential)
            && connection
                .base_url
                .trim_end_matches('/')
                == provider
                    .base_url()
                    .trim_end_matches('/')
        {
            connection.set_discovered(models.clone());
            if let Err(error) = crate::models::account_models::remember(
                name,
                connection,
                auth_directory,
                models.clone(),
            ) {
                diagnostics.push(format!("{name}：连接已保存，模型缓存写入失败：{error}"));
            }
        }
    }
    diagnostics.extend(
        crate::models::model_metadata::update(&mut config, false, false)
            .await
            .error,
    );
    let models = match config.model_options() {
        Ok(models) => models,
        Err(error) => {
            diagnostics.push(format!("连接已保存，模型投影更新失败：{error:#}"));
            verified_models
        }
    };
    Ok(SaveOutcome {
        models,
        diagnostics,
    })
}

#[cfg(test)]
mod tests;
