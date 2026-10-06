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
        let selected = parsed
            .model_options()?
            .into_iter()
            .find(|model| self.model.as_deref() == Some(model.qualified_name().as_str()));

        for (name, provider) in &mut parsed.providers {
            if let Some(selected) = &selected
                && selected.provider == *name
                && let Some(index) = provider
                    .models
                    .iter()
                    .position(|model| model.id == selected.model)
            {
                provider.models.swap(0, index);
                document["default_provider"] = name.clone().into();
            }
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
