//! Official API-key verification and model discovery.
use serde_json::{Value, json};

use crate::{AuthError, Result, http};

pub const DEEPSEEK_BASE: &str = "https://api.deepseek.com/v1/";

pub const ZHIPU_CODING_BASE: &str = "https://open.bigmodel.cn/api/coding/paas/v4/";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiKeyProvider {
    Deepseek,
    ZhipuCodingPlan,
}

impl ApiKeyProvider {
    pub fn name(self) -> &'static str {
        match self {
            Self::Deepseek => "deepseek",
            Self::ZhipuCodingPlan => "zhipu-coding-plan",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Deepseek => "DeepSeek API",
            Self::ZhipuCodingPlan => "智谱 Coding Plan",
        }
    }

    pub fn base_url(self) -> &'static str {
        match self {
            Self::Deepseek => DEEPSEEK_BASE,
            Self::ZhipuCodingPlan => ZHIPU_CODING_BASE,
        }
    }

    /// Verify before persisting. Zhipu uses OMP's one-token coding endpoint
    /// probe because listing models alone does not establish plan access.
    pub async fn verify(self, key: &str) -> Result<Vec<String>> {
        verify_at(self, key, self.base_url()).await
    }
}

pub async fn verify_deepseek(key: &str) -> Result<Vec<String>> {
    ApiKeyProvider::Deepseek
        .verify(key)
        .await
}

async fn verify_at(provider: ApiKeyProvider, key: &str, base: &str) -> Result<Vec<String>> {
    let key = key.trim();
    if key.is_empty() || key.len() > 16_384 || key.chars().any(char::is_control) {
        return Err(AuthError::InvalidToken);
    }
    let client = http::client()?;

    if provider == ApiKeyProvider::ZhipuCodingPlan {
        let response = client
            .post(format!("{base}chat/completions"))
            .bearer_auth(key)
            .json(&json!({
                "model": "glm-5.1",
                "messages": [{
                    "role": "user",
                    "content": "ping"
                }],
                "max_tokens": 1,
                "temperature": 0,
                "stream": false
            }))
            .send()
            .await
            .map_err(|_| AuthError::Transport)?;
        let payload = http::json_response(response).await?;
        if payload.get("error").is_some() || !payload["choices"].is_array() {
            return Err(AuthError::Configuration("coding plan verification failed"));
        }
    }

    let response = client
        .get(format!("{base}models"))
        .bearer_auth(key)
        .send()
        .await
        .map_err(|_| AuthError::Transport)?;
    model_ids(&http::json_response(response).await?)
}

fn model_ids(payload: &Value) -> Result<Vec<String>> {
    let rows = payload["data"]
        .as_array()
        .ok_or(AuthError::InvalidToken)?;
    let mut ids = Vec::new();
    for row in rows {
        let id = row["id"]
            .as_str()
            .ok_or(AuthError::InvalidToken)?;
        if id.trim().is_empty() || id.chars().any(char::is_control) {
            return Err(AuthError::InvalidToken);
        }
        if !ids
            .iter()
            .any(|existing| existing == id)
        {
            ids.push(id.to_owned());
        }
    }

    if ids.is_empty() {
        return Err(AuthError::Configuration(
            "account returned no available models",
        ));
    }

    Ok(ids)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn model_list_uses_server_ids_and_rejects_invalid_responses() {
        let payload = json!({
            "data": [
                {
                    "id": "new-model"
                },
                {
                    "id": "new-model"
                },
                {
                    "id": "other-model"
                }
            ]
        });

        let ids = model_ids(&payload).unwrap();

        assert_eq!(ids, ["new-model", "other-model"]);
        for invalid in [
            json!({}),
            json!({
                "data": []
            }),
            json!({
                "data": [{
                    "id": "bad\nmodel"
                }]
            }),
        ] {
            assert!(model_ids(&invalid).is_err());
        }
    }

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
    async fn zhipu_checks_plan_access_before_fetching_real_models() {
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
                (200, r#"{"choices":[{"message":{"content":"ok"}}]}"#),
                (200, r#"{"data":[{"id":"glm-server-model"}]}"#),
            ],
        );

        let (result, requests) = tokio::join!(
            verify_at(ApiKeyProvider::ZhipuCodingPlan, "test.secret", &base),
            server
        );

        assert_eq!(result.unwrap(), ["glm-server-model"]);
        assert!(
            requests[0]
                .0
                .starts_with("post /api/coding/paas/v4/chat/completions ")
        );
        assert_eq!(requests[0].1["model"], "glm-5.1");
        assert_eq!(requests[0].1["max_tokens"], 1);
        assert!(
            requests[1]
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
            ZHIPU_CODING_BASE
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

        assert!(matches!(result, Err(AuthError::Http(403))));
        assert_eq!(requests.len(), 1);
        assert!(!format!("{result:?}").contains("private-key"));
    }
}
