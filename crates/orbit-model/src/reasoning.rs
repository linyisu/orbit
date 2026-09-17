//! Provider capability adapters for reasoning controls.
//!
//! Keep compatibility choices here rather than inferring them from a model ID
//! inside individual request encoders. A gateway can rename a model while its
//! provider-level protocol remains stable.

use serde_json::{Value, json};

use crate::types::{ReasoningEffort, ReasoningProtocol};

pub(crate) fn apply_openai_chat(
    body: &mut Value,
    effort: ReasoningEffort,
    protocol: ReasoningProtocol,
) {
    body["reasoning_effort"] = json!(effort);
    match protocol {
        ReasoningProtocol::Deepseek => body["thinking"] = json!({"type": "enabled"}),
        ReasoningProtocol::Qwen => body["enable_thinking"] = json!(true),
        ReasoningProtocol::Standard => {}
    }
}

pub(crate) fn apply_openai_responses(
    body: &mut Value,
    effort: ReasoningEffort,
    _protocol: ReasoningProtocol,
) {
    body["reasoning"] = json!({"effort": effort});
}

pub(crate) fn apply_anthropic_messages(
    body: &mut Value,
    effort: ReasoningEffort,
    protocol: ReasoningProtocol,
) {
    body["thinking"] = match protocol {
        ReasoningProtocol::Deepseek => json!({"type": "enabled"}),
        ReasoningProtocol::Standard | ReasoningProtocol::Qwen => json!({"type": "adaptive"}),
    };
    body["output_config"] = json!({"effort": effort});
}
