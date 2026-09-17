//! Provider capability adapters for reasoning controls.
//!
//! Keep compatibility choices here rather than inferring them from a model ID
//! inside individual request encoders. A gateway can rename a model while its
//! provider-level protocol remains stable.

use serde_json::{Value, json};

use crate::types::ReasoningEffort;

fn merge_extra(body: &mut Value, extra: &Value) {
    match (body, extra) {
        (Value::Object(body), Value::Object(extra)) => {
            for (key, value) in extra {
                match body.get_mut(key) {
                    Some(existing) => merge_extra(existing, value),
                    None => {
                        body.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (body, extra) => *body = extra.clone(),
    }
}

pub(crate) fn apply_openai_chat(body: &mut Value, effort: ReasoningEffort, extra: &Value) {
    body["reasoning_effort"] = json!(effort);
    merge_extra(body, extra);
}

pub(crate) fn apply_openai_responses(body: &mut Value, effort: ReasoningEffort, extra: &Value) {
    body["reasoning"] = json!({"effort": effort});
    merge_extra(body, extra);
}

pub(crate) fn apply_anthropic_messages(body: &mut Value, effort: ReasoningEffort, extra: &Value) {
    body["thinking"] = json!({"type": "adaptive"});
    body["output_config"] = json!({"effort": effort});
    merge_extra(body, extra);
}
