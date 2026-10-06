//! Client model preferences, scoped to the local configuration file.
use std::io::Write;

use anyhow::{Context, Result};
use orbit_config::{config::FileConfig, models::ModelOption};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::configuration::Local;

#[derive(Deserialize, Serialize)]
struct Selection {
    provider: String,
    model: String,
}

impl Local {
    pub async fn model_catalog(&self) -> Result<Vec<ModelOption>> {
        let mut config = FileConfig::load(&self.config, &self.auth)?;
        orbit_config::models::account_models::refresh(&mut config, false).await;
        config.model_options()
    }

    fn selection_path(&self) -> Result<std::path::PathBuf> {
        let config = self.config.canonicalize()?;
        let key: String = Sha256::digest(config.as_os_str().as_encoded_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(self
            .auth
            .parent()
            .context("认证目录缺少父目录")?
            .join("preferences")
            .join(format!("model-{key}.json")))
    }

    pub fn last_model(&self) -> Result<Option<String>> {
        let content = match std::fs::read(self.selection_path()?) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let selection: Selection =
            serde_json::from_slice(&content).context("无法读取上次选择的模型")?;
        Ok(Some(format!("{}::{}", selection.provider, selection.model)))
    }

    pub async fn selected_model(&self) -> Result<Option<String>> {
        let requested = match &self.model {
            Some(model) => Some(model.clone()),
            None => self.last_model()?,
        };
        let Some(requested) = requested else {
            return Ok(None);
        };
        let models = self.model_catalog().await?;
        let matches: Vec<_> = models
            .iter()
            .filter(|model| {
                model.qualified_name() == requested
                    || (!requested.contains("::") && model.model == requested)
            })
            .collect();
        match matches.as_slice() {
            [model] => Ok(Some(model.qualified_name())),
            [] if self.model.is_none() => Ok(None),
            [] => anyhow::bail!("模型 {requested:?} 未配置"),
            _ => anyhow::bail!("模型名称不唯一，请使用 连接名::模型名"),
        }
    }

    pub fn remember_model(&self, qualified: &str) -> Result<()> {
        let (provider, model) = qualified.split_once("::").context("模型缺少连接名称")?;
        let path = self.selection_path()?;
        let directory = path.parent().context("模型偏好目录无效")?;
        std::fs::create_dir_all(directory)?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer(
            &mut file,
            &Selection {
                provider: provider.into(),
                model: model.into(),
            },
        )?;
        file.flush()?;
        file.persist(path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(root: &std::path::Path, name: &str) -> Local {
        let config = root.join(name);
        std::fs::write(
            &config,
            r#"[providers.one]
base_url = "http://localhost:8000/v1"
auto_refresh_models = false
models = [{ id = "first" }, { id = "chosen" }]
[providers.two]
base_url = "http://localhost:9000/v1"
auto_refresh_models = false
models = [{ id = "chosen" }]
"#,
        )
        .unwrap();
        Local {
            config,
            auth: root.join("auth"),
            model: None,
        }
    }

    #[tokio::test]
    async fn first_use_requires_selection_and_preferences_are_config_scoped() {
        let dir = tempfile::tempdir().unwrap();
        let first = local(dir.path(), "first.toml");
        let second = local(dir.path(), "second.toml");
        assert_eq!(first.selected_model().await.unwrap(), None);
        assert!(!first.selection_path().unwrap().exists());
        first.remember_model("two::chosen").unwrap();
        assert_eq!(
            first.selected_model().await.unwrap().as_deref(),
            Some("two::chosen")
        );
        assert_eq!(second.selected_model().await.unwrap(), None);
        first.remember_model("one::chosen").unwrap();
        assert_eq!(first.last_model().unwrap().as_deref(), Some("one::chosen"));
        let snapshot = first.snapshot().await.unwrap();
        assert!(!snapshot.content.contains("default_provider"));
        let config = FileConfig::parse(&snapshot.content, &first.auth).unwrap();
        assert_eq!(config.providers["one"].models[0].id, "first");
    }

    #[tokio::test]
    async fn unavailable_preference_requires_selection_and_explicit_errors_preserve_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut local = local(dir.path(), "config.toml");
        local.remember_model("one::removed").unwrap();
        assert_eq!(local.selected_model().await.unwrap(), None);
        local.model = Some("chosen".into());
        assert!(local.selected_model().await.is_err());
        local.model = Some("one::removed".into());
        assert!(local.selected_model().await.is_err());
        local.model = Some("first".into());
        assert_eq!(
            local.selected_model().await.unwrap().as_deref(),
            Some("one::first")
        );
        assert_eq!(local.last_model().unwrap().as_deref(), Some("one::removed"));
    }
}
