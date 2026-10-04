use std::{path::PathBuf, pin::Pin, time::Duration};

use base64::{Engine, engine::general_purpose::STANDARD};
use futures_core::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{client::ApiFormat, error::ModelError};

pub type ModelStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, ModelError>> + Send>>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum ChatMessage {
    System {
        content: String,
    },

    User {
        content: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<Attachment>,
    },

    Assistant {
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
    },

    Tool {
        tool_call_id: String,
        content: String,
    },
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self::System {
            content: content.into(),
        }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self::User {
            content: content.into(),
            attachments: Vec::new(),
        }
    }

    pub fn user_with_attachments(content: impl Into<String>, attachments: Vec<Attachment>) -> Self {
        Self::User {
            content: content.into(),
            attachments,
        }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self::Assistant {
            content: Some(content.into()),
            tool_calls: Vec::new(),
        }
    }

    pub fn assistant_tool_calls(tool_calls: Vec<ToolCall>) -> Self {
        Self::Assistant {
            content: None,
            tool_calls,
        }
    }

    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self::Tool {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Attachment {
    Image {
        name: String,
        mime_type: String,
        path: PathBuf,
    },
}

impl Attachment {
    /// Shared byte limit for imported, transmitted and displayed images.
    pub const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;

    pub fn name(&self) -> &str {
        match self {
            Self::Image { name, .. } => name,
        }
    }

    pub fn image(&self) -> Result<(&str, String), crate::error::ModelError> {
        match self {
            Self::Image {
                mime_type, path, ..
            } => {
                let bytes = Self::read_image_bytes(path)?;
                Ok((mime_type, STANDARD.encode(bytes)))
            }
        }
    }

    /// Read a regular image file within the shared byte budget. Format decoding
    /// belongs to the caller; this also supports SVG bytes in terminal previews.
    pub fn read_image_bytes(path: &std::path::Path) -> Result<Vec<u8>, crate::error::ModelError> {
        use std::io::Read;

        let io_error =
            |error| crate::error::ModelError::Attachment(format!("{}: {error}", path.display()));
        let too_large = || {
            crate::error::ModelError::Attachment(format!(
                "{} is not a regular image up to 20 MiB",
                path.display()
            ))
        };
        let metadata = std::fs::metadata(path).map_err(io_error)?;
        if !metadata.is_file() || metadata.len() > Self::MAX_IMAGE_BYTES as u64 {
            return Err(too_large());
        }

        let file = std::fs::File::open(path).map_err(io_error)?;
        if !file.metadata().map_err(io_error)?.is_file() {
            return Err(too_large());
        }
        let mut bytes = Vec::new();
        file.take(Self::MAX_IMAGE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(io_error)?;
        if bytes.len() > Self::MAX_IMAGE_BYTES {
            return Err(too_large());
        }

        Ok(bytes)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCall {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<ToolCallMetadata>,
    pub id: String,
    #[serde(rename = "type", default = "default_tool_call_kind")]
    pub kind: String,
    pub function: FunctionCall,
}

/// Provider evidence retained losslessly, projected only by its own adapter.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCallMetadata {
    pub google: GoogleToolMetadata,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct GoogleToolMetadata {
    pub model: String,
    pub thought_signature: Option<String>,
    pub native_id: Option<String>,
}

fn default_tool_call_kind() -> String {
    "function".to_owned()
}

impl ToolCall {
    pub fn function(id: impl Into<String>, name: impl Into<String>, arguments: Value) -> Self {
        Self {
            metadata: None,
            id: id.into(),
            kind: "function".to_owned(),
            function: FunctionCall {
                name: name.into(),
                arguments: arguments.to_string(),
            },
        }
    }

    pub fn arguments_json(&self) -> Result<Value, serde_json::Error> {
        serde_json::from_str(&self.function.arguments)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolDefinition {
    pub r#type: String,
    pub function: FunctionDefinition,
}

impl ToolDefinition {
    pub fn function(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: Value,
    ) -> Self {
        Self {
            r#type: "function".to_owned(),
            function: FunctionDefinition {
                name: name.into(),
                description: description.into(),
                parameters,
            },
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ToolDefinition>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

/// Wire adapter selected by a model capability profile.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningAdapter {
    OpenaiChat,
    OpenaiResponses,
    AnthropicMessages,
    Google,
}

impl ReasoningEffort {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn from_str(value: &str) -> Option<Self> {
        value.parse().ok()
    }

    pub fn label(self) -> &'static str {
        self.as_str()
    }
}

impl std::str::FromStr for ReasoningEffort {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "none" | "off" | "disabled" => Ok(Self::None),
            "minimal" => Ok(Self::Minimal),
            "low" => Ok(Self::Low),
            "medium" => Ok(Self::Medium),
            "high" => Ok(Self::High),
            "xhigh" => Ok(Self::Xhigh),
            "max" => Ok(Self::Max),
            _ => Err(()),
        }
    }
}

impl ChatRequest {
    pub fn new(messages: Vec<ChatMessage>) -> Self {
        Self {
            messages,
            ..Self::default()
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct ChatCompletion {
    pub id: Option<String>,
    pub model: Option<String>,
    pub message: ChatMessage,
    pub finish_reason: Option<String>,
    pub usage: Option<Usage>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Usage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum StreamEvent {
    TextDelta(String),
    ReasoningDelta(String),
    ToolCallDelta(ToolCallDelta),
    Completed {
        finish_reason: Option<String>,
        usage: Option<Usage>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCallDelta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<ToolCallMetadata>,
    pub index: usize,
    pub id: Option<String>,
    pub name: Option<String>,
    pub arguments: String,
}

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelConfig {
    pub api_format: ApiFormat,
    pub base_url: String,
    pub api_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<orbit_auth::CredentialRef>,
    pub model: String,
    #[serde(with = "duration_seconds")]
    pub timeout: Duration,
    /// Number of transport attempts after the initial request. Retries stop as
    /// soon as HTTP response headers have been accepted.
    #[serde(default = "default_max_retries")]
    pub max_retries: u8,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub reasoning_adapter: Option<ReasoningAdapter>,
    #[serde(default)]
    pub reasoning_extra: Value,
    #[serde(default = "default_supports_vision")]
    pub supports_vision: bool,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            api_format: ApiFormat::OpenaiChatCompletions,
            base_url: "https://api.openai.com/v1".to_owned(),
            api_key: None,
            credential: None,
            model: "gpt-5.6-sol".to_owned(),
            timeout: Duration::from_secs(120),
            max_retries: default_max_retries(),
            reasoning_effort: None,
            reasoning_adapter: None,
            reasoning_extra: Value::Object(Default::default()),
            supports_vision: default_supports_vision(),
        }
    }
}

impl std::fmt::Debug for ModelConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModelConfig")
            .field("api_format", &self.api_format)
            .field("base_url", &self.base_url)
            .field("api_key", &self.api_key.as_ref().map(|_| "[redacted]"))
            .field("credential", &self.credential)
            .field("model", &self.model)
            .field("timeout", &self.timeout)
            .field("max_retries", &self.max_retries)
            .field("reasoning_effort", &self.reasoning_effort)
            .field("reasoning_adapter", &self.reasoning_adapter)
            .field("supports_vision", &self.supports_vision)
            .finish()
    }
}

pub const fn default_max_retries() -> u8 {
    2
}

pub const fn default_supports_vision() -> bool {
    false
}

mod duration_seconds {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::time::Duration;

    pub fn serialize<S>(value: &Duration, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u64(value.as_secs())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Duration, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Duration::from_secs(u64::deserialize(deserializer)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_files_are_bounded_before_encoding_or_decoding() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("image.png");
        std::fs::write(&path, b"image bytes").unwrap();
        assert_eq!(Attachment::read_image_bytes(&path).unwrap(), b"image bytes");

        // A sparse file checks rejection without allocating a large fixture.
        std::fs::File::create(&path)
            .unwrap()
            .set_len(Attachment::MAX_IMAGE_BYTES as u64 + 1)
            .unwrap();
        assert!(Attachment::read_image_bytes(&path).is_err());
        assert!(Attachment::read_image_bytes(directory.path()).is_err());
    }

    #[test]
    fn max_effort_round_trips_without_becoming_xhigh() {
        let effort: ReasoningEffort = "max".parse().unwrap();
        let serialized = serde_json::to_string(&effort).unwrap();
        let restored: ReasoningEffort = serde_json::from_str(&serialized).unwrap();

        assert_eq!(effort, ReasoningEffort::Max);
        assert_eq!(effort.as_str(), "max");
        assert_eq!(effort.label(), "max");
        assert_eq!(serialized, "\"max\"");
        assert_eq!(restored, effort);
        assert_ne!(restored, ReasoningEffort::Xhigh);
    }

    #[test]
    fn test_reasoning_effort_parsing_and_labels() {
        assert_eq!(
            "none".parse::<ReasoningEffort>().unwrap(),
            ReasoningEffort::None
        );
        assert_eq!(
            "off".parse::<ReasoningEffort>().unwrap(),
            ReasoningEffort::None
        );
        assert_eq!(
            "disabled".parse::<ReasoningEffort>().unwrap(),
            ReasoningEffort::None
        );
        assert_eq!(
            "high".parse::<ReasoningEffort>().unwrap(),
            ReasoningEffort::High
        );

        assert_eq!(ReasoningEffort::None.as_str(), "none");
        assert_eq!(ReasoningEffort::None.label(), "none");

        let serialized = serde_json::to_string(&ReasoningEffort::None).unwrap();

        assert_eq!(serialized, "\"none\"");

        let deserialized: ReasoningEffort = serde_json::from_str("\"none\"").unwrap();

        assert_eq!(deserialized, ReasoningEffort::None);
    }
}
