use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

use async_stream::stream;
use orbit_model::{
    client::ModelClient,
    error::ModelError,
    types::{ChatCompletion, ChatMessage, ChatRequest, ModelStream, StreamEvent},
};
use tokio_util::sync::CancellationToken;

pub type CompletionFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ChatCompletion, ModelError>> + Send + 'a>>;

pub type StreamFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ModelStream, ModelError>> + Send + 'a>>;

pub type ObservedStreamFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(ModelStream, Option<u32>), ModelError>> + Send + 'a>>;

pub trait ModelBackend: Send + Sync {
    fn supports_vision(&self) -> bool {
        true
    }

    fn stream_with_retries<'a>(
        &'a self,
        request: ChatRequest,
        cancellation: CancellationToken,
    ) -> ObservedStreamFuture<'a> {
        Box::pin(async move {
            self.stream(request, cancellation)
                .await
                .map(|stream| (stream, None))
        })
    }
    fn complete<'a>(
        &'a self,
        request: ChatRequest,
        cancellation: CancellationToken,
    ) -> CompletionFuture<'a>;

    fn stream<'a>(
        &'a self,
        request: ChatRequest,
        cancellation: CancellationToken,
    ) -> StreamFuture<'a>;
}

#[derive(Clone)]
pub struct ModelClientBackend {
    client: ModelClient,
}

impl ModelClientBackend {
    pub fn new(client: ModelClient) -> Self {
        Self { client }
    }
}

impl ModelBackend for ModelClientBackend {
    fn supports_vision(&self) -> bool {
        self.client.supports_vision()
    }

    fn stream_with_retries<'a>(
        &'a self,
        request: ChatRequest,
        cancellation: CancellationToken,
    ) -> ObservedStreamFuture<'a> {
        Box::pin(async move {
            self.client
                .stream_with_retries(request, cancellation)
                .await
                .map(|(stream, count)| (stream, Some(count)))
        })
    }

    fn complete<'a>(
        &'a self,
        request: ChatRequest,
        cancellation: CancellationToken,
    ) -> CompletionFuture<'a> {
        Box::pin(self.client.complete_with_cancel(request, cancellation))
    }

    fn stream<'a>(
        &'a self,
        request: ChatRequest,
        cancellation: CancellationToken,
    ) -> StreamFuture<'a> {
        Box::pin(self.client.stream_with_cancel(request, cancellation))
    }
}

#[derive(Clone)]
pub struct MockModel {
    responses: Arc<Mutex<VecDeque<Result<ChatCompletion, ModelError>>>>,
    requests: Arc<Mutex<Vec<ChatRequest>>>,
    supports_vision: bool,
}

impl MockModel {
    pub fn new(responses: impl IntoIterator<Item = ChatCompletion>) -> Self {
        Self::from_results(responses.into_iter().map(Ok).collect::<Vec<_>>())
    }

    pub fn from_results(
        responses: impl IntoIterator<Item = Result<ChatCompletion, ModelError>>,
    ) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses.into_iter().collect())),
            requests: Arc::new(Mutex::new(Vec::new())),
            supports_vision: true,
        }
    }

    pub fn with_vision(mut self, supports_vision: bool) -> Self {
        self.supports_vision = supports_vision;
        self
    }

    pub fn requests(&self) -> Vec<ChatRequest> {
        self.requests
            .lock()
            .expect("mock request lock is not poisoned")
            .clone()
    }
}

impl ModelBackend for MockModel {
    fn supports_vision(&self) -> bool {
        self.supports_vision
    }

    fn complete<'a>(
        &'a self,
        request: ChatRequest,
        cancellation: CancellationToken,
    ) -> CompletionFuture<'a> {
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(ModelError::Cancelled);
            }

            self.requests
                .lock()
                .expect("mock request lock is not poisoned")
                .push(request);

            self.responses
                .lock()
                .expect("mock response lock is not poisoned")
                .pop_front()
                .unwrap_or_else(|| {
                    Err(ModelError::Protocol(
                        "mock model has no remaining responses".to_owned(),
                    ))
                })
        })
    }

    fn stream<'a>(
        &'a self,
        request: ChatRequest,
        cancellation: CancellationToken,
    ) -> StreamFuture<'a> {
        Box::pin(async move {
            let completion = self.complete(request, cancellation.clone()).await?;
            let message = match completion.message {
                ChatMessage::Assistant {
                    content,
                    tool_calls,
                } => (content.unwrap_or_default(), tool_calls),
                _ => (String::new(), Vec::new()),
            };
            let text = message.0;
            let calls = message.1;

            Ok(Box::pin(stream! {
                if !text.is_empty() {
                    yield Ok(StreamEvent::TextDelta(text));
                }
                for (index, call) in calls.into_iter().enumerate() {
                    yield Ok(StreamEvent::ToolCallDelta(orbit_model::types::ToolCallDelta {
                        index,
                        id: Some(call.id),
                        name: Some(call.function.name),
                        arguments: call.function.arguments,
                    }));
                }
                yield Ok(StreamEvent::Completed {
                    finish_reason: completion.finish_reason,
                    usage: completion.usage,
                });
            }) as ModelStream)
        })
    }
}
