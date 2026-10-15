//! Authenticated catalog retrieval with bounded pagination and no bundled model allowlist.
mod api_key;
pub use api_key::verify as verify_api_key;

use std::time::Duration;

use orbit_auth::{AuthStore, AuthTarget, CredentialRef};
use serde_json::{Value, json};

use crate::{client::ApiFormat, error::ModelError};

const MAX_CATALOG_BYTES: usize = 4 * 1_024 * 1_024;
const MAX_CATALOG_PAGES: usize = 100;
const MAX_PAGINATION_CURSOR_BYTES: usize = 4_096;

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);

/// Compatibility identity retained only for legacy Codex inference requests.
pub const CODEX_CLIENT_VERSION: &str = "0.153.0";

/// Whether this wire format has a supported discovery transport for the configured endpoint.
pub fn supports(format: ApiFormat, base: &str) -> bool {
    if !format.requires_saved_credential() {
        return true;
    }
    matches!(
        (format, base.trim_end_matches('/')),
        (
            ApiFormat::GoogleCodeAssist,
            "https://daily-cloudcode-pa.googleapis.com" | "https://cloudcode-pa.googleapis.com",
        ) | (
            ApiFormat::ChatgptCodex,
            "https://chatgpt.com/backend-api/codex" | "https://api.openai.com/v1",
        )
    )
}

/// Only fixed account endpoints are accepted; credentials never follow redirects.
pub async fn discover(
    reference: &CredentialRef,
    format: ApiFormat,
    base: &str,
) -> Result<Value, ModelError> {
    let (target, path) = match (format, base.trim_end_matches('/')) {
        (ApiFormat::GoogleCodeAssist, "https://daily-cloudcode-pa.googleapis.com") => (
            AuthTarget::GoogleCodeAssist,
            "https://daily-cloudcode-pa.googleapis.com/v1internal:fetchAvailableModels",
        ),

        (ApiFormat::GoogleCodeAssist, "https://cloudcode-pa.googleapis.com") => (
            AuthTarget::GoogleCodeAssist,
            "https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota",
        ),

        (ApiFormat::ChatgptCodex, "https://api.openai.com/v1") => {
            (AuthTarget::ChatgptCodex, "https://api.openai.com/v1/models")
        }
        (ApiFormat::ChatgptCodex, "https://chatgpt.com/backend-api/codex") => {
            return Err(orbit_auth::AuthError::ReauthorizationRequired.into());
        }

        _ => {
            return Err(ModelError::Protocol(
                "account discovery is unavailable for this endpoint".into(),
            ));
        }
    };
    if base.trim_end_matches('/') == "https://daily-cloudcode-pa.googleapis.com" {
        orbit_auth::refresh_antigravity_client_version().await;
    }
    let endpoint = reqwest::Url::parse(path)
        .map_err(|_| ModelError::Protocol("invalid account endpoint".into()))?;
    let auth = AuthStore::new(reference.directory.clone())?
        .resolve(&reference.name, &endpoint, target)
        .await?;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(DISCOVERY_TIMEOUT)
        .build()?;
    let request = if format == ApiFormat::GoogleCodeAssist {
        let body = if base.trim_end_matches('/') == "https://cloudcode-pa.googleapis.com" {
            json!({
                "project": auth.project_id
            })
        } else {
            json!({})
        };
        client.post(path).json(&body)
    } else {
        client.get(path)
    };
    let request = request
        .headers(auth.headers.clone())
        .header("accept", "application/json")
        .build()?;
    if format == ApiFormat::GoogleCodeAssist {
        fetch_account_object(&client, request).await
    } else {
        read_pages(&client, request, format).await
    }
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
        let target = format.auth_target();
        Some(
            AuthStore::new(reference.directory.clone())?
                .resolve(&reference.name, &endpoint, target)
                .await?,
        )
    } else {
        None
    };
    let mut request = client
        .get(endpoint)
        .header("accept", "application/json");
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

    read_pages(&client, request.build()?, format).await
}

async fn fetch_account_object(
    client: &reqwest::Client,
    request: reqwest::Request,
) -> Result<Value, ModelError> {
    let response = client.execute(request).await?;
    let mut size = 0;
    read_json(response, &mut size).await
}

/// Bound all pages by one deadline and byte budget; never publish an incomplete catalog.
async fn read_pages(
    client: &reqwest::Client,
    request: reqwest::Request,
    format: ApiFormat,
) -> Result<Value, ModelError> {
    let deadline = std::time::Instant::now() + DISCOVERY_TIMEOUT;
    let mut cursor = None::<String>;
    let mut seen = std::collections::HashSet::new();
    let mut all = Vec::new();
    let mut size = 0usize;
    for _ in 0..MAX_CATALOG_PAGES {
        let mut page = request
            .try_clone()
            .expect("model discovery GET request");
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
        let response = client
            .execute(page)
            .await
            .map_err(|_| ModelError::Protocol("model discovery request failed".into()))?;
        let mut payload = read_json(response, &mut size).await?;
        let field = if format == ApiFormat::ChatgptCodex && payload["models"].is_array() {
            "models"
        } else {
            "data"
        };
        let rows = payload[field]
            .as_array()
            .ok_or_else(|| ModelError::Protocol("model catalog has no data array".into()))?;
        all.extend(rows.iter().cloned());
        let more = match payload.get("has_more") {
            Some(Value::Bool(more)) => *more,
            None | Some(Value::Null) => false,
            _ => {
                return Err(ModelError::Protocol(
                    "model catalog pagination marker is invalid".into(),
                ));
            }
        };
        if !more {
            payload[field] = Value::Array(all);
            if format != ApiFormat::ChatgptCodex {
                api_model_ids(&payload)?;
            }
            return Ok(payload);
        }

        let next = payload["last_id"]
            .as_str()
            .filter(|value| {
                !value.is_empty()
                    && value.len() <= MAX_PAGINATION_CURSOR_BYTES
                    && !value.chars().any(char::is_control)
            })
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

async fn read_json(mut response: reqwest::Response, size: &mut usize) -> Result<Value, ModelError> {
    if !response.status().is_success() {
        return Err(ModelError::Http {
            status: response.status().as_u16(),
            request_id: String::new(),
            body: "model discovery failed".into(),
            retryable: false,
            context_limit: false,
        });
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        *size = size.saturating_add(chunk.len());
        if *size > MAX_CATALOG_BYTES {
            return Err(ModelError::Protocol("model catalog is too large".into()));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| ModelError::Protocol("model catalog is not JSON".into()))
}

fn api_model_ids(payload: &Value) -> Result<Vec<String>, ModelError> {
    let rows = payload["data"]
        .as_array()
        .ok_or_else(|| ModelError::Protocol("model catalog has no data array".into()))?;
    let mut ids = Vec::new();
    for row in rows {
        let id = row["id"]
            .as_str()
            .filter(|id| crate::catalog::valid_model_id(id))
            .ok_or_else(|| ModelError::Protocol("model catalog contains an invalid ID".into()))?;
        if !ids
            .iter()
            .any(|existing| existing == id)
        {
            ids.push(id.to_owned());
        }
    }

    Ok(ids)
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;

    async fn catalog_server(
        listener: &tokio::net::TcpListener,
        status: &str,
        body: &str,
    ) -> String {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.ends_with(b"\r\n\r\n") {
            request.push(socket.read_u8().await.unwrap());
        }
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        socket
            .write_all(response.as_bytes())
            .await
            .unwrap();
        String::from_utf8(request)
            .unwrap()
            .to_ascii_lowercase()
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
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
                .await
                .unwrap();
            let base = format!("http://{}/custom/v1/", listener.local_addr().unwrap());
            let server = catalog_server(
                &listener,
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
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
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
                socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: \
                    close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
                requests.push(
                    String::from_utf8(request)
                        .unwrap()
                        .to_ascii_lowercase(),
                );
            }

            requests
        };
        let (result, requests) = tokio::join!(
            discover_api_catalog(&base, ApiFormat::AnthropicMessages, "orbit-fixture-key"),
            server
        );
        let payload = result.unwrap();
        assert_eq!(api_model_ids(&payload).unwrap(), ["orbit-a", "orbit-b"]);
        assert_eq!(payload["data"][0]["max_input_tokens"], 200_000);
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
    async fn subscription_catalog_preserves_server_slugs_across_pages() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let client = reqwest::Client::new();
        let request = client
            .get(format!(
                "http://{}/v1/models",
                listener.local_addr().unwrap()
            ))
            .bearer_auth("generated-access")
            .build()
            .unwrap();
        let server = async {
            let mut requests = Vec::new();
            for (slug, more) in [("generated-first", true), ("generated-next", false)] {
                let body = json!({
                    "models": [{ "slug": slug, "visibility": "list" }],
                    "has_more": more,
                    "last_id": slug,
                })
                .to_string();
                requests.push(catalog_server(&listener, "200 OK", &body).await);
            }
            requests
        };
        let (payload, requests) = tokio::join!(
            read_pages(&client, request, ApiFormat::ChatgptCodex),
            server
        );
        let catalog = crate::catalog::parse(ApiFormat::ChatgptCodex, &payload.unwrap()).unwrap();
        assert_eq!(
            catalog
                .models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            ["generated-first", "generated-next"]
        );
        assert!(requests[1].starts_with("get /v1/models?after_id=generated-first "));
        assert!(
            requests
                .iter()
                .all(|request| { request.contains("authorization: bearer generated-access") })
        );
    }

    #[tokio::test]
    async fn pagination_with_no_cursor_does_not_publish_a_partial_catalog() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let base = format!("http://{}/orbit/v1", listener.local_addr().unwrap());
        let server = catalog_server(
            &listener,
            "200 OK",
            r#"{"data":[{"id":"orbit-a"}],"has_more":true}"#,
        );
        let (result, _) = tokio::join!(
            discover_api_catalog(&base, ApiFormat::AnthropicMessages, "orbit-fixture-key"),
            server
        );
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("cursor")
        );
    }

    #[tokio::test]
    async fn discovery_failure_does_not_expose_response_body_or_infer_models() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = catalog_server(&listener, "403 Forbidden", "private-response");

        let (result, _) = tokio::join!(
            discover_api_models(&base, ApiFormat::OpenaiResponses, "secret"),
            server
        );

        let error = result.unwrap_err().to_string();
        assert!(error.contains("403"));
        assert!(!error.contains("private-response") && !error.contains("secret"));
    }
}
