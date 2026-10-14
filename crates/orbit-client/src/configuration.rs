mod watch;
pub use watch::Watcher;

use anyhow::{Context, Result};
use orbit_auth::AuthStore;
pub use orbit_protocol::Configuration;
use std::{collections::BTreeMap, path::PathBuf};

/// Resolve the local credential directory consistently across every frontend.
pub fn auth_directory(override_path: Option<PathBuf>) -> Result<PathBuf> {
    orbit_auth::directory(
        override_path,
        std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from),
        std::env::var_os("HOME").map(PathBuf::from),
    )
}

#[derive(Clone, Debug)]
pub struct Local {
    pub config: PathBuf,
    pub auth: PathBuf,
    pub model: Option<String>,
}

impl Local {
    pub async fn snapshot(&self) -> Result<Configuration> {
        self.snapshot_with_refresh(false).await
    }

    pub async fn snapshot_with_refresh(&self, force: bool) -> Result<Configuration> {
        Ok(self
            .snapshot_with_report(force)
            .await?
            .0)
    }

    pub async fn snapshot_with_report(&self, force: bool) -> Result<(Configuration, Vec<String>)> {
        let content = std::fs::read_to_string(&self.config)
            .with_context(|| format!("无法读取本地配置 {}", self.config.display()))?;
        let mut parsed = orbit_config::config::FileConfig::parse(&content, &self.auth)?;
        let report = orbit_config::models::updater::refresh(&mut parsed, force).await;
        let notices = if force {
            report
                .notices
                .into_iter()
                .chain(report.diagnostics)
                .collect()
        } else {
            Vec::new()
        };
        let mut document: toml::Value = toml::from_str(&content)?;
        let store = AuthStore::new(self.auth.clone())?;
        let mut credentials = BTreeMap::new();
        for (name, provider) in &parsed.providers {
            // Transport an explicit resolved snapshot, without changing the source file.
            let mut snapshot = provider.spec.clone();
            snapshot.set_models(&provider.models);
            for model in &provider.models {
                if let Some(settings) = snapshot
                    .model_overrides
                    .get_mut(&model.id)
                {
                    let requested = provider
                        .spec
                        .model_overrides
                        .get(&model.id)
                        .and_then(|m| m.max_output_tokens)
                        .unwrap_or(parsed.max_output_tokens);
                    settings.max_output_tokens = Some(
                        requested.min(
                            model
                                .max_output_tokens
                                .unwrap_or(u32::MAX),
                        ),
                    );
                }
            }
            snapshot.auto_refresh_models = false;
            document["providers"][name] = toml::Value::try_from(&snapshot)?;
            if let Some(name) = &provider.credential
                && !credentials.contains_key(name)
            {
                credentials.insert(
                    name.clone(),
                    serde_json::to_value(store.snapshot(name).await?)?,
                );
            }
        }

        Ok((
            Configuration {
                content: toml::to_string(&document)?,
                credentials,
            },
            notices,
        ))
    }
}
