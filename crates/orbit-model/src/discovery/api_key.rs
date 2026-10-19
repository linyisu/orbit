//! Verify official key access using the same paginated catalog as ordinary refreshes.
use crate::{client::ApiFormat, error::ModelError};
use orbit_auth::ApiKeyProvider;
use serde_json::json;

/// Discover before probing plan inference so the probe never pins a model ID in source.
pub async fn verify(provider: ApiKeyProvider, key: &str) -> Result<Vec<String>, ModelError> {
    Ok(verify_catalog(provider, key)
        .await?
        .models
        .into_iter()
        .map(|model| model.id)
        .collect())
}

/// Preserve live capability metadata when verifying an official API connection.
pub async fn verify_catalog(
    provider: ApiKeyProvider,
    key: &str,
) -> Result<crate::catalog::ModelCatalog, ModelError> {
    verify_at(provider, key, provider.base_url()).await
}

async fn verify_at(
    provider: ApiKeyProvider,
    key: &str,
    base: &str,
) -> Result<crate::catalog::ModelCatalog, ModelError> {
    const MAX_API_KEY_BYTES: usize = 16 * 1_024;
    let key = key.trim();
    if key.is_empty() || key.len() > MAX_API_KEY_BYTES || key.chars().any(char::is_control) {
        return Err(orbit_auth::AuthError::InvalidToken.into());
    }
    let payload = super::discover_api_catalog(base, ApiFormat::OpenaiChatCompletions, key).await?;
    let catalog = crate::catalog::parse(ApiFormat::OpenaiChatCompletions, &payload)
        .map_err(|error| ModelError::Protocol(error.to_string()))?;
    if provider == ApiKeyProvider::ZhipuCodingPlan {
        let model = catalog
            .models
            .first()
            .ok_or_else(|| ModelError::Protocol("账号未返回可用于验证套餐的模型".into()))?;
        let response = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(super::DISCOVERY_TIMEOUT)
            .build()?
            .post(format!("{base}chat/completions"))
            .bearer_auth(key)
            .json(&json!({
                "model": model.id,
                "messages": [{ "role": "user", "content": "ping" }],
                "max_tokens": 1, "temperature": 0, "stream": false,
            }))
            .send()
            .await?;
        let payload = super::read_json(response, &mut 0).await?;
        if payload.get("error").is_some() || !payload["choices"].is_array() {
            return Err(ModelError::Protocol(
                "coding plan verification failed".into(),
            ));
        }
    }
    Ok(catalog)
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;

    async fn serve_requests(
        listener: tokio::net::TcpListener,
        responses: Vec<(u16, &'static str)>,
    ) -> Vec<(String, Value)> {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut requests = Vec::new();
        for (status, body) in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            while !bytes.ends_with(b"\r\n\r\n") {
                bytes.push(socket.read_u8().await.unwrap());
            }
            let headers = String::from_utf8(bytes)
                .unwrap()
                .to_ascii_lowercase();
            let length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .map_or(0, |length| length.parse::<usize>().unwrap());
            let mut request_body = vec![0; length];
            socket
                .read_exact(&mut request_body)
                .await
                .unwrap();
            requests.push((
                headers,
                serde_json::from_slice(&request_body).unwrap_or(Value::Null),
            ));
            let response = format!(
                "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            socket
                .write_all(response.as_bytes())
                .await
                .unwrap();
        }

        requests
    }

    #[tokio::test]
    async fn zhipu_probes_only_a_model_returned_by_live_discovery() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let base = format!(
            "http://{}/api/coding/paas/v4/",
            listener.local_addr().unwrap()
        );
        let server = serve_requests(
            listener,
            vec![
                (200, r#"{"data":[{"id":"generated-server-model"}]}"#),
                (200, r#"{"choices":[{"message":{"content":"ok"}}]}"#),
            ],
        );

        let (result, requests) = tokio::join!(
            verify_at(ApiKeyProvider::ZhipuCodingPlan, "test.secret", &base),
            server
        );

        assert_eq!(result.unwrap().models[0].id, "generated-server-model");
        assert!(
            requests[1]
                .0
                .starts_with("post /api/coding/paas/v4/chat/completions ")
        );
        assert_eq!(requests[1].1["model"], "generated-server-model");
        assert_eq!(requests[1].1["max_tokens"], 1);
        assert!(
            requests[0]
                .0
                .starts_with("get /api/coding/paas/v4/models ")
        );
        assert!(
            requests
                .iter()
                .all(|(headers, _)| headers.contains("authorization: bearer test.secret"))
        );
        assert_eq!(
            ApiKeyProvider::ZhipuCodingPlan.base_url(),
            orbit_auth::ZHIPU_CODING_BASE
        );
    }

    #[tokio::test]
    async fn zhipu_rejection_stops_without_another_billing_endpoint() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let base = format!("http://{}/", listener.local_addr().unwrap());
        let server = serve_requests(
            listener,
            vec![(403, r#"{"error":{"message":"private-key"}}"#)],
        );

        let (result, requests) = tokio::join!(
            verify_at(ApiKeyProvider::ZhipuCodingPlan, "test.secret", &base),
            server
        );

        assert!(matches!(result, Err(ModelError::Http { status: 403, .. })));
        assert_eq!(requests.len(), 1);
        assert!(!format!("{result:?}").contains("private-key"));
    }
}
