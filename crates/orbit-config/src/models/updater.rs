//! Unified refresh for public specifications and credential-scoped account catalogs.
use crate::config::FileConfig;
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{Arc, LazyLock, Mutex},
    time::Instant,
};

#[derive(Clone, Debug, Default)]
pub struct UpdateReport {
    pub notices: Vec<String>,
    pub diagnostics: Vec<String>,
}
type Gate = Arc<tokio::sync::Mutex<Option<Instant>>>;
type WeakGate = std::sync::Weak<tokio::sync::Mutex<Option<Instant>>>;
static GATES: LazyLock<Mutex<BTreeMap<PathBuf, WeakGate>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

/// Resolve declarations and credential-bound caches without network access or
/// waiting for an in-flight refresh. Opening a session must stay local.
pub async fn load_cached(config: &mut FileConfig) -> UpdateReport {
    for provider in config.providers.values_mut() {
        provider.resolve();
    }
    let mut report = UpdateReport::default();
    for refreshed in super::account_models::refresh_connections(config, false, false, false).await {
        report
            .diagnostics
            .extend(refreshed.diagnostics);
    }
    report.diagnostics.extend(
        super::model_metadata::update(config, false, false)
            .await
            .error,
    );
    report.diagnostics.sort();
    report
}

pub async fn refresh(config: &mut FileConfig, force: bool) -> UpdateReport {
    let Some(directory) = config.auth_directory.clone() else {
        return UpdateReport::default();
    };
    let started = Instant::now();
    let gate: Gate = {
        let mut gates = GATES.lock().unwrap();
        gates.retain(|_, gate| gate.strong_count() > 0);
        match gates
            .get(&directory)
            .and_then(std::sync::Weak::upgrade)
        {
            Some(gate) => gate,
            None => {
                let gate = Arc::new(tokio::sync::Mutex::new(None));
                gates.insert(directory, Arc::downgrade(&gate));
                gate
            }
        }
    };
    let mut completed = gate.lock().await;
    let force_download = force && completed.is_none_or(|time| time < started);
    // Rebuild from declarations and source metadata, never from last refresh's supplemented values.
    for provider in config.providers.values_mut() {
        provider.resolve();
    }
    let mut report = UpdateReport::default();
    for refreshed in
        super::account_models::refresh_connections(config, force, force_download, true).await
    {
        if force {
            use super::account_models::RefreshState;
            use orbit_model::catalog::CatalogCoverage;
            let message = match refreshed.state {
                RefreshState::Fetched(CatalogCoverage::Complete) => "模型目录已更新",
                RefreshState::Fetched(CatalogCoverage::Partial) => {
                    "已更新已知模型；服务仅提供部分目录"
                }
                RefreshState::Cached(CatalogCoverage::Complete) => "使用已缓存的完整目录",
                RefreshState::Cached(CatalogCoverage::Partial) => "使用已缓存的部分目录",
                RefreshState::Fallback => "刷新失败，沿用身份匹配的缓存",
                RefreshState::Unavailable => "更新未完成，仅保留显式配置",
                RefreshState::Skipped => "未请求远端目录",
            };
            report
                .notices
                .push(format!("{}：{message}", refreshed.name));
        }
        report
            .diagnostics
            .extend(refreshed.diagnostics);
    }
    let metadata = super::model_metadata::update(config, force_download, true).await;
    if force {
        report.notices.push(metadata.status);
    }
    report
        .diagnostics
        .extend(metadata.error);
    if force {
        *completed = Some(Instant::now());
    }
    report.notices.sort();
    report.diagnostics.sort();
    report
}

#[cfg(test)]
mod tests;
