//! Public serving specifications supplement discovery without granting model access.
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::Path,
    sync::LazyLock,
    time::{Duration, SystemTime},
};

use anyhow::{Context, Result, ensure};
use orbit_model::{
    client::ApiFormat,
    types::{ReasoningAdapter, ReasoningEffort},
};
use orbit_tools::web_fetch::{WebClient, WebConfig};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::config::{
    FileConfig, MIN_CONTEXT_TOKENS, ModelProfile, ProviderConfig, ReasoningProfile,
};

const SOURCE_URL: &str = "https://models.dev/api.json";
const CACHE_LIFETIME: Duration = Duration::from_secs(86_400);
const REFRESH_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_CATALOG_BYTES: usize = 20 * 1_024 * 1_024;

#[derive(Clone, Deserialize, Serialize)]
struct Catalog {
    source: Value,
    providers: BTreeMap<String, Provider>,
}

#[derive(Clone, Deserialize, Serialize)]
struct Provider {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    api: Option<String>,
    models: BTreeMap<String, Spec>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
struct Spec {
    #[serde(default)]
    limit: Limits,
    #[serde(default)]
    modalities: Modalities,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    reasoning_options: Vec<Value>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
struct Limits {
    #[serde(skip_serializing_if = "Option::is_none")]
    context: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    input: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
struct Modalities {
    #[serde(default)]
    input: Vec<String>,
}

fn bundled() -> &'static Catalog {
    static CATALOG: LazyLock<Catalog> = LazyLock::new(|| {
        serde_json::from_str(include_str!("model_metadata.json"))
            .expect("validated model metadata snapshot")
    });
    &CATALOG
}

fn normalize(payload: Value) -> Result<Catalog> {
    let entries = payload
        .as_object()
        .context("model metadata is not a provider catalog")?;
    let mut providers = BTreeMap::new();
    for (id, entry) in entries {
        let Some(models) = entry["models"].as_object() else {
            continue;
        };
        let models = models
            .iter()
            .filter_map(|(id, spec)| {
                if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
                    return None;
                }
                serde_json::from_value(spec.clone())
                    .ok()
                    .map(|spec| (id.clone(), spec))
            })
            .collect::<BTreeMap<_, Spec>>();
        if !models.is_empty() {
            providers.insert(
                id.clone(),
                Provider {
                    api: entry["api"].as_str().map(str::to_owned),
                    models,
                },
            );
        }
    }
    ensure!(
        !providers.is_empty(),
        "model metadata contains no usable providers"
    );
    Ok(Catalog {
        source: json!({
            "url": SOURCE_URL,
            "retrieved_at": SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)?
                .as_secs(),
            "license": "MIT"
        }),
        providers,
    })
}

fn read_cache(path: &Path) -> Option<Catalog> {
    if fs::metadata(path).ok()?.len() > MAX_CATALOG_BYTES as u64 {
        return None;
    }
    let catalog: Catalog = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    (catalog.source["url"] == SOURCE_URL && !catalog.providers.is_empty()).then_some(catalog)
}

fn save(path: &Path, catalog: &Catalog) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut directories = fs::DirBuilder::new();
    directories.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        directories.mode(0o700);
    }
    directories.create(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    write_catalog(&mut file, catalog)?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

fn write_catalog(writer: &mut impl Write, catalog: &Catalog) -> Result<()> {
    writeln!(writer, "{{")?;
    write!(writer, "  \"source\": ")?;
    serde_json::to_writer(&mut *writer, &catalog.source)?;
    writeln!(writer, ",\n  \"providers\": {{")?;
    for (index, (id, provider)) in catalog.providers.iter().enumerate() {
        if index > 0 {
            writeln!(writer, ",")?;
        }
        write!(writer, "    {}: {{", serde_json::to_string(id)?)?;
        if let Some(api) = &provider.api {
            write!(writer, "\n      \"api\": {},", serde_json::to_string(api)?)?;
        }
        writeln!(writer, "\n      \"models\": {{")?;
        for (index, (id, spec)) in provider.models.iter().enumerate() {
            if index > 0 {
                writeln!(writer, ",")?;
            }
            write!(writer, "        {}: ", serde_json::to_string(id)?)?;
            serde_json::to_writer(&mut *writer, spec)?;
        }
        write!(writer, "\n      }}\n    }}")?;
    }
    writeln!(writer, "\n  }}\n}}")?;
    Ok(())
}

async fn download(web: &WebConfig) -> Result<Catalog> {
    normalize(
        WebClient::new(web)?
            .fetch_json(SOURCE_URL, MAX_CATALOG_BYTES)
            .await?,
    )
}

pub async fn export(output: &Path, web: &WebConfig) -> Result<()> {
    save(output, &download(web).await?)
}

pub(crate) struct MetadataUpdate {
    pub status: String,
    pub error: Option<String>,
}

pub(crate) async fn update(config: &mut FileConfig, force: bool, network: bool) -> MetadataUpdate {
    if !config
        .providers
        .values()
        .any(|p| api_format(p.api_format))
    {
        return MetadataUpdate {
            status: "公共规格：此连接使用账号专属目录".into(),
            error: None,
        };
    }
    let Some(directory) = config.auth_directory.as_ref() else {
        return MetadataUpdate {
            status: "公共规格：使用内置快照".into(),
            error: None,
        };
    };
    let path = directory.join("model-metadata/catalog.json");
    let cache = read_cache(&path);
    let fresh = fs::metadata(&path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|stamp| stamp.elapsed().ok())
        .is_some_and(|age| age < CACHE_LIFETIME);
    let refresh = network
        && config
            .providers
            .values()
            .any(|provider| api_format(provider.api_format) && provider.auto_refresh_models);
    let attempted = force || (refresh && !(fresh && cache.is_some()));
    let previous = cache
        .as_ref()
        .map(|c| serde_json::to_value(&c.providers).expect("serializable catalog"));
    let (catalog, error) = if !force && (!refresh || (fresh && cache.is_some())) {
        (cache, None)
    } else {
        match tokio::time::timeout(REFRESH_TIMEOUT, download(&config.web)).await {
            Ok(Ok(catalog)) => {
                let error = save(&path, &catalog)
                    .err()
                    .map(|e| format!("模型规格缓存写入失败：{e}"));
                (Some(catalog), error)
            }
            result => {
                let error = match result {
                    Ok(Err(error)) => error.to_string(),
                    Err(_) => "请求超时".into(),
                    _ => unreachable!(),
                };
                (
                    cache,
                    Some(format!("模型规格更新失败，沿用缓存或内置快照：{error}")),
                )
            }
        }
    };
    let catalog = catalog
        .as_ref()
        .unwrap_or_else(|| bundled());
    for provider in config.providers.values_mut() {
        catalog.supplement(provider);
    }
    let status = if error.is_some() {
        "公共规格：更新失败，沿用缓存或内置快照"
    } else if !attempted {
        "公共规格：使用有效缓存或内置快照"
    } else if previous
        == Some(serde_json::to_value(&catalog.providers).expect("serializable catalog"))
    {
        "公共规格：已是最新"
    } else {
        "公共规格：已更新"
    };
    MetadataUpdate {
        status: format!("{status} · models.dev"),
        error,
    }
}

fn api_format(format: ApiFormat) -> bool {
    matches!(
        format,
        ApiFormat::OpenaiChatCompletions
            | ApiFormat::OpenaiResponses
            | ApiFormat::AnthropicMessages
    )
}

fn endpoint(value: &str) -> Option<String> {
    let url = url::Url::parse(value).ok()?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return None;
    }
    let path = url.path().trim_end_matches('/');
    let path = path.strip_suffix("/v1").unwrap_or(path);
    Some(format!("{}{path}", url.origin().ascii_serialization()))
}

impl Catalog {
    fn supplement(&self, provider: &mut ProviderConfig) {
        if !api_format(provider.api_format) {
            return;
        }
        let Some(base) = endpoint(&provider.base_url) else {
            return;
        };
        let catalogs = self
            .providers
            .iter()
            .filter(|(id, entry)| {
                let api = entry
                    .api
                    .as_deref()
                    .or(match id.as_str() {
                        "openai" => Some("https://api.openai.com/v1"),
                        "anthropic" => Some("https://api.anthropic.com/v1"),
                        _ => None,
                    });
                api.and_then(endpoint).as_ref() == Some(&base)
            })
            .collect::<Vec<_>>();
        if catalogs.is_empty() {
            return;
        }
        let format = provider.spec.api_format;
        for model in &mut provider.models {
            let mut specs = catalogs
                .iter()
                .filter_map(|(id, catalog)| {
                    catalog
                        .models
                        .get(&model.id)
                        .map(|spec| (id.as_str(), spec))
                });
            let Some((id, spec)) = specs.next() else {
                continue;
            };
            if specs.any(|(_, other)| other != spec) {
                continue;
            }
            spec.supplement(model, id, format);
        }
    }
}

impl Spec {
    // Explicit profiles and live serving metadata have already been applied.
    // Unknown endpoints and IDs never borrow limits from a similarly named model.
    fn supplement(&self, model: &mut ModelProfile, provider: &str, format: ApiFormat) {
        model.context_tokens = model.context_tokens.or(self
            .limit
            .context
            .filter(|n| *n >= MIN_CONTEXT_TOKENS));
        model.input_context_tokens = model
            .input_context_tokens
            .or(self.limit.input.filter(|n| {
                *n >= MIN_CONTEXT_TOKENS
                    && model
                        .context_tokens
                        .is_none_or(|context| *n <= context)
            }));
        model.max_output_tokens = model
            .max_output_tokens
            .or(self.limit.output.filter(|n| *n > 0));
        if model.supports_vision.is_none() && !self.modalities.input.is_empty() {
            model.supports_vision = Some(
                self.modalities
                    .input
                    .iter()
                    .any(|kind| kind == "image"),
            );
        }
        if model.reasoning.is_none() {
            let adapter = match (provider, format) {
                ("openai", ApiFormat::OpenaiChatCompletions) => Some(ReasoningAdapter::OpenaiChat),
                ("openai", ApiFormat::OpenaiResponses) => Some(ReasoningAdapter::OpenaiResponses),
                ("anthropic", ApiFormat::AnthropicMessages) => {
                    Some(ReasoningAdapter::AnthropicMessages)
                }
                _ => None,
            };
            if let Some(adapter) = adapter {
                let efforts = self
                    .reasoning_options
                    .iter()
                    .filter(|option| option["type"] == "effort")
                    .filter_map(|option| option["values"].as_array())
                    .flatten()
                    .filter_map(|value| {
                        serde_json::from_value::<ReasoningEffort>(value.clone()).ok()
                    })
                    .collect::<Vec<_>>();
                if let Some(default) = efforts
                    .iter()
                    .find(|e| **e == ReasoningEffort::High)
                    .or_else(|| efforts.last())
                    .copied()
                {
                    model.reasoning = Some(ReasoningProfile {
                        adapter,
                        efforts,
                        default,
                        extra: toml::Table::new(),
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
