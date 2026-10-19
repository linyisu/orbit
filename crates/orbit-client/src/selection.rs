//! Client model preferences, scoped to the local configuration file.
#[cfg(test)]
use orbit_config::test_support::{TestConfig, TestModel, TestProvider};
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

/// Resolve a picker or command choice against the catalog sampled from the active session.
pub fn requested_model(requested: &str, current: &str, models: &[String]) -> Result<String> {
    let qualified = if requested.contains("::") {
        requested.to_owned()
    } else {
        current.split_once("::").map_or_else(
            || requested.to_owned(),
            |(provider, _)| format!("{provider}::{requested}"),
        )
    };
    anyhow::ensure!(
        models.contains(&qualified),
        "模型未配置；请从当前会话的模型目录选择。"
    );
    Ok(qualified)
}

impl Local {
    pub async fn model_catalog(&self) -> Result<Vec<ModelOption>> {
        let mut config = FileConfig::parse(&self.read()?, &self.auth)?;
        let _ = orbit_config::models::updater::load_cached(&mut config).await;
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
        let path = match self.selection_path() {
            Ok(path) => path,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let content = match std::fs::read(path) {
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
        let (provider, model) = qualified
            .split_once("::")
            .context("模型缺少连接名称")?;
        let path = self.selection_path()?;
        let directory = path
            .parent()
            .context("模型偏好目录无效")?;
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

    #[tokio::test]
    async fn missing_configuration_is_first_use_but_invalid_configuration_is_an_error() {
        let directory = tempfile::tempdir().unwrap();
        let local = Local {
            config: directory
                .path()
                .join("not-created/config.toml"),
            auth: directory.path().join("auth"),
            model: None,
        };
        assert_eq!(local.last_model().unwrap(), None);
        assert_eq!(local.selected_model().await.unwrap(), None);
        assert!(
            local
                .model_catalog()
                .await
                .unwrap()
                .is_empty()
        );
        assert!(!local.config.exists());
        std::fs::create_dir(local.config.parent().unwrap()).unwrap();
        std::fs::write(&local.config, "invalid = [").unwrap();
        assert!(local.model_catalog().await.is_err());
        std::fs::remove_file(&local.config).unwrap();
        std::fs::create_dir(&local.config).unwrap();
        assert!(local.model_catalog().await.is_err());
    }

    #[test]
    fn model_choices_use_current_connection_and_require_qualified_foreign_names() {
        let models = vec![
            "one::small".into(),
            "two::small".into(),
            "two::large".into(),
        ];
        assert_eq!(
            requested_model("small", "one::old", &models).unwrap(),
            "one::small"
        );
        assert_eq!(
            requested_model("two::large", "one::old", &models).unwrap(),
            "two::large"
        );
        assert!(requested_model("large", "one::old", &models).is_err());
        assert!(requested_model("missing", "one::old", &models).is_err());
    }

    #[tokio::test]
    async fn opening_model_selection_does_not_contact_providers() {
        let root = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let config = root.path().join("config.toml");
        TestConfig::new()
            .provider(
                TestProvider::new("slow")
                    .endpoint(format!("http://{}/v1", listener.local_addr().unwrap()))
                    .models(["local-model"])
                    .refresh(true),
            )
            .write(&config)
            .unwrap();
        let local = Local {
            config,
            auth: root.path().join("auth"),
            model: Some("slow::local-model".into()),
        };
        let selected = tokio::time::timeout(
            std::time::Duration::from_millis(250),
            local.selected_model(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(selected.as_deref(), Some("slow::local-model"));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(30), listener.accept())
                .await
                .is_err()
        );
    }

    fn local(root: &std::path::Path, name: &str) -> Local {
        let config = root.join(name);
        std::fs::write(
            &config,
            TestConfig::new()
                .provider(
                    TestProvider::new("one")
                        .endpoint("http://localhost:8000/v1")
                        .models([TestModel::new("first"), TestModel::new("chosen")]),
                )
                .provider(
                    TestProvider::new("two")
                        .endpoint("http://localhost:9000/v1")
                        .models([TestModel::new("chosen")]),
                )
                .providers_toml()
                .unwrap(),
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
        first
            .remember_model("two::chosen")
            .unwrap();
        assert_eq!(
            first
                .selected_model()
                .await
                .unwrap()
                .as_deref(),
            Some("two::chosen")
        );
        assert_eq!(second.selected_model().await.unwrap(), None);
        first
            .remember_model("one::chosen")
            .unwrap();
        assert_eq!(first.last_model().unwrap().as_deref(), Some("one::chosen"));
        let snapshot = first.snapshot().await.unwrap();
        assert!(
            !snapshot
                .content
                .contains("default_provider")
        );
        let config = FileConfig::parse(&snapshot.content, &first.auth).unwrap();
        assert_eq!(config.providers["one"].models()[0].id, "first");
    }

    #[tokio::test]
    async fn unavailable_preference_requires_selection_and_explicit_errors_preserve_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut local = local(dir.path(), "config.toml");
        local
            .remember_model("one::removed")
            .unwrap();
        assert_eq!(local.selected_model().await.unwrap(), None);
        local.model = Some("chosen".into());
        assert!(local.selected_model().await.is_err());
        local.model = Some("one::removed".into());
        assert!(local.selected_model().await.is_err());
        local.model = Some("first".into());
        assert_eq!(
            local
                .selected_model()
                .await
                .unwrap()
                .as_deref(),
            Some("one::first")
        );
        assert_eq!(local.last_model().unwrap().as_deref(), Some("one::removed"));
    }
}
