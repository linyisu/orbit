//! Model capability records shared by discovery and configuration projection.
use crate::{
    client::ApiFormat,
    types::{ReasoningAdapter, ReasoningEffort},
};
use anyhow::{Result, bail};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
pub struct ModelProfile {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Overrides the top-level context window for this model only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<usize>,
    /// Full usable input window for providers that allow output outside this
    /// input limit. When omitted, `context_tokens` keeps its total-budget
    /// meaning and Orbit reserves the configured output upper bound from it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_context_tokens: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningProfile>,
    #[serde(default)]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_vision: Option<bool>,
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
pub struct ReasoningProfile {
    pub adapter: ReasoningAdapter,
    pub efforts: Vec<ReasoningEffort>,
    pub default: ReasoningEffort,
    #[serde(default)]
    pub extra: toml::Table,
}

/// `extra` is for provider extensions. These fields are owned by Orbit's request encoder; accepting
/// them here would silently replace the model, transport shape, output budget, or tool contract.
pub fn validate_reasoning_extra(
    model: &str,
    api_format: ApiFormat,
    extra: &toml::Table,
) -> Result<()> {
    for key in [
        "model",
        "messages",
        "input",
        "stream",
        "tools",
        "tool_choice",
        "max_tokens",
        "max_output_tokens",
        "temperature",
    ] {
        if extra.contains_key(key) {
            bail!(
                "model {model:?}: reasoning.extra may not override the standard request field \
                    {key:?}"
            );
        }
    }

    let conflicts_effort = match api_format {
        ApiFormat::OpenaiChatCompletions => extra.contains_key("reasoning_effort"),
        ApiFormat::OpenaiResponses | ApiFormat::ChatgptCodex => extra
            .get("reasoning")
            .and_then(toml::Value::as_table)
            .is_some_and(|reasoning| reasoning.contains_key("effort")),

        ApiFormat::GoogleCodeAssist => !extra.is_empty(),
        ApiFormat::AnthropicMessages => extra
            .get("output_config")
            .and_then(toml::Value::as_table)
            .is_some_and(|output| output.contains_key("effort")),
    };

    if conflicts_effort {
        bail!("model {model:?}: reasoning.extra may not override the configured reasoning effort");
    }

    Ok(())
}

impl ModelProfile {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            max_output_tokens: None,
            context_tokens: None,
            input_context_tokens: None,
            reasoning: None,
            supports_vision: None,
        }
    }
}
