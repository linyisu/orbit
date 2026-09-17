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
    pub fn name(&self) -> &str {
        match self {
            Self::Image { name, .. } => name,
        }
    }

    pub fn image(&self) -> Result<(&str, String), crate::error::ModelError> {
        const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;
        match self {
            Self::Image {
                mime_type, path, ..
            } => {
                let metadata = std::fs::metadata(path).map_err(|error| {
                    crate::error::ModelError::Attachment(format!("{}: {error}", path.display()))
                })?;
                if !metadata.is_file() || metadata.len() > MAX_IMAGE_BYTES {
                    return Err(crate::error::ModelError::Attachment(format!(
                        "{} is not a regular image up to 20 MiB",
                        path.display()
                    )));
                }
                let bytes = std::fs::read(path).map_err(|error| {
                    crate::error::ModelError::Attachment(format!("{}: {error}", path.display()))
                })?;
                Ok((mime_type, STANDARD.encode(bytes)))
            }
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "default_tool_call_kind")]
    pub kind: String,
    pub function: FunctionCall,
}

fn default_tool_call_kind() -> String {
    "function".to_owned()
}

impl ToolCall {
    pub fn function(id: impl Into<String>, name: impl Into<String>, arguments: Value) -> Self {
        Self {
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
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
}

/// Wire adapter selected by a model capability profile.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningAdapter {
    OpenaiChat,
    OpenaiResponses,
    AnthropicMessages,
}

impl ReasoningEffort {
    pub fn label(self) -> &'static str {
        match self {
            Self::Minimal => "极低",
            Self::Low => "低",
            Self::Medium => "中",
            Self::High => "高",
            Self::Xhigh => "极高",
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
    pub model: String,
    #[serde(with = "duration_seconds")]
    pub timeout: Duration,
    pub reasoning_effort: Option<ReasoningEffort>,
    pub reasoning_adapter: Option<ReasoningAdapter>,
    #[serde(default)]
    pub reasoning_extra: Value,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            api_format: ApiFormat::OpenaiChatCompletions,
            base_url: "https://api.openai.com/v1".to_owned(),
            api_key: None,
            model: "gpt-5.6-sol".to_owned(),
            timeout: Duration::from_secs(120),
            reasoning_effort: None,
            reasoning_adapter: None,
            reasoning_extra: Value::Object(Default::default()),
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
            .field("model", &self.model)
            .field("timeout", &self.timeout)
            .field("reasoning_effort", &self.reasoning_effort)
            .field("reasoning_adapter", &self.reasoning_adapter)
            .finish()
    }
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
