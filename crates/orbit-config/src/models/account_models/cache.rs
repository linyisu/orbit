//! Credential-scoped catalog persistence; legacy wire caches are decoded at this boundary.
use crate::config::ProviderConfig;
use anyhow::{Context, Result};
use orbit_model::{catalog::ModelCatalog, client::ApiFormat};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

const CACHE_LIFETIME: Duration = Duration::from_secs(15 * 60);
const MAX_CACHE_BYTES: u64 = 4 * 1_024 * 1_024;

pub(super) struct CatalogCache {
    path: PathBuf,
    binding: Value,
    format: ApiFormat,
}

impl CatalogCache {
    pub fn new(name: &str, provider: &ProviderConfig, directory: &Path) -> Result<Self> {
        let identity = if let Some(name) = &provider.credential {
            let info = orbit_auth::AuthStore::new(directory.to_path_buf())?
                .list()?
                .into_iter()
                .find(|info| info.name == *name)
                .context("账号尚未登录")?;
            let revision = if info.kind == orbit_auth::CredentialKind::ApiKey {
                std::fs::metadata(directory.join(format!("{name}.json")))
                    .ok()
                    .and_then(|metadata| metadata.modified().ok())
                    .and_then(|time| {
                        time.duration_since(SystemTime::UNIX_EPOCH)
                            .ok()
                    })
                    .map(|duration| duration.as_nanos().to_string())
            } else {
                None
            };
            let mut identity = json!({
                "name": name,
                "kind": info.kind,
                "endpoint": info.endpoint,
                "account": info.account,
                "account_id": info.account_id,
                "project": info.project_id,
                "revision": revision
            });
            if let Some(client) = info.registration_id {
                identity["registration_id"] = json!(client);
            }
            identity
        } else {
            json!({
                "key_fingerprint": fingerprint(&provider.api_key)
            })
        };
        let binding = json!({
            "version": 2,
            "identity": identity,
            "format": provider.api_format,
            "endpoint": provider.base_url.trim_end_matches('/')
        });

        Ok(Self {
            path: directory
                .join("model-catalog")
                .join(format!("{}.json", fingerprint(name))),
            binding,
            format: provider.api_format,
        })
    }

    pub fn load(&self) -> Option<ModelCatalog> {
        read_cache(&self.path, &self.binding, self.format)
    }

    pub fn fresh(&self) -> bool {
        std::fs::metadata(&self.path)
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|time| {
                SystemTime::now()
                    .duration_since(time)
                    .ok()
            })
            .is_some_and(|age| age < CACHE_LIFETIME)
    }

    pub fn write(&self, catalog: &ModelCatalog) -> Result<()> {
        let directory = self
            .path
            .parent()
            .expect("catalog cache directory");
        let mut directories = std::fs::DirBuilder::new();
        directories.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            directories.mode(0o700);
        }
        directories.create(directory)?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        file.write_all(&serde_json::to_vec(
            &json!({ "binding": self.binding, "catalog": catalog }),
        )?)?;
        file.as_file().sync_all()?;
        file.persist(&self.path)?;
        Ok(())
    }
}

pub(super) fn fingerprint(value: &str) -> String {
    Sha256::digest(value.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

pub(super) fn read_cache(path: &Path, binding: &Value, format: ApiFormat) -> Option<ModelCatalog> {
    let file = std::fs::File::open(path).ok()?;
    use std::io::Read;
    let mut bytes = Vec::new();
    file.take(MAX_CACHE_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAX_CACHE_BYTES {
        return None;
    }
    let data: Value = serde_json::from_slice(&bytes).ok()?;
    if data.get("binding") != Some(binding) {
        return None;
    }
    let catalog: ModelCatalog = if let Some(catalog) = data.get("catalog") {
        serde_json::from_value(catalog.clone()).ok()?
    } else {
        orbit_model::catalog::parse(format, &data["payload"]).ok()?
    };
    catalog.validate(format).ok()?;
    Some(catalog)
}
