//! Editable connection snapshots and atomic configuration updates.
use std::path::Path;

use anyhow::{Context, Result, ensure};
use orbit_model::client::ApiFormat;

use crate::{
    config::{FileConfig, ModelProfile, ProviderConfig},
    models::ModelOption,
};

#[derive(Clone)]
pub struct ConnectionDraft {
    pub name: String,
    pub original_name: Option<String>,
    pub source: String,
    pub provider: Option<ProviderConfig>,
    pub format: ApiFormat,
    pub endpoint: String,
    pub models: Vec<ModelProfile>,
    original_models: Vec<String>,
}

impl ConnectionDraft {
    pub fn load(path: &Path, name: Option<&str>) -> Result<Self> {
        let source = super::read_config(path)?;
        let config: FileConfig = toml::from_str(&source)?;
        let provider = name
            .map(|name| {
                config
                    .providers
                    .get(name)
                    .cloned()
                    .context("连接已不存在")
            })
            .transpose()?;
        let models = provider
            .as_ref()
            .map_or_else(Vec::new, |p| {
                let mut ids = p
                    .models
                    .iter()
                    .map(|m| m.id.clone())
                    .collect::<Vec<_>>();
                // Keep overrides for temporarily unavailable automatic models editable and intact.
                for id in p.spec.model_overrides.keys() {
                    if !ids.contains(id) {
                        ids.push(id.clone());
                    }
                }
                ids.into_iter()
                    .map(|id| {
                        let mut explicit = ModelProfile::new(&id);
                        if let Some(settings) = p.spec.model_overrides.get(&id) {
                            settings.apply(&mut explicit, p.api_format);
                        }
                        explicit
                    })
                    .collect::<Vec<_>>()
            });

        Ok(Self {
            name: name.unwrap_or_default().to_owned(),
            original_name: name.map(str::to_owned),
            format: provider
                .as_ref()
                .map_or(ApiFormat::OpenaiChatCompletions, |p| p.api_format),
            endpoint: provider
                .as_ref()
                .map_or_else(String::new, |p| p.base_url.clone()),
            original_models: models
                .iter()
                .map(|m| m.id.clone())
                .collect(),
            models,
            provider,
            source,
        })
    }

    pub fn account(&self) -> bool {
        matches!(
            self.format,
            ApiFormat::GoogleCodeAssist | ApiFormat::ChatgptCodex
        )
    }

    /// Read provider-independent defaults from this reviewed snapshot, without network discovery.
    pub fn model_defaults(&self) -> Result<(usize, u32)> {
        let config: FileConfig = toml::from_str(&self.source)?;
        Ok((config.context_tokens, config.max_output_tokens))
    }

    /// Validate connection identity and credential routing before discovery or persistence.
    pub fn validate_service(&self, key: &KeyChange<'_>) -> Result<()> {
        ensure!(super::valid_name(&self.name), "连接名称无效");
        let endpoint = url::Url::parse(&self.endpoint).context("服务地址无效")?;
        ensure!(
            matches!(endpoint.scheme(), "http" | "https")
                && endpoint.host_str().is_some()
                && endpoint.username().is_empty()
                && endpoint.password().is_none()
                && endpoint.query().is_none()
                && endpoint.fragment().is_none(),
            "请输入不含账号、查询参数或片段的 HTTP(S) 服务地址"
        );

        if let Some(provider) = &self.provider {
            if matches!(
                provider.api_format,
                ApiFormat::GoogleCodeAssist | ApiFormat::ChatgptCodex
            ) {
                ensure!(
                    self.format == provider.api_format
                        && self.endpoint == provider.base_url
                        && matches!(key, KeyChange::Keep),
                    "套餐账号的协议与地址由认证流程管理"
                );
            }

            if matches!(key, KeyChange::Keep)
                && (provider.credential.is_some() || !provider.api_key.is_empty())
            {
                ensure!(
                    self.endpoint.trim_end_matches('/') == provider.base_url.trim_end_matches('/'),
                    "修改服务地址后，请重新填写密钥或选择无需认证"
                );
            }
        }

        Ok(())
    }

    fn preserves_automatic_selection(&self) -> bool {
        self.provider
            .as_ref()
            .is_some_and(|p| p.spec.models.is_auto())
            && self
                .original_models
                .iter()
                .eq(self.models.iter().map(|m| &m.id))
    }
}

pub fn saved_connections(path: &Path) -> Result<Vec<String>> {
    let config: FileConfig = toml::from_str(&super::read_config(path)?)?;
    Ok(config.providers.into_keys().collect())
}

/// Keep means retain the existing credential without copying its secret into the UI.
pub enum KeyChange<'a> {
    Keep,
    Replace(&'a str),
    None,
}

impl<'a> KeyChange<'a> {
    /// A blank field retains existing authentication, or declares an unauthenticated new service.
    pub fn from_input(secret: &'a str, retain: bool) -> Self {
        match (secret.is_empty(), retain) {
            (false, _) => Self::Replace(secret),
            (true, true) => Self::Keep,
            (true, false) => Self::None,
        }
    }
}

pub async fn save(
    path: &Path,
    auth: &Path,
    draft: &ConnectionDraft,
    key: KeyChange<'_>,
) -> Result<Vec<ModelOption>> {
    draft.validate_service(&key)?;
    ensure!(
        !draft.models.is_empty() || draft.preserves_automatic_selection(),
        "请至少启用一个模型"
    );
    super::models::validate_profiles(&draft.models)?;
    let mut document = draft
        .source
        .parse::<toml_edit::DocumentMut>()?;
    if let Some(name) = &draft.original_name {
        ensure!(name == &draft.name, "编辑连接时不能更改连接名称");
    } else {
        ensure!(
            document["providers"]
                .get(&draft.name)
                .is_none(),
            "连接名称已存在"
        );
    }

    let credential = format!(
        "connection-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    );
    let providers = document["providers"]
        .clone()
        .into_table()
        .map_err(|_| anyhow::anyhow!("供应商配置无效"))?;
    document["providers"] = toml_edit::Item::Table(providers);
    let item = &mut document["providers"][&draft.name];
    let table = if item.is_none() {
        toml_edit::Table::new()
    } else {
        item.clone()
            .into_table()
            .map_err(|_| anyhow::anyhow!("连接配置无效"))?
    };

    *item = toml_edit::Item::Table(table);
    let provider = &mut document["providers"][&draft.name];
    provider["api_format"] = toml_edit::value(
        serde_json::to_value(draft.format)?
            .as_str()
            .context("协议无效")?,
    );
    provider["base_url"] = toml_edit::value(&draft.endpoint);
    let mut spec = crate::config::ProviderSpec::new(
        draft.format,
        &draft.endpoint,
        crate::config::ModelSelection::Explicit(Vec::new()),
    );
    spec.set_models(&draft.models);
    if draft.preserves_automatic_selection() {
        spec.models = crate::config::ModelSelection::auto();
    }
    super::document::models(provider, &spec)?;
    match key {
        KeyChange::Keep => {}
        KeyChange::Replace(_) | KeyChange::None => {
            let table = provider
                .as_table_like_mut()
                .context("连接配置无效")?;
            table.remove("api_key");
            table.remove("credential");
            if matches!(key, KeyChange::Replace(_)) {
                provider["credential"] = toml_edit::value(&credential);
            }
        }
    }

    let updated = document.to_string();
    let parsed = FileConfig::parse(&updated, auth)?;
    for model in parsed.model_options()? {
        orbit_model::client::ModelClient::new(model.config)?;
    }
    ensure!(
        super::read_config(path)? == draft.source,
        "配置已被其他程序修改，请返回后重新打开"
    );

    let store = orbit_auth::AuthStore::new(auth.to_path_buf())?;
    if let KeyChange::Replace(secret) = key {
        store
            .set_api_key(&credential, secret.to_owned(), &draft.endpoint)
            .await?;
    }

    if let Err(error) = super::persist(path, &draft.source, &updated) {
        if matches!(key, KeyChange::Replace(_)) {
            store.remove(&credential).await?;
        }
        return Err(error);
    }

    FileConfig::load(path, auth)?.model_options()
}

/// Remove one connection from the reviewed snapshot. A credential shared with
/// another connection remains in the authentication store.
pub async fn remove(path: &Path, auth: &Path, draft: &ConnectionDraft) -> Result<Vec<ModelOption>> {
    let name = draft
        .original_name
        .as_deref()
        .context("只能移除已保存的连接")?;
    let original: FileConfig = toml::from_str(&draft.source)?;
    let credential = original
        .providers
        .get(name)
        .and_then(|provider| provider.credential.clone());
    let mut document = draft
        .source
        .parse::<toml_edit::DocumentMut>()?;
    let providers = document["providers"]
        .as_table_like_mut()
        .context("供应商配置无效")?;
    ensure!(providers.remove(name).is_some(), "连接已不存在，请刷新列表");
    if providers.is_empty() {
        // A parent created by [providers.name] is implicit; once empty it
        // would disappear during serialization, but FileConfig requires it.
        document["providers"] = toml_edit::Item::Table(toml_edit::Table::new());
    }
    if document
        .get("compaction")
        .and_then(|item| item.get("model"))
        .and_then(toml_edit::Item::as_str)
        .and_then(|model| model.split_once("::"))
        .is_some_and(|(provider, _)| provider == name)
        && let Some(compaction) = document["compaction"].as_table_like_mut()
    {
        compaction.remove("model");
    }
    let updated = document.to_string();
    let remaining = FileConfig::parse(&updated, auth)?;
    let options = remaining.model_options()?;
    for model in &options {
        orbit_model::client::ModelClient::new(model.config.clone())?;
    }
    super::persist(path, &draft.source, &updated)?;
    if let Some(credential) = credential.as_ref()
        && !remaining
            .providers
            .values()
            .any(|provider| provider.credential.as_ref() == Some(credential))
    {
        orbit_auth::AuthStore::new(auth.to_path_buf())?
            .remove(credential)
            .await
            .context("连接已移除，但无法清理它的凭据。请刷新列表后重试凭据清理。")?;
    }
    Ok(options)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn model_limits_can_be_saved_and_cleared_without_replacing_other_settings() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let auth = directory.path().join("auth");
        let store = orbit_auth::AuthStore::new(&auth).unwrap();
        store
            .set_api_key("existing", "secret".into(), "http://localhost/v1")
            .await
            .unwrap();
        std::fs::write(
            &path,
            "# preserved\n[providers.gateway]\napi_format='openai_responses'\nbas\
            e_url='http://localhost/v1'\ncredential='existing'\nauto_refresh_models=false\ntimeout\
            _seconds=75\nmodels=['a','b']\n[providers.gateway.model_overrides.a]\nsupports_vision=\
            true\n",
        )
        .unwrap();
        let mut draft = ConnectionDraft::load(&path, Some("gateway")).unwrap();
        draft.models[0].context_tokens = Some(131_072);
        draft.models[0].input_context_tokens = Some(96_000);
        draft.models[0].max_output_tokens = Some(8_192);
        let options = save(&path, &auth, &draft, KeyChange::Keep)
            .await
            .unwrap();
        assert_eq!(options[0].input_context_tokens, 96_000);
        assert_eq!(options[0].max_output_tokens, 8_192);
        assert!(options[0].supports_vision);

        let mut draft = ConnectionDraft::load(&path, Some("gateway")).unwrap();
        assert_eq!(draft.models[0].context_tokens, Some(131_072));
        assert_eq!(draft.models[0].input_context_tokens, Some(96_000));
        assert_eq!(draft.models[0].max_output_tokens, Some(8_192));
        assert!(draft.models[1].context_tokens.is_none());
        draft.models[0].context_tokens = None;
        draft.models[0].input_context_tokens = None;
        draft.models[0].max_output_tokens = None;
        save(&path, &auth, &draft, KeyChange::Keep)
            .await
            .unwrap();

        let draft = ConnectionDraft::load(&path, Some("gateway")).unwrap();
        assert!(draft.models[0].context_tokens.is_none());
        assert!(
            draft.models[0]
                .input_context_tokens
                .is_none()
        );
        assert!(
            draft.models[0]
                .max_output_tokens
                .is_none()
        );
        assert_eq!(draft.models[0].supports_vision, Some(true));
        let configuration = FileConfig::load(&path, &auth).unwrap();
        assert_eq!(configuration.providers["gateway"].timeout_seconds, 75);
        assert_eq!(
            configuration.providers["gateway"]
                .credential
                .as_deref(),
            Some("existing")
        );
        assert_eq!(
            configuration.providers["gateway"]
                .models
                .len(),
            2
        );
        assert_eq!(store.list().unwrap().len(), 1);
        let source = std::fs::read_to_string(&path).unwrap();
        assert!(source.starts_with("# preserved"));
        assert!(!source.contains("secret"));
    }

    #[tokio::test]
    async fn removal_preserves_shared_credentials_and_rejects_stale_snapshots() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.toml");
        let auth = directory.path().join("auth");
        let store = orbit_auth::AuthStore::new(&auth).unwrap();
        store
            .set_api_key("shared", "secret".into(), "http://localhost/v1")
            .await
            .unwrap();
        let provider = "api_format='openai_chat_completions'\nbase_url='http://localhost/v1'\ncrede\
            ntial='shared'\nmodels=['model']\n";
        std::fs::write(
            &path,
            format!(
                "# preserved\n[compaction]\nmodel='first::model'\n[providers.\
            first]\n{provider}[providers.second]\n{provider}"
            ),
        )
        .unwrap();
        let first = ConnectionDraft::load(&path, Some("first")).unwrap();
        let stale = ConnectionDraft::load(&path, Some("second")).unwrap();
        let options = remove(&path, &auth, &first)
            .await
            .unwrap();
        assert_eq!(options.len(), 1);
        assert_eq!(saved_connections(&path).unwrap(), vec!["second"]);
        assert!(
            FileConfig::load(&path, &auth)
                .unwrap()
                .compaction
                .model
                .is_none()
        );
        assert_eq!(store.list().unwrap().len(), 1);
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .starts_with("# preserved")
        );
        assert!(
            remove(&path, &auth, &stale)
                .await
                .is_err()
        );
        assert_eq!(store.list().unwrap().len(), 1);
        let second = ConnectionDraft::load(&path, Some("second")).unwrap();
        assert!(
            remove(&path, &auth, &second)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(store.list().unwrap().is_empty());
        assert!(
            saved_connections(&path)
                .unwrap()
                .is_empty()
        );
    }
}
