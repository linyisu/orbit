use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ModelError {
    #[error("model name cannot be empty")]
    InvalidModel,
    #[error("unsupported API format: {0}")]
    UnsupportedApiFormat(String),
    #[error("base URL is invalid: {0}")]
    InvalidBaseUrl(String),
    #[error("request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("model request timed out after {0:?}")]
    Timeout(Duration),
    #[error("model request was cancelled")]
    Cancelled,
    #[error("model returned HTTP {status}{request_id}: {body}")]
    Http {
        status: u16,
        request_id: String,
        body: String,
        retryable: bool,
    },
    #[error("could not decode model response: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("model protocol error: {0}")]
    Protocol(String),
    #[error("model stream ended without a complete server-sent event")]
    IncompleteSseEvent,
}

impl ModelError {
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Http { retryable, .. } => *retryable,
            Self::Request(_) | Self::Timeout(_) => true,
            Self::InvalidModel
            | Self::UnsupportedApiFormat(_)
            | Self::InvalidBaseUrl(_)
            | Self::Cancelled
            | Self::Decode(_)
            | Self::Protocol(_)
            | Self::IncompleteSseEvent => false,
        }
    }
}
