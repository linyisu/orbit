//! Provider model discovery and a bounded, credential-scoped metadata cache.
use std::path::Path;
#[cfg(test)]
use std::time::SystemTime;

use anyhow::{Context, Result};
use futures_util::{StreamExt, stream};
use orbit_model::client::ApiFormat;
use serde_json::Value;

use crate::config::{FileConfig, ModelProfile, ProviderConfig};
mod cache;
#[cfg(test)]
use serde_json::json;

#[derive(Clone, Copy)]
pub(crate) enum RefreshState {
    Skipped,
    Cached(orbit_model::catalog::CatalogCoverage),
    Fetched(orbit_model::catalog::CatalogCoverage),
    Fallback,
    Unavailable,
}

pub(crate) struct ConnectionRefresh {
    pub name: String,
    pub state: RefreshState,
    pub diagnostics: Vec<String>,
}

pub(crate) async fn refresh_connections(
    config: &mut FileConfig,
    force: bool,
    bypass_cache: bool,
    network: bool,
) -> Vec<ConnectionRefresh> {
    let Some(directory) = config.auth_directory.as_ref() else {
        return Vec::new();
    };
    let mut jobs = Vec::new();
    for (name, provider) in &mut config.providers {
        if supported(provider.api_format, &provider.base_url) {
            let allow_network = network && (force || provider.auto_refresh_models);
            jobs.push(refresh_connection(
                name,
                provider,
                directory,
                bypass_cache,
                allow_network,
            ));
        }
    }

    stream::iter(jobs)
        .buffer_unordered(4)
        .collect()
        .await
}

async fn refresh_connection(
    name: &str,
    provider: &mut ProviderConfig,
    directory: &Path,
    force: bool,
    allow_network: bool,
) -> ConnectionRefresh {
    let mut diagnostics = Vec::new();
    let state = match refresh_provider(
        name,
        provider,
        directory,
        force,
        allow_network,
        &mut diagnostics,
    )
    .await
    {
        Ok(state) => state,
        Err(error) => {
            diagnostics.push(format!("{name}：模型目录更新失败：{error}"));
            RefreshState::Unavailable
        }
    };
    ConnectionRefresh {
        name: name.to_owned(),
        state,
        diagnostics,
    }
}

async fn refresh_provider(
    name: &str,
    provider: &mut ProviderConfig,
    directory: &Path,
    force: bool,
    allow_network: bool,
    diagnostics: &mut Vec<String>,
) -> Result<RefreshState> {
    provider.discard_discovery();
    let reference = provider
        .credential
        .as_ref()
        .map(|name| orbit_auth::CredentialRef {
            directory: directory.to_path_buf(),
            name: name.clone(),
        });
    let cache = cache::CatalogCache::new(name, provider, directory)?;
    let cached = cache.load();
    let fresh = cache.fresh();
    let (catalog, state) = if let Some(cached) = cached
        .as_ref()
        .filter(|_| !force && (fresh || !allow_network))
    {
        (cached.clone(), RefreshState::Cached(cached.coverage))
    } else if !allow_network {
        return Ok(RefreshState::Skipped);
    } else {
        let discovered = async {
            let payload = if provider
                .api_format
                .requires_saved_credential()
            {
                orbit_model::discovery::discover(
                    reference
                        .as_ref()
                        .context("套餐账号需要已保存的认证")?,
                    provider.api_format,
                    &provider.base_url,
                )
                .await?
            } else if let Some(reference) = &reference {
                orbit_model::discovery::discover_saved_api_catalog(
                    reference,
                    &provider.base_url,
                    provider.api_format,
                )
                .await?
            } else {
                orbit_model::discovery::discover_api_catalog(
                    &provider.base_url,
                    provider.api_format,
                    &provider.api_key,
                )
                .await?
            };

            orbit_model::catalog::parse(provider.api_format, &payload)
        }
        .await;

        match discovered {
            Ok(mut payload) => {
                payload.models = payload.resolve(
                    cached
                        .as_ref()
                        .map(|catalog| catalog.models.clone())
                        .unwrap_or_default(),
                );
                let write = cache.write(&payload);
                if let Err(error) = write {
                    diagnostics.push(format!("{name}：模型缓存写入失败：{error}"));
                }
                let state = RefreshState::Fetched(payload.coverage);
                (payload, state)
            }

            Err(error) => {
                diagnostics.push(format!(
                    "{name}：模型刷新失败，保留身份匹配的缓存或显式配置：{error}"
                ));
                match cached {
                    Some(payload) => (payload, RefreshState::Fallback),
                    None => return Ok(RefreshState::Unavailable),
                }
            }
        }
    };
    provider.set_discovered(catalog.models);
    Ok(state)
}

/// Apply availability according to the provider's response contract, not its hostname.
#[cfg(test)]
fn merge_catalog(provider: &mut ProviderConfig, catalog: orbit_model::catalog::ModelCatalog) {
    let models = catalog.resolve(
        provider
            .discovered
            .clone()
            .unwrap_or_default(),
    );
    provider.set_discovered(models);
}

#[cfg(test)]
fn merge_discovered_models(provider: &mut ProviderConfig, models: Vec<ModelProfile>) {
    merge_catalog(
        provider,
        orbit_model::catalog::ModelCatalog {
            models,
            unavailable: Vec::new(),
            coverage: orbit_model::catalog::CatalogCoverage::Complete,
        },
    );
}

fn supported(format: ApiFormat, base: &str) -> bool {
    orbit_model::discovery::supports(format, base)
}

#[cfg(test)]
use cache::{fingerprint, read_cache};

/// Store a just-verified complete catalog without a second discovery request.
pub(crate) fn remember(
    name: &str,
    provider: &ProviderConfig,
    directory: &Path,
    models: Vec<ModelProfile>,
) -> Result<()> {
    let catalog = orbit_model::catalog::ModelCatalog {
        models,
        unavailable: Vec::new(),
        coverage: orbit_model::catalog::CatalogCoverage::Complete,
    };
    catalog.validate(provider.api_format)?;
    cache::CatalogCache::new(name, provider, directory)?.write(&catalog)
}

/// Decode provider metadata; configuration validation remains at the declaration boundary.
pub fn normalize(format: ApiFormat, payload: &Value) -> Result<(Vec<ModelProfile>, Vec<String>)> {
    orbit_model::catalog::normalize(format, payload)
}

pub use orbit_model::catalog::{default_reasoning, default_vision};

#[cfg(test)]
mod tests;
