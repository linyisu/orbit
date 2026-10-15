use std::time::Duration;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ModelError {
    #[error("authentication failed: {0}")]
    Authentication(#[from] orbit_auth::AuthError),
    #[error("model identifier is empty, too long, or contains control characters")]
    InvalidModel,
    #[error("unsupported API format: {0}")]
    UnsupportedApiFormat(String),
    #[error("base URL is invalid: {0}")]
    InvalidBaseUrl(String),
    #[error("request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("model request timed out after {duration:?}: {stage}")]
    Timeout { duration: Duration, stage: String },
    #[error("upstream error: {message}{response_id}")]
    Upstream {
        message: String,
        response_id: String,
    },
    #[error("model request was cancelled")]
    Cancelled,
    #[error("model returned HTTP {status}{request_id}: {body}")]
    Http {
        status: u16,
        request_id: String,
        body: String,
        retryable: bool,
        context_limit: bool,
    },
    #[error("could not decode model response: {0}")]
    Decode(#[from] serde_json::Error),
    #[error("model protocol error: {0}")]
    Protocol(String),
    #[error("could not read image attachment: {0}")]
    Attachment(String),
    #[error("model stream ended without a complete server-sent event")]
    IncompleteSseEvent,
}

impl ModelError {
    /// Only explicit protocol error codes justify context recovery. HTTP 400
    /// also covers invalid parameters; 413 can mean byte size, not token count.
    /// Do not infer a context limit from arbitrary upstream prose.
    pub fn is_context_limit(&self) -> bool {
        matches!(
            self,
            Self::Http {
                status: 400 | 413 | 422,
                context_limit: true,
                ..
            }
        )
    }

    pub(crate) fn has_context_limit_code(body: &str) -> bool {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
            return false;
        };

        [
            value["error"]["code"].as_str(),
            value["error"]["type"].as_str(),
        ]
        .into_iter()
        .flatten()
        .any(|code| {
            matches!(
                code,
                "context_length_exceeded" | "context_window_exceeded" | "prompt_too_long"
            )
        })
    }

    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Http { retryable, .. } => *retryable,
            Self::Request(_) | Self::Timeout { .. } => true,
            Self::Authentication(_)
            | Self::InvalidModel
            | Self::UnsupportedApiFormat(_)
            | Self::InvalidBaseUrl(_)
            | Self::Cancelled
            | Self::Decode(_)
            | Self::Upstream { .. }
            | Self::Protocol(_)
            | Self::Attachment(_)
            | Self::IncompleteSseEvent => false,
        }
    }
}
