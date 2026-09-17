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

#[derive(Clone, Copy, Debug, Deserialize, serde::Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ApiFormat {
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

        let host = endpoint.host_str().unwrap_or_default();
        if (host.starts_with("sk-") && !host.contains('.') && host.len() > 20)
            || config
                .api_key
                .as_deref()
                .is_some_and(|key| !key.is_empty() && host == key)
        {
            return Err(ModelError::InvalidBaseUrl(
                "base_url appears to contain an API key instead of a service address; put the service URL in base_url and the credential in api_key".into(),
            ));
        }
        if !endpoint.username().is_empty() || endpoint.password().is_some() {
            return Err(ModelError::InvalidBaseUrl(
                "base_url must not contain login credentials; use api_key for authentication"
                    .into(),
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
        let response = self.send(request, false, cancellation.clone()).await?;
        let decode = async {
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
        };
        select! {
            _ = cancellation.cancelled() => Err(ModelError::Cancelled),
            result = time::timeout(self.config.timeout, decode) => result.unwrap_or_else(|_| Err(ModelError::Timeout {
                duration: self.config.timeout,
                stage: "HTTP response headers received; waiting for the complete JSON body".into(),
            })),
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
        }?;

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
                        return Err(ModelError::Timeout { duration: self.config.timeout, stage: "waiting for HTTP response headers (no response body received)".into() });
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
        let body = select! {
            _ = cancellation.cancelled() => return Err(ModelError::Cancelled),
            result = time::timeout(self.config.timeout, response.text()) => match result {
                Ok(Ok(body)) => body,
                Ok(Err(_)) => "<unreadable response body>".to_owned(),
                Err(_) => "<timed out reading HTTP error body>".to_owned(),
            },
        };

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
        let mut buffer = Vec::new();
        let mut received_bytes = 0;
        let mut decoded_events = 0;

        loop {
            let next = select! {
                _ = cancellation.cancelled() => Err(ModelError::Cancelled),
                chunk = time::timeout(timeout, body.next()) => {
                    match chunk {
                        Ok(Some(Ok(chunk))) => Ok(Some(chunk)),
                        Ok(Some(Err(error))) => Err(ModelError::Request(error)),
                        Ok(None) => Ok(None),
                        Err(Elapsed { .. }) => Err(ModelError::Timeout {
                            duration: timeout,
                            stage: format!("no SSE data received during the idle timeout ({received_bytes} body bytes received previously, {decoded_events} model events)"),
                        }),
                    }
                }
            };

            let next = match next {
                Ok(next) => next,
                Err(error) => {
                    yield Err(error);
                    return;
                }
            };

            let Some(chunk) = next else {
                break;
            };

            received_bytes += chunk.len();
            buffer.extend_from_slice(&chunk);

            while let Some(end) = find_sse_separator(&buffer) {
                let separator_len = if buffer[end..].starts_with(b"\r\n\r\n") {
                    4
                } else {
                    2
                };
                let bytes = buffer.drain(..end + separator_len).collect::<Vec<_>>();
                let frame = match String::from_utf8(bytes) {
                    Ok(frame) => frame,
                    Err(_) => {
                        yield Err(ModelError::Protocol("SSE frame contains invalid UTF-8".to_owned()));
                        return;
                    }
                };

                if let Some(data) = sse_data(&frame) {
                    if data == "[DONE]" {
                        return;
                    }

                    match decode_sse_data(&data, &decode) {
                        Ok((events, terminal)) => {
                            decoded_events += events.len();
                            for event in events {
                                yield Ok(event);
                            }
                            if terminal { return; }
                        }
                        Err(error) => {
                            yield Err(error);
                            return;
                        }
                    }
                }
            }
        }

        let buffer = match String::from_utf8(buffer) {
            Ok(buffer) => buffer,
            Err(_) => {
                yield Err(ModelError::Protocol("SSE stream ended inside a UTF-8 character".to_owned()));
                return;
            }
        };
        if !buffer.trim().is_empty() {
            match sse_data(&buffer) {
                Some(data) if data == "[DONE]" => {}
                Some(data) => match decode_sse_data(&data, &decode) {
                    Ok((events, _)) => {
                        for event in events {
                            yield Ok(event);
                        }
                    }
                    Err(error) => yield Err(error),
                },
                None => yield Err(ModelError::IncompleteSseEvent),
            }
        }
    })
}

fn decode_sse_data<F>(data: &str, decode: &F) -> Result<(Vec<StreamEvent>, bool), ModelError>
where
    F: Fn(&str) -> Result<Vec<StreamEvent>, ModelError>,
{
    let value: serde_json::Value = serde_json::from_str(data)?;
    if let Some(error) = upstream_error(&value) {
        return Err(error);
    }
    let terminal = matches!(
        value["type"].as_str(),
        Some("response.completed" | "response.incomplete" | "message_stop")
    );
    Ok((decode(data)?, terminal))
}

pub(crate) fn upstream_error(value: &serde_json::Value) -> Option<ModelError> {
    let kind = value["type"].as_str();
    let error = value
        .get("error")
        .filter(|error| !error.is_null())
        .or_else(|| {
            value
                .get("response")
                .and_then(|response| response.get("error"))
                .filter(|error| !error.is_null())
        });
    if error.is_none()
        && !matches!(kind, Some("error" | "response.failed"))
        && value["status"] != "failed"
    {
        return None;
    }
    let message = error
        .and_then(|error| {
            error
                .get("message")
                .and_then(|message| message.as_str())
                .or_else(|| error.as_str())
        })
        .or_else(|| value["message"].as_str())
        .unwrap_or("provider reported a failed response");
    let code = error
        .and_then(|error| error.get("code").or_else(|| error.get("type")))
        .and_then(|code| code.as_str());
    let response_id = value
        .get("response")
        .and_then(|response| response["id"].as_str())
        .or_else(|| value["id"].as_str())
        .map(|id| format!(" [response {id}]"))
        .unwrap_or_default();
    Some(ModelError::Upstream {
        message: code
            .map(|code| format!("{code}: {message}"))
            .unwrap_or_else(|| message.to_owned()),
        response_id,
    })
}

pub(crate) fn find_sse_separator(buffer: &[u8]) -> Option<usize> {
    let lf = buffer.windows(2).position(|bytes| bytes == b"\n\n");
    let crlf = buffer.windows(4).position(|bytes| bytes == b"\r\n\r\n");

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
    #[test]
    fn rejects_credentials_used_as_service_addresses_without_echoing_them() {
        for address in [
            "http://sk-not-a-real-secret-123456789",
            "https://name:private-password@example.com/v1",
        ] {
            let config = crate::types::ModelConfig {
                base_url: address.into(),
                ..Default::default()
            };
            let error = super::ModelClient::new(config).unwrap_err().to_string();
            assert!(error.contains("base URL is invalid"));
            assert!(!error.contains("sk-not-a-real-secret"));
            assert!(!error.contains("private-password"));
        }
    }

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
            api_key: Some("secret-value".to_owned()),
            ..ModelConfig::default()
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
    async fn anthropic_thinking_and_top_level_usage_survive_token_exhaustion() {
        let (base_url, request) = test_server(concat!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n",
            "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"thinking\",\"thinking\":\"first \"}}\n\n",
            "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"reasoning\"}}\n\n",
            "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"max_tokens\"},\"usage\":{\"output_tokens\":16384}}\n\n",
            "data: {\"type\":\"message_stop\"}\n\n",
        )).await;
        let client = client(&base_url, "anthropic_messages", "fixture", None);
        let mut req = ChatRequest::new(vec![ChatMessage::user("think")]);
        req.max_tokens = Some(16384);
        let mut stream = client.stream(req).await.unwrap();
        let mut thinking = String::new();
        let mut finished = false;
        while let Some(event) = stream.next().await {
            match event.unwrap() {
                StreamEvent::ReasoningDelta(text) => thinking.push_str(&text),
                StreamEvent::Completed {
                    finish_reason,
                    usage,
                } => {
                    assert_eq!(finish_reason.as_deref(), Some("max_tokens"));
                    assert_eq!(usage.unwrap().completion_tokens, Some(16384));
                    finished = true;
                }
                _ => panic!("unexpected event"),
            }
        }
        assert_eq!(thinking, "first reasoning");
        assert!(finished);
        assert!(request.await.contains("\"max_tokens\":16384"));
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
    async fn stream_accepts_done_frame_without_trailing_separator() {
        let (base_url, _) = test_server(concat!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"},\"finish_reason\":null}]}\n\n",
            "data: [DONE]",
        ))
        .await;
        let client = client(&base_url, "openai_chat_completions", "mock-chat", None);
        let mut stream = client
            .stream(ChatRequest::new(vec![ChatMessage::user("hello")]))
            .await
            .expect("stream starts");
        let mut text = String::new();

        while let Some(event) = stream.next().await {
            if let StreamEvent::TextDelta(delta) = event.expect("stream event succeeds") {
                text.push_str(&delta);
            }
        }

        assert_eq!(text, "hello");
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

    #[test]
    fn reports_error_envelopes_in_all_stream_formats() {
        for value in [
            serde_json::json!({"error": {"message": "overloaded", "code": "upstream_error"}}),
            serde_json::json!({"type": "error", "error": {"type": "overloaded_error", "message": "overloaded"}}),
            serde_json::json!({"type": "response.failed", "response": {"id": "resp-test", "error": {"code": "upstream_error", "message": "overloaded"}}}),
        ] {
            let error = decode_sse_data(&value.to_string(), &|_| {
                panic!("failed responses must not reach the normal decoder")
            })
            .unwrap_err();
            assert!(
                matches!(error, ModelError::Upstream { message, .. } if message.contains("overloaded"))
            );
        }
    }

    #[tokio::test]
    async fn responses_completion_does_not_wait_for_connection_close() {
        let response = concat!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"done\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\"}}\n\n",
        );
        let (url, _) =
            test_server_held(vec![response.as_bytes().to_vec()], Duration::from_secs(2)).await;
        let model = client(&url, "openai_responses", "mock", None);
        let mut events = model
            .stream(ChatRequest::new(vec![ChatMessage::user("hello")]))
            .await
            .unwrap();
        time::timeout(Duration::from_millis(300), async {
            let mut completed = false;
            while let Some(event) = events.next().await {
                completed |= matches!(event.unwrap(), StreamEvent::Completed { .. });
            }
            assert!(completed);
        })
        .await
        .expect("terminal SSE event ends the stream immediately");
    }

    #[tokio::test]
    async fn json_body_timeout_is_bounded_and_identifies_the_stage() {
        let (url, _) = test_server_held(
            vec![
                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 100\r\n\r\n"
                    .to_vec(),
            ],
            Duration::from_secs(2),
        )
        .await;
        let model = ModelClient::new(ModelConfig {
            base_url: url,
            timeout: Duration::from_millis(100),
            ..ModelConfig::default()
        })
        .unwrap();
        let error = time::timeout(
            Duration::from_secs(1),
            model.complete(ChatRequest::new(vec![ChatMessage::user("hello")])),
        )
        .await
        .unwrap()
        .unwrap_err();
        assert!(matches!(error, ModelError::Timeout { stage, .. } if stage.contains("JSON body")));
    }

    #[tokio::test]
    async fn active_stream_can_run_longer_than_the_idle_timeout() {
        let mut chunks =
            vec![b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n".to_vec()];
        for _ in 0..30 {
            chunks.push(b"data: {\"choices\":[{\"delta\":{\"reasoning_content\":\"thinking\"},\"finish_reason\":null}]}\n\n".to_vec());
        }
        chunks.push(b"data: [DONE]\n\n".to_vec());
        let (url, _) = test_server_chunks(chunks).await;
        let timeout = Duration::from_millis(200);
        let model = ModelClient::new(ModelConfig {
            base_url: url,
            timeout,
            ..ModelConfig::default()
        })
        .unwrap();
        let mut events = model
            .stream(ChatRequest::new(vec![ChatMessage::user("hello")]))
            .await
            .unwrap();
        let started = time::Instant::now();
        let mut count = 0;
        while let Some(event) = events.next().await {
            assert!(matches!(event.unwrap(), StreamEvent::ReasoningDelta(_)));
            count += 1;
        }
        assert_eq!(count, 30);
        assert!(started.elapsed() > timeout);
    }

    #[tokio::test]
    async fn heartbeat_only_stream_times_out_with_received_byte_count() {
        let (url, _) = test_server_held(
            vec![b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n: PING\n\n".to_vec()],
            Duration::from_secs(2),
        )
        .await;
        let model = ModelClient::new(ModelConfig {
            base_url: url,
            timeout: Duration::from_millis(100),
            ..ModelConfig::default()
        })
        .unwrap();
        let mut events = model
            .stream(ChatRequest::new(vec![ChatMessage::user("hello")]))
            .await
            .unwrap();
        let error = time::timeout(Duration::from_secs(1), events.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(
            matches!(error, ModelError::Timeout { stage, .. } if stage.contains("8 body bytes received previously, 0 model events"))
        );
    }

    #[tokio::test]
    async fn preserves_unicode_split_across_network_chunks() {
        let response = concat!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"中文\"},\"finish_reason\":null}]}\n\n",
            "data: [DONE]\n\n",
        );
        let split = response.find('中').unwrap() + 1;
        let (base_url, _) = test_server_chunks(vec![
            response.as_bytes()[..split].to_vec(),
            response.as_bytes()[split..].to_vec(),
        ])
        .await;
        let mut stream = client(&base_url, "openai_chat_completions", "mock", None)
            .stream(ChatRequest::new(vec![ChatMessage::user("hello")]))
            .await
            .unwrap();
        let mut text = String::new();
        while let Some(event) = stream.next().await {
            if let StreamEvent::TextDelta(delta) = event.unwrap() {
                text.push_str(&delta);
            }
        }
        assert_eq!(text, "中文");
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
        test_server_chunks(vec![response.into().into_bytes()]).await
    }

    async fn test_server_chunks(
        chunks: Vec<Vec<u8>>,
    ) -> (String, impl std::future::Future<Output = String>) {
        test_server_held(chunks, Duration::ZERO).await
    }

    async fn test_server_held(
        chunks: Vec<Vec<u8>>,
        hold: Duration,
    ) -> (String, impl std::future::Future<Output = String>) {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind test server");
        let address = listener.local_addr().expect("local address");
        let chunks = Arc::new(chunks);
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

            for chunk in chunks.iter() {
                socket.write_all(chunk).await.expect("write response");
                time::sleep(Duration::from_millis(10)).await;
            }
            time::sleep(hold).await;
            let _ = socket.shutdown().await;
            let _ = sender.send(String::from_utf8_lossy(&bytes).to_ascii_lowercase());
        });

        (format!("http://{address}"), async move {
            receiver.await.expect("request captured")
        })
    }
}
