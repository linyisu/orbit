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
}

impl ConnectionDraft {
    pub fn load(path: &Path, name: Option<&str>) -> Result<Self> {
        let source = super::read_config(path)?;
        let config: FileConfig = toml::from_str(&source)?;
        let provider = name
            .map(|name| config.providers.get(name).cloned().context("连接已不存在"))
            .transpose()?;

        Ok(Self {
            name: name.unwrap_or_default().to_owned(),
            original_name: name.map(str::to_owned),
            format: provider
                .as_ref()
                .map_or(ApiFormat::OpenaiChatCompletions, |p| p.api_format),
            endpoint: provider
                .as_ref()
                .map_or_else(String::new, |p| p.base_url.clone()),
            models: provider
                .as_ref()
                .map_or_else(Vec::new, |p| p.models.clone()),
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

pub async fn save(
    path: &Path,
    auth: &Path,
    draft: &ConnectionDraft,
    key: KeyChange<'_>,
) -> Result<Vec<ModelOption>> {
    ensure!(
        !draft.name.trim().is_empty()
            && draft.name.len() <= 128
            && !draft.name.chars().any(char::is_control),
        "连接名称无效"
    );
    ensure!(!draft.models.is_empty(), "请至少启用一个模型");
    ensure!(
        draft.models.iter().all(|m| !m.id.trim().is_empty()
            && m.id.len() <= 256
            && !m.id.chars().any(char::is_control)),
        "模型 ID 无效"
    );
    let endpoint = url::Url::parse(&draft.endpoint).context("服务地址无效")?;
    ensure!(
        matches!(endpoint.scheme(), "http" | "https")
            && endpoint.host_str().is_some()
            && endpoint.username().is_empty()
            && endpoint.password().is_none()
            && endpoint.query().is_none()
            && endpoint.fragment().is_none(),
        "请输入不含账号、查询参数或片段的 HTTP(S) 服务地址"
    );

    if let Some(provider) = &draft.provider {
        if matches!(
            provider.api_format,
            ApiFormat::GoogleCodeAssist | ApiFormat::ChatgptCodex
        ) {
            ensure!(
                draft.format == provider.api_format
                    && draft.endpoint == provider.base_url
                    && matches!(key, KeyChange::Keep),
                "套餐账号的协议与地址由认证流程管理"
            );
        }

        if matches!(key, KeyChange::Keep) && provider.credential.is_some() {
            ensure!(
                draft.endpoint.trim_end_matches('/') == provider.base_url.trim_end_matches('/'),
                "修改服务地址后，请重新填写密钥或选择无需认证"
            );
        }
    }

    let mut document = draft.source.parse::<toml_edit::DocumentMut>()?;
    if let Some(name) = &draft.original_name {
        ensure!(name == &draft.name, "编辑连接时不能更改连接名称");
    } else {
        ensure!(
            document["providers"].get(&draft.name).is_none(),
            "连接名称已存在"
        );
    }

    let credential = format!(
        "connection-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos()
    );
    let serialized = toml::to_string(&serde_json::json!({ "models": draft.models }))?;
    let mut model_doc = serialized.parse::<toml_edit::DocumentMut>()?;
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
    provider["managed_models"] = toml_edit::value(true);
    provider["models"] = model_doc.remove("models").context("模型配置无效")?;
    match key {
        KeyChange::Keep => {}
        KeyChange::Replace(_) | KeyChange::None => {
            let table = provider.as_table_like_mut().context("连接配置无效")?;
            table.remove("api_key");
            table.remove("credential");
            if matches!(key, KeyChange::Replace(_)) {
                provider["credential"] = toml_edit::value(&credential);
            }
        }
    }

    if document["default_provider"]
        .as_str()
        .is_none_or(str::is_empty)
    {
        document["default_provider"] = toml_edit::value(&draft.name);
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
