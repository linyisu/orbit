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
        orbit_config::directories::config(),
        orbit_config::directories::home(),
    )
}

#[derive(Clone, Debug)]
pub struct Local {
    pub config: PathBuf,
    pub auth: PathBuf,
    pub model: Option<String>,
}

impl Local {
    /// Prepare a connection from declarations and caches without waiting on model discovery.
    pub async fn snapshot(&self) -> Result<Configuration> {
        let content = std::fs::read_to_string(&self.config)
            .with_context(|| format!("无法读取本地配置 {}", self.config.display()))?;
        let mut parsed = orbit_config::config::FileConfig::parse(&content, &self.auth)?;
        orbit_config::models::updater::load_cached(&mut parsed).await;
        self.resolved_snapshot(&content, &parsed)
            .await
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
        Ok((
            self.resolved_snapshot(&content, &parsed)
                .await?,
            notices,
        ))
    }

    /// Export resolved models and referenced credentials without modifying the local config.
    async fn resolved_snapshot(
        &self,
        content: &str,
        parsed: &orbit_config::config::FileConfig,
    ) -> Result<Configuration> {
        let mut document: toml::Value = toml::from_str(content)?;
        let store = AuthStore::new(self.auth.clone())?;
        let mut credentials = BTreeMap::new();
        for (name, provider) in &parsed.providers {
            // Transport an explicit resolved snapshot, without changing the source file.
            let mut snapshot = provider.spec().clone();
            snapshot.set_models(provider.models());
            for model in provider.models() {
                if let Some(settings) = snapshot
                    .model_overrides
                    .get_mut(&model.id)
                {
                    let requested = provider
                        .spec()
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

        Ok(Configuration {
            content: toml::to_string(&document)?,
            credentials,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_config::test_support::{TestConfig, TestProvider};

    #[tokio::test]
    async fn remote_connection_snapshots_do_not_wait_for_model_discovery() {
        let root = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let config = root.path().join("config.toml");
        TestConfig::new()
            .provider(
                TestProvider::new("slow")
                    .endpoint(format!("http://{}/v1", listener.local_addr().unwrap()))
                    .models(["declared-model"])
                    .refresh(true),
            )
            .write(&config)
            .unwrap();
        let local = Local {
            config: config.clone(),
            auth: root.path().join("auth"),
            model: None,
        };
        for target in [
            crate::transport::Target::Ssh {
                host: "synthetic-host".into(),
                options: Default::default(),
            },
            crate::transport::Target::Wsl {
                distribution: "Synthetic Linux".into(),
                user: None,
            },
        ] {
            let connection = crate::transport::Connection {
                target,
                local_configuration: Some(local.clone()),
                workspace: "synthetic-workspace".into(),
                executable: "unused-server".into(),
                config: config.clone(),
                database: None,
                auth_dir: None,
                allow_mcp_start: false,
                unattended: true,
                interactive: None,
            };
            let snapshot = tokio::time::timeout(
                std::time::Duration::from_millis(250),
                connection.configuration(),
            )
            .await
            .expect("connection preparation must stay offline")
            .unwrap()
            .unwrap();
            let parsed =
                orbit_config::config::FileConfig::parse(&snapshot.content, &local.auth).unwrap();
            assert_eq!(parsed.providers["slow"].models()[0].id, "declared-model");
            assert!(!parsed.providers["slow"].auto_refresh_models);
        }
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
    }
}
