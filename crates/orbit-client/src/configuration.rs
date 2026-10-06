use anyhow::{Context, Result};
use orbit_auth::AuthStore;
pub use orbit_protocol::Configuration;
use std::{collections::BTreeMap, path::PathBuf};

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
        let content = std::fs::read_to_string(&self.config)
            .with_context(|| format!("无法读取本地配置 {}", self.config.display()))?;
        let mut parsed = orbit_config::config::FileConfig::parse(&content, &self.auth)?;
        orbit_config::models::account_models::refresh(&mut parsed, force).await;
        let mut document: toml::Value = toml::from_str(&content)?;
        let store = AuthStore::new(self.auth.clone())?;
        let mut credentials = BTreeMap::new();
        document
            .as_table_mut()
            .context("invalid configuration")?
            .remove("default_provider");
        for (name, provider) in &parsed.providers {
            let table = document["providers"][name]
                .as_table_mut()
                .context("invalid provider table")?;
            table.insert("models".into(), toml::Value::try_from(&provider.models)?);
            table.insert("managed_models".into(), false.into());
            table.insert("auto_refresh_models".into(), false.into());
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
