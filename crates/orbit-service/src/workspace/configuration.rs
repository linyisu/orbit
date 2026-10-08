use anyhow::Result;
use orbit_auth::AuthStore;
#[cfg(test)]
use orbit_config::test_support::{TestConfig, TestModel, TestProvider};
use orbit_protocol::Configuration;
use std::path::PathBuf;

pub struct Installed {
    pub options: super::setup::WorkspaceOptions,
    _directory: tempfile::TempDir,
}

impl Installed {
    pub async fn install(
        configuration: Configuration,
        options: &super::setup::WorkspaceOptions,
    ) -> Result<Installed> {
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
        let mut document: toml::Value = toml::from_str(&configuration.content)?;
        let rustup = std::env::var_os("RUSTUP_HOME")
            .map(PathBuf::from)
            .or_else(|| options.home.as_ref().map(|home| home.join(".rustup")));
        adapt_toolchain(&mut document, rustup);
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

fn adapt_toolchain(document: &mut toml::Value, rustup: Option<PathBuf>) {
    let Some(command) = document
        .get_mut("command")
        .and_then(toml::Value::as_table_mut)
    else {
        return;
    };
    let Some(path) = command.get("rust_toolchain").and_then(toml::Value::as_str) else {
        return;
    };
    if toolchain_exists(std::path::Path::new(path)) {
        return;
    }

    let discovered = rustup.and_then(|root| {
        let settings = std::fs::read_to_string(root.join("settings.toml")).ok()?;
        let settings: toml::Value = toml::from_str(&settings).ok()?;
        let name = settings.get("default_toolchain")?.as_str()?;
        let path = root.join("toolchains").join(name);
        toolchain_exists(&path).then_some(path)
    });
    let discovered = discovered
        .or_else(|| toolchain_exists(std::path::Path::new("/usr")).then(|| PathBuf::from("/usr")));
    if let Some(path) = discovered {
        command.insert(
            "rust_toolchain".into(),
            path.to_string_lossy().into_owned().into(),
        );
    } else {
        command.remove("rust_toolchain");
    }
}

fn toolchain_exists(path: &std::path::Path) -> bool {
    path.is_absolute()
        && path.join("bin/rustc").is_file()
        && path.join("bin/cargo").is_file()
        && path.join("lib/rustlib").is_dir()
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_client::configuration::Local;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn missing_local_toolchain_uses_target_default_without_breaking_other_commands() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let target = root.join("toolchains/target-default");
        std::fs::create_dir_all(target.join("bin")).unwrap();
        std::fs::create_dir_all(target.join("lib/rustlib")).unwrap();
        for program in ["cargo", "rustc"] {
            std::fs::write(target.join("bin").join(program), "").unwrap();
        }
        std::fs::write(
            root.join("settings.toml"),
            "default_toolchain = 'target-default'",
        )
        .unwrap();
        let missing = root.join("missing-local-toolchain");
        let mut config = toml::Value::try_from(serde_json::json!({
            "command": { "rust_toolchain": missing, "max_timeout_seconds": 123 }
        }))
        .unwrap();
        adapt_toolchain(&mut config, Some(root.into()));
        assert_eq!(
            config["command"]["rust_toolchain"].as_str(),
            target.to_str()
        );
        assert_eq!(
            config["command"]["max_timeout_seconds"].as_integer(),
            Some(123)
        );
        adapt_toolchain(&mut config, None);
        assert_eq!(
            config["command"]["rust_toolchain"].as_str(),
            target.to_str()
        );
        std::fs::remove_dir_all(target).unwrap();
        adapt_toolchain(&mut config, None);
        assert_ne!(
            config["command"]
                .get("rust_toolchain")
                .and_then(toml::Value::as_str),
            missing.to_str()
        );
    }

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
            &TestConfig::new()
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
            snapshot.credentials.keys().collect::<Vec<_>>(),
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
        let installed = Installed::install(snapshot, &options).await.unwrap();
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
        let root = installed.options.config.parent().unwrap().to_owned();
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        drop(installed);
        assert!(!root.exists());
    }
}
