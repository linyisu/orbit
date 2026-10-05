use std::{collections::BTreeMap, fmt, path::PathBuf};

use anyhow::{Context, Result};
use orbit_auth::{AuthStore, CredentialSnapshot};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct Local {
    pub config: PathBuf,
    pub auth: PathBuf,
    pub model: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct Configuration {
    content: String,
    credentials: BTreeMap<String, CredentialSnapshot>,
}

impl fmt::Debug for Configuration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Configuration([redacted])")
    }
}

impl Local {
    pub async fn snapshot(&self) -> Result<Configuration> {
        let content = std::fs::read_to_string(&self.config)
            .with_context(|| format!("无法读取本地配置 {}", self.config.display()))?;
        let mut parsed = crate::config::FileConfig::parse(&content, &self.auth)?;
        crate::account_models::refresh(&mut parsed, false).await;
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
                credentials.insert(name.clone(), store.snapshot(name).await?);
            }
        }

        Ok(Configuration {
            content: toml::to_string(&document)?,
            credentials,
        })
    }
}

pub struct Installed {
    pub options: super::setup::WorkspaceOptions,
    _directory: tempfile::TempDir,
}

impl Configuration {
    pub async fn install(self, options: &super::setup::WorkspaceOptions) -> Result<Installed> {
        use std::{
            io::Write,
            os::unix::fs::{OpenOptionsExt, PermissionsExt},
        };

        let directory = tempfile::Builder::new()
            .prefix("configuration-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(super::runtime_directory()?)?;
        let mut options = options.clone();
        options.config = directory.path().join("config.toml");
        options.auth_directory = directory.path().join("auth");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&options.config)?;
        file.write_all(self.content.as_bytes())?;
        let store = AuthStore::new(options.auth_directory.clone())?;
        for (name, credential) in self.credentials {
            store.import_snapshot(&name, &credential).await?;
        }
        crate::config::FileConfig::load(&options.config, &options.auth_directory)?;
        Ok(Installed {
            options,
            _directory: directory,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn local_configuration_carries_selected_model_and_only_referenced_credentials() {
        let source = tempfile::tempdir().unwrap();
        let target = tempfile::tempdir().unwrap();
        let auth = source.path().join("auth");
        let store = AuthStore::new(auth.clone()).unwrap();
        for name in ["used", "unrelated"] {
            store
                .set_api_key(name, format!("secret-{name}"), "https://example.com/v1")
                .await
                .unwrap();
        }
        let config = source.path().join("local.toml");
        std::fs::write(
            &config,
            r#"
default_provider = "test"
[providers.test]
base_url = "https://example.com/v1"
credential = "used"
auto_refresh_models = false
models = [{ id = "first" }, { id = "chosen" }]
"#,
        )
        .unwrap();
        let snapshot = Local {
            config,
            auth,
            model: Some("test::chosen".into()),
        }
        .snapshot()
        .await
        .unwrap();
        assert_eq!(
            snapshot.credentials.keys().collect::<Vec<_>>(),
            vec!["used"]
        );
        assert!(!format!("{snapshot:?}").contains("secret"));
        let snapshot: Configuration =
            serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
        let options = super::super::setup::WorkspaceOptions {
            initial_directory: target.path().into(),
            config: target.path().join("config.toml"),
            auth_directory: target.path().join("auth"),
            database: target.path().join("db"),
            allow_mcp_start: false,
            home: None,
            config_home: None,
            instruction_dir: None,
            editor_command: "vi".into(),
        };
        let installed = snapshot.install(&options).await.unwrap();
        assert!(!options.config.exists());
        assert!(!options.auth_directory.exists());
        let parsed = crate::config::FileConfig::load(
            &installed.options.config,
            &installed.options.auth_directory,
        )
        .unwrap();
        assert_eq!(
            parsed.default_model_option().unwrap().qualified_name(),
            "test::chosen"
        );
        let store = AuthStore::new(installed.options.auth_directory.clone()).unwrap();
        let auth = store
            .resolve(
                "used",
                &url::Url::parse("https://example.com/v1/chat/completions").unwrap(),
                orbit_auth::AuthTarget::Openai,
            )
            .await
            .unwrap();
        assert_eq!(auth.headers["authorization"], "Bearer secret-used");
        assert_eq!(
            std::fs::metadata(&installed.options.config)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        let root = installed.options.config.parent().unwrap().to_owned();
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        drop(installed);
        assert!(!root.exists());
    }
}
