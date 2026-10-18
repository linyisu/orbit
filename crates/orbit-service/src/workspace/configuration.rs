use anyhow::Result;
use orbit_auth::AuthStore;
#[cfg(test)]
use orbit_config::test_support::{TestConfig, TestModel, TestProvider};
use orbit_protocol::Configuration;

pub struct Installed {
    pub options: super::setup::WorkspaceOptions,
    _directory: tempfile::TempDir,
}

impl Installed {
    pub async fn install(
        configuration: Configuration,
        options: &super::setup::WorkspaceOptions,
    ) -> Result<Installed> {
        use std::io::Write;
        #[cfg(unix)]
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

        #[cfg(unix)]
        let directory = tempfile::Builder::new()
            .prefix("configuration-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(super::runtime_directory()?)?;
        #[cfg(windows)]
        let directory = orbit_runtime::filesystem::private::temporary_directory(
            &super::runtime_directory()?,
            "configuration-",
        )?;
        let mut options = options.clone();
        options.config = directory.path().join("config.toml");
        options.auth_directory = directory.path().join("auth");
        #[cfg(windows)]
        let mut file = orbit_runtime::filesystem::private::open_file(&options.config, true, true)?;
        #[cfg(unix)]
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&options.config)?;
        let document: toml::Value = toml::from_str(&configuration.content)?;
        file.write_all(toml::to_string(&document)?.as_bytes())?;
        let store = AuthStore::new(options.auth_directory.clone())?;
        for (name, credential) in configuration.credentials {
            store
                .import_snapshot(&name, &serde_json::from_value(credential)?)
                .await?;
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
    use orbit_client::configuration::Local;
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
            TestConfig::new()
                .provider(
                    TestProvider::new("test")
                        .endpoint("https://example.com/v1")
                        .credential("used")
                        .models([TestModel::new("first"), TestModel::new("chosen")]),
                )
                .providers_toml()
                .unwrap(),
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
            snapshot
                .credentials
                .keys()
                .collect::<Vec<_>>(),
            vec!["used"]
        );
        assert!(!format!("{snapshot:?}").contains("secret"));
        let snapshot: Configuration =
            serde_json::from_slice(&serde_json::to_vec(&snapshot).unwrap()).unwrap();
        let options = super::super::setup::WorkspaceOptions {
            model: Some("test::chosen".into()),
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
        let installed = Installed::install(snapshot, &options)
            .await
            .unwrap();
        assert!(!options.config.exists());
        assert!(!options.auth_directory.exists());
        let parsed = crate::config::FileConfig::load(
            &installed.options.config,
            &installed.options.auth_directory,
        )
        .unwrap();
        assert_eq!(
            parsed
                .select_model("test::chosen")
                .unwrap()
                .qualified_name(),
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
        let root = installed
            .options
            .config
            .parent()
            .unwrap()
            .to_owned();
        assert_eq!(
            std::fs::metadata(&root)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        drop(installed);
        assert!(!root.exists());
    }
}
