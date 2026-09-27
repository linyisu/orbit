//! Account-scoped discovery, following OMP v18.2.8 discovery wire contracts.
use std::time::Duration;

use orbit_auth::{AuthStore, AuthTarget, CredentialRef};
use serde_json::{Value, json};

use crate::{client::ApiFormat, error::ModelError};

pub const CODEX_CLIENT_VERSION: &str = "0.153.0";

/// Only fixed account endpoints are accepted; credentials never follow redirects.
pub async fn discover(
    reference: &CredentialRef,
    format: ApiFormat,
    base: &str,
) -> Result<Value, ModelError> {
    let (target, paths) = match (format, base.trim_end_matches('/')) {
        (ApiFormat::OpenaiChatCompletions, "https://api.deepseek.com/v1") => (
            AuthTarget::Openai,
            vec!["https://api.deepseek.com/v1/models".to_owned()],
        ),
        (ApiFormat::OpenaiChatCompletions, "https://open.bigmodel.cn/api/coding/paas/v4") => (
            AuthTarget::Openai,
            vec!["https://open.bigmodel.cn/api/coding/paas/v4/models".to_owned()],
        ),
        (ApiFormat::GoogleCodeAssist, "https://daily-cloudcode-pa.googleapis.com") => (
            AuthTarget::GoogleCodeAssist,
            vec![
                "https://daily-cloudcode-pa.googleapis.com/v1internal:fetchAvailableModels"
                    .to_owned(),
            ],
        ),
        (ApiFormat::ChatgptCodex, "https://chatgpt.com/backend-api/codex") => (
            AuthTarget::ChatgptCodex,
            vec![
                format!(
                    "https://chatgpt.com/backend-api/codex/models?client_version={CODEX_CLIENT_VERSION}"
                ),
                format!(
                    "https://chatgpt.com/backend-api/models?client_version={CODEX_CLIENT_VERSION}"
                ),
            ],
        ),
        _ => {
            return Err(ModelError::Protocol(
                "account discovery is unavailable for this endpoint".into(),
            ));
        }
    };
    if format == ApiFormat::GoogleCodeAssist {
        orbit_auth::refresh_antigravity_client_version().await;
    }
    let endpoint = reqwest::Url::parse(base)
        .map_err(|_| ModelError::Protocol("invalid account endpoint".into()))?;
    let auth = AuthStore::new(reference.directory.clone())?
        .resolve(&reference.name, &endpoint, target)
        .await?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()?;
    let mut last = None;

    for path in paths {
        let request = if format == ApiFormat::GoogleCodeAssist {
            client.post(path).json(&json!({}))
        } else if format == ApiFormat::ChatgptCodex {
            client
                .get(path)
                .header("version", CODEX_CLIENT_VERSION)
                .header("openai-beta", "responses=experimental")
        } else {
            client.get(path)
        };
        let response = request
            .headers(auth.headers.clone())
            .header("accept", "application/json")
            .send()
            .await;
        let mut response = match response {
            Ok(response) => response,
            Err(error) => {
                last = Some(error.to_string());
                continue;
            }
        };
        if !response.status().is_success() {
            let status = response.status();
            if status.as_u16() == 401 || status.as_u16() == 403 {
                return Err(ModelError::Protocol(format!(
                    "model discovery rejected authorization: HTTP {status}"
                )));
            }
            last = Some(format!("HTTP {status}"));
            continue;
        }

        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if bytes.len().saturating_add(chunk.len()) > 4 * 1024 * 1024 {
                return Err(ModelError::Protocol(
                    "model discovery response exceeds 4 MiB".into(),
                ));
            }
            bytes.extend_from_slice(&chunk);
        }
        let payload: Value = serde_json::from_slice(&bytes)?;
        return Ok(payload);
    }

    Err(ModelError::Protocol(format!(
        "model discovery failed: {}",
        last.unwrap_or_default()
    )))
}
