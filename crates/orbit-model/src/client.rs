use std::time::Duration;

use async_stream::stream;
use futures_util::StreamExt;
use reqwest::{Client, Response, Url, header};
use serde::Deserialize;
use tokio::{
    select,
    time::{self, error::Elapsed},
};
use tokio_util::sync::CancellationToken;

use crate::{error::ModelError, types::*};

#[derive(Clone, Copy, Debug, Default, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApiFormat {
    #[default]
    OpenaiChatCompletions,
    OpenaiResponses,
    AnthropicMessages,
}

impl ApiFormat {
    pub fn parse(value: &str) -> Result<Self, ModelError> {
        match value {
            "openai_chat_completions" => Ok(Self::OpenaiChatCompletions),
            "openai_responses" => Ok(Self::OpenaiResponses),
            "anthropic_messages" => Ok(Self::AnthropicMessages),
            other => Err(ModelError::UnsupportedApiFormat(other.to_owned())),
        }
    }
}

#[derive(Clone)]
pub struct ModelClient {
    http: Client,
    config: ModelConfig,
    endpoint: Url,
}

impl std::fmt::Debug for ModelClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModelClient")
            .field("config", &self.config)
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl ModelClient {
    pub fn new(config: ModelConfig) -> Result<Self, ModelError> {
        if config.model.trim().is_empty() {
            return Err(ModelError::InvalidModel);
        }

        let format = config.api_format;
        let suffix = match format {
            ApiFormat::OpenaiChatCompletions => "chat/completions",
            ApiFormat::OpenaiResponses => "responses",
            ApiFormat::AnthropicMessages => "messages",
        };

        let endpoint = Url::parse(&format!(
            "{}/{suffix}",
            config.base_url.trim_end_matches('/')
        ))
        .map_err(|error| ModelError::InvalidBaseUrl(error.to_string()))?;

        if !matches!(endpoint.scheme(), "http" | "https") {
            return Err(ModelError::InvalidBaseUrl(
                "URL scheme must be http or https".to_owned(),
            ));
        }

        let http = Client::builder().connect_timeout(config.timeout).build()?;

        Ok(Self {
            http,
            config,
            endpoint,
        })
    }

    pub fn config(&self) -> &ModelConfig {
        &self.config
    }

    pub async fn complete(&self, request: ChatRequest) -> Result<ChatCompletion, ModelError> {
        self.complete_with_cancel(request, CancellationToken::new())
            .await
    }

    pub async fn complete_with_cancel(
        &self,
        request: ChatRequest,
        cancellation: CancellationToken,
    ) -> Result<ChatCompletion, ModelError> {
        let response = self.send(request, false, cancellation).await?;

        match self.config.api_format {
            ApiFormat::OpenaiChatCompletions => {
                crate::openai::chat_completions::decode_completion(response).await
            }
            ApiFormat::OpenaiResponses => {
                crate::openai::responses::decode_completion(response).await
            }
            ApiFormat::AnthropicMessages => {
                crate::anthropic::messages::decode_completion(response).await
            }
        }
    }

    pub async fn stream(&self, request: ChatRequest) -> Result<ModelStream, ModelError> {
        self.stream_with_cancel(request, CancellationToken::new())
            .await
    }

    pub async fn stream_with_cancel(
        &self,
        request: ChatRequest,
        cancellation: CancellationToken,
    ) -> Result<ModelStream, ModelError> {
        let response = self.send(request, true, cancellation.clone()).await?;
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();

        if !content_type.contains("text/event-stream") {
            return Err(ModelError::Protocol(
                "streaming endpoint did not return text/event-stream".to_owned(),
            ));
        }

        match self.config.api_format {
            ApiFormat::OpenaiChatCompletions => Ok(crate::openai::chat_completions::stream(
                response,
                self.config.timeout,
                cancellation,
            )),
            ApiFormat::OpenaiResponses => Ok(crate::openai::responses::stream(
                response,
                self.config.timeout,
                cancellation,
            )),
            ApiFormat::AnthropicMessages => Ok(crate::anthropic::messages::stream(
                response,
                self.config.timeout,
                cancellation,
            )),
        }
    }

    async fn send(
        &self,
        request: ChatRequest,
        stream: bool,
        cancellation: CancellationToken,
    ) -> Result<Response, ModelError> {
        let format = self.config.api_format;
        let (body, headers) = match format {
            ApiFormat::OpenaiChatCompletions => {
                crate::openai::chat_completions::encode(&self.config.model, request, stream)
            }
            ApiFormat::OpenaiResponses => {
                crate::openai::responses::encode(&self.config.model, request, stream)
            }
            ApiFormat::AnthropicMessages => {
                crate::anthropic::messages::encode(&self.config.model, request, stream)
            }
        };

        let mut builder = self
            .http
            .post(self.endpoint.clone())
            .headers(headers)
            .header(header::CONTENT_TYPE, "application/json")
            .json(&body);

        if let Some(api_key) = &self.config.api_key {
            builder = match format {
                ApiFormat::AnthropicMessages => builder.header("x-api-key", api_key),
                _ => builder.bearer_auth(api_key),
            };
        }

        let response = select! {
            _ = cancellation.cancelled() => return Err(ModelError::Cancelled),
            result = time::timeout(self.config.timeout, builder.send()) => {
                match result {
                    Ok(result) => result?,
                    Err(Elapsed { .. }) => {
                        return Err(ModelError::Timeout(self.config.timeout));
                    }
                }
            }
        };

        if response.status().is_success() {
            return Ok(response);
        }

        let status = response.status().as_u16();
        let request_id = response
            .headers()
            .get("x-request-id")
            .or_else(|| response.headers().get("request-id"))
            .and_then(|value| value.to_str().ok())
            .map(|value| format!(" [request {value}]"))
            .unwrap_or_default();
        let body = response
            .text()
            .await
            .unwrap_or_else(|_| "<unreadable response body>".to_owned());

        Err(ModelError::Http {
            status,
            request_id,
            body: truncate_body(&body, 2_000),
            retryable: matches!(status, 408 | 425 | 429 | 500 | 502 | 503 | 504 | 529),
        })
    }
}

pub(crate) fn sse_stream<F>(
    response: Response,
    timeout: Duration,
    cancellation: CancellationToken,
    decode: F,
) -> ModelStream
where
    F: Fn(&str) -> Result<Vec<StreamEvent>, ModelError> + Send + Sync + 'static,
{
    let mut body = response.bytes_stream();

    Box::pin(stream! {
        let mut buffer = String::new();

        loop {
            let next = select! {
                _ = cancellation.cancelled() => Err(ModelError::Cancelled),
                chunk = time::timeout(timeout, body.next()) => {
                    match chunk {
                        Ok(Some(Ok(chunk))) => Ok(Some(chunk)),
                        Ok(Some(Err(error))) => Err(ModelError::Request(error)),
                        Ok(None) => Ok(None),
                        Err(Elapsed { .. }) => Err(ModelError::Timeout(timeout)),
                    }
                }
            };

            let next = match next {
                Ok(next) => next,
                Err(error) => {
                    yield Err(error);
                    break;
                }
            };

            let Some(chunk) = next else {
                break;
            };

            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(end) = find_sse_separator(&buffer) {
                let separator_len = if buffer[end..].starts_with("\r\n\r\n") {
                    4
                } else {
                    2
                };
                let frame = buffer.drain(..end + separator_len).collect::<String>();

                if let Some(data) = sse_data(&frame) {
                    if data == "[DONE]" {
                        return;
                    }

                    match decode(&data) {
                        Ok(events) => {
                            for event in events {
                                yield Ok(event);
                            }
                        }
                        Err(error) => {
                            yield Err(error);
                            return;
                        }
                    }
                }
            }
        }

        if !buffer.trim().is_empty() {
            yield Err(ModelError::IncompleteSseEvent);
        }
    })
}

pub(crate) fn find_sse_separator(buffer: &str) -> Option<usize> {
    let lf = buffer.find("\n\n");
    let crlf = buffer.find("\r\n\r\n");

    match (lf, crlf) {
        (Some(lf), Some(crlf)) => Some(lf.min(crlf)),
        (Some(index), None) | (None, Some(index)) => Some(index),
        (None, None) => None,
    }
}

pub(crate) fn sse_data(frame: &str) -> Option<String> {
    let mut data = String::new();

    for line in frame.lines() {
        if let Some(value) = line.strip_prefix("data:") {
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(value.trim_start());
        }
    }

    (!data.is_empty()).then_some(data)
}

pub(crate) fn truncate_body(body: &str, limit: usize) -> String {
    let mut end = body.len().min(limit);

    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }

    let mut result = body[..end].to_owned();
    if end < body.len() {
        result.push('…');
    }

    result
}

pub(crate) fn invalid_response(message: &str) -> ModelError {
    ModelError::Protocol(message.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use std::sync::Arc;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };

    #[test]
    fn parses_all_supported_api_formats() {
        assert_eq!(
            ApiFormat::parse("openai_chat_completions").unwrap(),
            ApiFormat::OpenaiChatCompletions
        );
        assert_eq!(
            ApiFormat::parse("openai_responses").unwrap(),
            ApiFormat::OpenaiResponses
        );
        assert_eq!(
            ApiFormat::parse("anthropic_messages").unwrap(),
            ApiFormat::AnthropicMessages
        );
        assert!(matches!(
            ApiFormat::parse("unknown"),
            Err(ModelError::UnsupportedApiFormat(_))
        ));
    }

    #[test]
    fn model_config_debug_redacts_api_key() {
        let config = ModelConfig {
            api_format: ApiFormat::OpenaiChatCompletions,
            base_url: "https://api.openai.com/v1".to_owned(),
            api_key: Some("secret-value".to_owned()),
            model: "mock-model".to_owned(),
            timeout: Duration::from_secs(120),
        };
        let debug = format!("{config:?}");

        assert!(!debug.contains("secret-value"));
        assert!(debug.contains("[redacted]"));
    }

    #[tokio::test]
    async fn openai_chat_uses_chat_completions_and_bearer_auth() {
        let (base_url, request) = test_server(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n{\"id\":\"chat-1\",\"model\":\"mock\",\"choices\":[{\"message\":{\"role\":\"assistant\",\"content\":\"hello\"},\"finish_reason\":\"stop\"}]}",
        )
        .await;
        let client = client(
            &base_url,
            "openai_chat_completions",
            "mock-model",
            Some("openai-key"),
        );
        let completion = client
            .complete(ChatRequest::new(vec![ChatMessage::user("hello")]))
            .await
            .expect("completion succeeds");

        assert_eq!(completion.message, ChatMessage::assistant("hello"));

        let request = request.await;
        assert!(request.starts_with("post /chat/completions http/1.1"));
        assert!(request.contains("authorization: bearer openai-key"));
        assert!(request.contains("\"model\":\"mock-model\""));
    }

    #[tokio::test]
    async fn openai_responses_maps_output_text_and_uses_responses_path() {
        let (base_url, request) = test_server(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n{\"id\":\"resp-1\",\"model\":\"mock\",\"output_text\":\"hello from responses\",\"output\":[],\"status\":\"completed\",\"usage\":{\"input_tokens\":2,\"output_tokens\":3,\"total_tokens\":5}}",
        )
        .await;
        let client = client(
            &base_url,
            "openai_responses",
            "mock-responses",
            Some("responses-key"),
        );
        let completion = client
            .complete(ChatRequest::new(vec![
                ChatMessage::system("be concise"),
                ChatMessage::user("hello"),
            ]))
            .await
            .expect("completion succeeds");

        assert_eq!(
            completion.message,
            ChatMessage::assistant("hello from responses")
        );
        assert_eq!(completion.finish_reason.as_deref(), Some("stop"));

        let request = request.await;
        assert!(request.starts_with("post /responses http/1.1"));
        assert!(request.contains("authorization: bearer responses-key"));
        assert!(request.contains("\"instructions\":\"be concise\""));
        assert!(request.contains("\"input\""));
    }

    #[tokio::test]
    async fn anthropic_messages_maps_text_and_uses_anthropic_headers() {
        let (base_url, request) = test_server(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\r\n{\"id\":\"msg-1\",\"model\":\"claude\",\"content\":[{\"type\":\"text\",\"text\":\"hello from claude\"}],\"stop_reason\":\"end_turn\",\"usage\":{\"input_tokens\":4,\"output_tokens\":5}}",
        )
        .await;
        let client = client(
            &base_url,
            "anthropic_messages",
            "claude-sonnet",
            Some("anthropic-key"),
        );
        let completion = client
            .complete(ChatRequest::new(vec![
                ChatMessage::system("be concise"),
                ChatMessage::user("hello"),
            ]))
            .await
            .expect("completion succeeds");

        assert_eq!(
            completion.message,
            ChatMessage::assistant("hello from claude")
        );

        let request = request.await;
        assert!(request.starts_with("post /messages http/1.1"));
        assert!(request.contains("x-api-key: anthropic-key"));
        assert!(request.contains("anthropic-version: 2023-06-01"));
        assert!(request.contains("\"system\":\"be concise\""));
    }

    #[tokio::test]
    async fn responses_stream_maps_text_and_completion() {
        let (base_url, _) = test_server(concat!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"hello\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"usage\":{\"input_tokens\":1,\"output_tokens\":2,\"total_tokens\":3}}}\n\n",
        ))
        .await;
        let client = client(&base_url, "openai_responses", "mock-responses", None);
        let mut stream = client
            .stream(ChatRequest::new(vec![ChatMessage::user("hello")]))
            .await
            .expect("stream starts");
        let mut text = String::new();
        let mut completed = false;

        while let Some(event) = stream.next().await {
            match event.expect("stream event succeeds") {
                StreamEvent::TextDelta(delta) => text.push_str(&delta),
                StreamEvent::Completed { .. } => completed = true,
                _ => {}
            }
        }

        assert_eq!(text, "hello");
        assert!(completed);
    }

    fn client(base_url: &str, api_format: &str, model: &str, api_key: Option<&str>) -> ModelClient {
        ModelClient::new(ModelConfig {
            api_format: ApiFormat::parse(api_format).unwrap(),
            base_url: base_url.to_owned(),
            api_key: api_key.map(str::to_owned),
            model: model.to_owned(),
            timeout: Duration::from_secs(5),
        })
        .expect("client builds")
    }

    async fn test_server(
        response: impl Into<String>,
    ) -> (String, impl std::future::Future<Output = String>) {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind test server");
        let address = listener.local_addr().expect("local address");
        let response = Arc::new(response.into());
        let (sender, receiver) = tokio::sync::oneshot::channel();

        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept request");
            let mut bytes = Vec::new();
            let mut buffer = [0_u8; 4096];

            loop {
                let count = socket.read(&mut buffer).await.expect("read request");
                if count == 0 {
                    break;
                }

                bytes.extend_from_slice(&buffer[..count]);
                if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }

            socket
                .write_all(response.as_bytes())
                .await
                .expect("write response");
            let _ = socket.shutdown().await;
            let _ = sender.send(String::from_utf8_lossy(&bytes).to_ascii_lowercase());
        });

        (format!("http://{address}"), async move {
            receiver.await.expect("request captured")
        })
    }
}
