//! Account-scoped discovery, following OMP v18.2.8 discovery wire contracts.
use std::time::Duration;

use orbit_auth::{AuthStore, AuthTarget, CredentialRef};
use serde_json::{Value, json};

use crate::{client::ApiFormat, error::ModelError};

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);

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

        (ApiFormat::GoogleCodeAssist, "https://cloudcode-pa.googleapis.com") => (
            AuthTarget::GoogleCodeAssist,
            vec!["https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota".to_owned()],
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
        .timeout(DISCOVERY_TIMEOUT)
        .build()?;
    let mut last = None;

    for path in paths {
        let request = if format == ApiFormat::GoogleCodeAssist {
            let body = if base.trim_end_matches('/') == "https://cloudcode-pa.googleapis.com" {
                json!({ "project": auth.project_id })
            } else {
                json!({})
            };
            client.post(path).json(&body)
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

pub async fn discover_api_models(
    base: &str,
    format: ApiFormat,
    key: &str,
) -> Result<Vec<String>, ModelError> {
    api_model_ids(&discover_api_catalog(base, format, key).await?)
}

pub async fn discover_saved_api_models(
    reference: &CredentialRef,
    base: &str,
    format: ApiFormat,
) -> Result<Vec<String>, ModelError> {
    api_model_ids(&discover_saved_api_catalog(reference, base, format).await?)
}

pub async fn discover_api_catalog(
    base: &str,
    format: ApiFormat,
    key: &str,
) -> Result<Value, ModelError> {
    discover_api_catalog_inner(base, format, key, None).await
}

pub async fn discover_saved_api_catalog(
    reference: &CredentialRef,
    base: &str,
    format: ApiFormat,
) -> Result<Value, ModelError> {
    discover_api_catalog_inner(base, format, "", Some(reference)).await
}

async fn discover_api_catalog_inner(
    base: &str,
    format: ApiFormat,
    key: &str,
    reference: Option<&CredentialRef>,
) -> Result<Value, ModelError> {
    if !matches!(
        format,
        ApiFormat::OpenaiChatCompletions
            | ApiFormat::OpenaiResponses
            | ApiFormat::AnthropicMessages
    ) {
        return Err(ModelError::Protocol(
            "unsupported API discovery protocol".into(),
        ));
    }

    let mut endpoint = reqwest::Url::parse(base)
        .map_err(|_| ModelError::Protocol("invalid service URL".into()))?;
    if !matches!(endpoint.scheme(), "http" | "https")
        || endpoint.host_str().is_none()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
    {
        return Err(ModelError::Protocol(
            "use an HTTP(S) base URL without credentials, query or fragment".into(),
        ));
    }

    endpoint.set_path(&format!("{}/models", endpoint.path().trim_end_matches('/')));
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(DISCOVERY_TIMEOUT)
        .build()?;
    let auth = if let Some(reference) = reference {
        let target = if format == ApiFormat::AnthropicMessages {
            AuthTarget::Anthropic
        } else {
            AuthTarget::Openai
        };
        Some(
            AuthStore::new(reference.directory.clone())?
                .resolve(&reference.name, &endpoint, target)
                .await?,
        )
    } else {
        None
    };
    let mut request = client.get(endpoint).header("accept", "application/json");
    if let Some(auth) = auth {
        request = request.headers(auth.headers);
    }
    if format == ApiFormat::AnthropicMessages {
        request = request.header("anthropic-version", "2023-06-01");
        if !key.is_empty() {
            request = request.header("x-api-key", key);
        }
    } else if !key.is_empty() {
        request = request.bearer_auth(key);
    }

    let request = request.build()?;
    let deadline = std::time::Instant::now() + DISCOVERY_TIMEOUT;
    let mut cursor = None::<String>;
    let mut seen = std::collections::HashSet::new();
    let mut all = Vec::new();
    let mut size = 0usize;
    for _ in 0..100 {
        let mut page = request.try_clone().expect("model discovery GET request");
        if let Some(cursor) = &cursor {
            page.url_mut()
                .query_pairs_mut()
                .append_pair("after_id", cursor);
        }

        *page.timeout_mut() = Some(
            deadline
                .checked_duration_since(std::time::Instant::now())
                .ok_or_else(|| ModelError::Protocol("model discovery timed out".into()))?,
        );
        let mut response = client
            .execute(page)
            .await
            .map_err(|_| ModelError::Protocol("model discovery request failed".into()))?;

        if !response.status().is_success() {
            return Err(ModelError::Protocol(format!(
                "model discovery returned HTTP {}",
                response.status()
            )));
        }

        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            size = size.saturating_add(chunk.len());
            if size > 4 * 1024 * 1024 {
                return Err(ModelError::Protocol("model catalog is too large".into()));
            }
            bytes.extend_from_slice(&chunk);
        }

        let mut payload: Value = serde_json::from_slice(&bytes)
            .map_err(|_| ModelError::Protocol("model catalog is not JSON".into()))?;
        let rows = payload["data"]
            .as_array()
            .ok_or_else(|| ModelError::Protocol("model catalog has no data array".into()))?;
        all.extend(rows.iter().cloned());
        if payload["has_more"] != true {
            payload["data"] = Value::Array(all);
            api_model_ids(&payload)?;
            return Ok(payload);
        }

        let next = payload["last_id"]
            .as_str()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| ModelError::Protocol("model catalog pagination has no cursor".into()))?;

        if !seen.insert(next.to_owned()) {
            return Err(ModelError::Protocol(
                "model catalog pagination repeated its cursor".into(),
            ));
        }

        cursor = Some(next.to_owned());
    }

    Err(ModelError::Protocol(
        "model catalog has too many pages".into(),
    ))
}

fn api_model_ids(payload: &Value) -> Result<Vec<String>, ModelError> {
    let rows = payload["data"]
        .as_array()
        .ok_or_else(|| ModelError::Protocol("model catalog has no data array".into()))?;
    let mut ids = Vec::new();
    for row in rows {
        if let Some(id) = row["id"].as_str()
            && !id.trim().is_empty()
            && id.len() <= 256
            && !id.chars().any(char::is_control)
            && !ids.iter().any(|existing| existing == id)
        {
            ids.push(id.to_owned());
        }
    }

    if ids.is_empty() {
        return Err(ModelError::Protocol(
            "model catalog is empty; enter model IDs manually".into(),
        ));
    }

    Ok(ids)
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    async fn catalog_server(listener: tokio::net::TcpListener, status: &str, body: &str) -> String {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(socket.read_u8().await.unwrap());
        }
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
        String::from_utf8(request).unwrap().to_ascii_lowercase()
    }

    #[tokio::test]
    async fn custom_catalog_preserves_prefix_and_selects_protocol_auth() {
        for (format, key, header) in [
            (
                ApiFormat::OpenaiResponses,
                "test-key",
                Some("authorization: bearer test-key"),
            ),
            (
                ApiFormat::AnthropicMessages,
                "test-key",
                Some("x-api-key: test-key"),
            ),
            (ApiFormat::OpenaiChatCompletions, "", None),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}/custom/v1/", listener.local_addr().unwrap());
            let server = catalog_server(
                listener,
                "200 OK",
                r#"{"data":[{"id":"a"},{"id":"a"},{"id":"b"}]}"#,
            );

            let (result, request) = tokio::join!(discover_api_models(&base, format, key), server);

            assert_eq!(result.unwrap(), ["a", "b"]);
            assert!(request.starts_with("get /custom/v1/models "), "{request}");
            if let Some(header) = header {
                assert!(request.contains(header), "{request}");
            } else {
                assert!(
                    !request.contains("authorization:") && !request.contains("x-api-key:"),
                    "{request}"
                );
            }
        }
    }

    #[tokio::test]
    async fn api_catalog_reads_all_pages_and_retains_capabilities() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/orbit/v1", listener.local_addr().unwrap());
        let server = async {
            let mut requests = Vec::new();
            for body in [
                r#"{"data":[{"id":"orbit-a","max_input_tokens":200000}],"has_more":true,"last_id":"orbit-a"}"#,
                r#"{"data":[{"id":"orbit-b","capabilities":{"image_input":{"supported":true}}}],"has_more":false,"last_id":"orbit-b"}"#,
            ] {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(socket.read_u8().await.unwrap());
                }
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
                requests.push(String::from_utf8(request).unwrap().to_ascii_lowercase());
            }

            requests
        };
        let (result, requests) = tokio::join!(
            discover_api_catalog(&base, ApiFormat::AnthropicMessages, "orbit-fixture-key"),
            server
        );
        let payload = result.unwrap();
        assert_eq!(api_model_ids(&payload).unwrap(), ["orbit-a", "orbit-b"]);
        assert_eq!(payload["data"][0]["max_input_tokens"], 200000);
        assert_eq!(
            payload["data"][1]["capabilities"]["image_input"]["supported"],
            true
        );
        assert!(requests[0].starts_with("get /orbit/v1/models "));
        assert!(requests[1].starts_with("get /orbit/v1/models?after_id=orbit-a "));
        assert!(
            requests
                .iter()
                .all(|request| request.contains("x-api-key: orbit-fixture-key"))
        );
    }

    #[tokio::test]
    async fn pagination_with_no_cursor_does_not_publish_a_partial_catalog() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/orbit/v1", listener.local_addr().unwrap());
        let server = catalog_server(
            listener,
            "200 OK",
            r#"{"data":[{"id":"orbit-a"}],"has_more":true}"#,
        );
        let (result, _) = tokio::join!(
            discover_api_catalog(&base, ApiFormat::AnthropicMessages, "orbit-fixture-key"),
            server
        );
        assert!(result.unwrap_err().to_string().contains("cursor"));
    }

    #[tokio::test]
    async fn discovery_failure_does_not_expose_response_body_or_infer_models() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = catalog_server(listener, "403 Forbidden", "private-response");

        let (result, _) = tokio::join!(
            discover_api_models(&base, ApiFormat::OpenaiResponses, "secret"),
            server
        );

        let error = result.unwrap_err().to_string();
        assert!(error.contains("403"));
        assert!(!error.contains("private-response") && !error.contains("secret"));
    }
}
