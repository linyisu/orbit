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
    GoogleCodeAssist,
    ChatgptCodex,
}

impl ApiFormat {
    pub fn parse(value: &str) -> Result<Self, ModelError> {
        match value {
            "openai_chat_completions" => Ok(Self::OpenaiChatCompletions),
            "openai_responses" => Ok(Self::OpenaiResponses),
            "anthropic_messages" => Ok(Self::AnthropicMessages),
            "google_code_assist" => Ok(Self::GoogleCodeAssist),
            "chatgpt_codex" => Ok(Self::ChatgptCodex),
            other => Err(ModelError::UnsupportedApiFormat(other.to_owned())),
        }
    }
}

#[derive(Clone)]
pub struct ModelClient {
    http: Client,
    config: ModelConfig,
    endpoint: Url,
    auth_store: Option<orbit_auth::AuthStore>,
    #[cfg(test)]
    request_endpoint: Option<Url>,
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
        if config.max_retries > 5 {
            return Err(ModelError::Protocol(
                "max_retries must be between 0 and 5".to_owned(),
            ));
        }

        if let Some(adapter) = config.reasoning_adapter {
            let compatible = matches!(
                (config.api_format, adapter),
                (
                    ApiFormat::OpenaiChatCompletions,
                    ReasoningAdapter::OpenaiChat
                ) | (
                    ApiFormat::OpenaiResponses | ApiFormat::ChatgptCodex,
                    ReasoningAdapter::OpenaiResponses
                ) | (
                    ApiFormat::AnthropicMessages,
                    ReasoningAdapter::AnthropicMessages
                ) | (ApiFormat::GoogleCodeAssist, ReasoningAdapter::Google)
            );

            if !compatible {
                return Err(ModelError::Protocol(
                    "reasoning adapter does not match the configured API format".into(),
                ));
            }
        }

        let format = config.api_format;
        let suffix = match format {
            ApiFormat::OpenaiChatCompletions => "chat/completions",
            ApiFormat::OpenaiResponses | ApiFormat::ChatgptCodex => "responses",
            ApiFormat::GoogleCodeAssist => "v1internal:streamGenerateContent?alt=sse",
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

        if config.credential.is_some() && config.api_key.is_some() {
            return Err(ModelError::Protocol(
                "choose either api_key or credential authentication".into(),
            ));
        }

        if matches!(
            format,
            ApiFormat::GoogleCodeAssist | ApiFormat::ChatgptCodex
        ) && config.credential.is_none()
        {
            return Err(ModelError::Protocol(
                "account-backed providers require a saved credential".into(),
            ));
        }

        let auth_store = config
            .credential
            .as_ref()
            .map(|reference| orbit_auth::AuthStore::new(reference.directory.clone()))
            .transpose()?;

        let http = Client::builder()
            .connect_timeout(config.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()?;

        Ok(Self {
            http,
            config,
            endpoint,
            auth_store,
            #[cfg(test)]
            request_endpoint: None,
        })
    }

    pub fn config(&self) -> &ModelConfig {
        &self.config
    }

    pub fn supports_vision(&self) -> bool {
        self.config.supports_vision
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
        if matches!(
            self.config.api_format,
            ApiFormat::GoogleCodeAssist | ApiFormat::ChatgptCodex
        ) {
            let stream = self.stream_with_cancel(request, cancellation).await?;
            return crate::collect::completion(stream, &self.config.model).await;
        }

        let (response, _) = self.send(request, false, cancellation.clone()).await?;
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
                ApiFormat::GoogleCodeAssist | ApiFormat::ChatgptCodex => {
                    unreachable!("account providers collect SSE")
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
        self.stream_with_retries(request, cancellation)
            .await
            .map(|(stream, _)| stream)
    }

    pub async fn stream_with_retries(
        &self,
        request: ChatRequest,
        cancellation: CancellationToken,
    ) -> Result<(ModelStream, u32), ModelError> {
        let (response, retries) = self.send(request, true, cancellation.clone()).await?;
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_ascii_lowercase();

        // ChatGPT can omit Content-Type on a successful SSE response. Let the
        // event decoder validate that body, including its terminal event.
        let codex_without_type =
            self.config.api_format == ApiFormat::ChatgptCodex && content_type.is_empty();
        if !content_type.contains("text/event-stream") && !codex_without_type {
            return Err(ModelError::Protocol(format!(
                "streaming endpoint returned HTTP {} with Content-Type {:?}; expected text/event-stream",
                response.status(),
                content_type
            )));
        }

        let stream = match self.config.api_format {
            ApiFormat::GoogleCodeAssist => crate::google::stream(
                response,
                self.config.timeout,
                cancellation,
                self.config.model.clone(),
            ),

            ApiFormat::OpenaiChatCompletions => {
                crate::openai::chat_completions::stream(response, self.config.timeout, cancellation)
            }
            ApiFormat::OpenaiResponses | ApiFormat::ChatgptCodex => {
                crate::openai::responses::stream(response, self.config.timeout, cancellation)
            }
            ApiFormat::AnthropicMessages => {
                crate::anthropic::messages::stream(response, self.config.timeout, cancellation)
            }
        };

        Ok((stream, retries))
    }

    async fn send(
        &self,
        mut request: ChatRequest,
        stream: bool,
        cancellation: CancellationToken,
    ) -> Result<(Response, u32), ModelError> {
        if request.reasoning_effort.is_none() {
            request.reasoning_effort = self.config.reasoning_effort;
        }

        let format = self.config.api_format;
        let (body, headers) = match format {
            ApiFormat::GoogleCodeAssist => crate::google::encode(&self.config.model, request),
            ApiFormat::ChatgptCodex => crate::openai::codex::encode(
                &self.config.model,
                request,
                self.config.reasoning_adapter,
                &self.config.reasoning_extra,
            ),

            ApiFormat::OpenaiChatCompletions => crate::openai::chat_completions::encode(
                &self.config.model,
                request,
                self.config.reasoning_adapter,
                &self.config.reasoning_extra,
                stream,
            ),

            ApiFormat::OpenaiResponses => crate::openai::responses::encode(
                &self.config.model,
                request,
                self.config.reasoning_adapter,
                &self.config.reasoning_extra,
                stream,
            ),

            ApiFormat::AnthropicMessages => crate::anthropic::messages::encode(
                &self.config.model,
                request,
                self.config.reasoning_adapter,
                &self.config.reasoning_extra,
                stream,
            ),
        }?;

        for attempt in 0..=self.config.max_retries {
            let result = self
                .send_once(&body, &headers, format, cancellation.clone())
                .await;

            match result {
                Ok(response) => return Ok((response, u32::from(attempt))),
                Err(error) if error.is_retryable() && attempt < self.config.max_retries => {
                    let delay = Duration::from_millis(250_u64 << attempt);
                    select! {
                        _ = cancellation.cancelled() => return Err(ModelError::Cancelled),
                        _ = time::sleep(delay) => {}
                    }
                }

                Err(error) => return Err(error),
            }
        }

        unreachable!("retry loop returns on every final attempt")
    }

    /// Executes a single request. Callers may retry only errors produced here:
    /// a successful response is handed to the stream decoder and is never
    /// replayed after bytes from the provider become observable.
    async fn send_once(
        &self,
        body: &serde_json::Value,
        headers: &reqwest::header::HeaderMap,
        format: ApiFormat,
        cancellation: CancellationToken,
    ) -> Result<Response, ModelError> {
        let endpoint = &self.endpoint;
        #[cfg(test)]
        let endpoint = self.request_endpoint.as_ref().unwrap_or(endpoint);
        let mut builder = self
            .http
            .post(endpoint.clone())
            .headers(headers.clone())
            .header(header::CONTENT_TYPE, "application/json");
        let mut body = body.clone();

        if let (Some(store), Some(reference)) = (&self.auth_store, &self.config.credential) {
            let auth = select! {
                _ = cancellation.cancelled() => return Err(ModelError::Cancelled),
                result = time::timeout(self.config.timeout, store.resolve(
                    &reference.name, &self.endpoint, match format {
                        ApiFormat::AnthropicMessages => orbit_auth::AuthTarget::Anthropic,
                        ApiFormat::GoogleCodeAssist => orbit_auth::AuthTarget::GoogleCodeAssist,
                        ApiFormat::ChatgptCodex => orbit_auth::AuthTarget::ChatgptCodex,
                        _ => orbit_auth::AuthTarget::Openai,
                    },
                )) => result.map_err(|_| orbit_auth::AuthError::Timeout)??,
            };

            if format == ApiFormat::GoogleCodeAssist {
                body["project"] =
                    serde_json::json!(auth.project_id.ok_or(orbit_auth::AuthError::InvalidToken)?);
            }

            if auth.kind == orbit_auth::CredentialKind::AntigravityOAuth {
                crate::google::antigravity_envelope(&mut body)?;
                if body["model"]
                    .as_str()
                    .is_some_and(|model| model.starts_with("claude-"))
                {
                    builder = builder.header("anthropic-beta", "interleaved-thinking-2025-05-14");
                }
            }

            builder = builder.headers(auth.headers);
        } else if let Some(api_key) = &self.config.api_key {
            builder = match format {
                ApiFormat::AnthropicMessages => builder.header("x-api-key", api_key),
                _ => builder.bearer_auth(api_key),
            };
        }

        let builder = builder.json(&body);
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
            context_limit: ModelError::has_context_limit_code(&body),
        })
    }
}

pub(crate) fn sse_stream<F>(
    response: Response,
    timeout: Duration,
    cancellation: CancellationToken,
    mut decode: F,
) -> ModelStream
where
    F: FnMut(&str) -> Result<Vec<StreamEvent>, ModelError> + Send + Sync + 'static,
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

                    match decode_sse_data(&data, &mut decode) {
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
                Some(data) => match decode_sse_data(&data, &mut decode) {
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

fn decode_sse_data<F>(data: &str, decode: &mut F) -> Result<(Vec<StreamEvent>, bool), ModelError>
where
    F: FnMut(&str) -> Result<Vec<StreamEvent>, ModelError>,
{
    let value: serde_json::Value = serde_json::from_str(data)?;

    if let Some(error) = upstream_error(&value) {
        return Err(error);
    }

    let terminal = matches!(
        value["type"].as_str(),
        Some("response.completed" | "response.incomplete" | "message_stop")
    ) || value["response"]["candidates"][0]["finishReason"].is_string()
        || value["candidates"][0]["finishReason"].is_string();

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
mod tests;
