//! Public serving specifications supplement discovery without granting model access.
use std::{
    fs,
    io::Write,
    path::Path,
    time::{Duration, SystemTime},
};

use anyhow::Result;
use orbit_model::catalog::public_metadata::{Catalog, bundled, supports as api_format};
#[cfg(test)]
use orbit_model::{
    catalog::public_metadata::Spec,
    client::ApiFormat,
    types::{ReasoningAdapter, ReasoningEffort},
};
use orbit_tools::web_fetch::{WebClient, WebConfig};
use serde_json::{Value, json};

use crate::config::FileConfig;
#[cfg(test)]
use crate::config::{ModelProfile, ProviderConfig, ReasoningProfile};

const SOURCE_URL: &str = "https://models.dev/api.json";
const CACHE_LIFETIME: Duration = Duration::from_secs(86_400);
const REFRESH_TIMEOUT: Duration = Duration::from_secs(3);
const MAX_CATALOG_BYTES: usize = 20 * 1_024 * 1_024;

fn normalize(payload: Value) -> Result<Catalog> {
    let source = json!({
        "url": SOURCE_URL,
        "retrieved_at": SystemTime::now().duration_since(SystemTime::UNIX_EPOCH)?.as_secs(),
        "license": "MIT"
    });
    orbit_model::catalog::public_metadata::normalize(payload, source)
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
        catalog.supplement(
            provider.spec.api_format,
            &provider.spec.base_url,
            &mut provider.models,
        );
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

#[cfg(test)]
mod tests;
