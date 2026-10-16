//! Model-facing intent metadata, separated from validated tool arguments.
use serde_json::{Value, json};

use crate::types::{ToolCall, ToolDefinition};

const PURPOSE_FIELD: &str = "_orbit_purpose";
const MAX_PURPOSE_CHARS: usize = 160;

/// Adds optional intent to object schemas without taking over an existing business field.
/// Unsupported schemas retain their original contract and use parameter-based UI summaries.
pub fn advertise(definition: &mut ToolDefinition) -> bool {
    let schema = &mut definition.function.parameters;
    if schema["type"] != "object"
        || [
            "$ref",
            "allOf",
            "anyOf",
            "oneOf",
            "not",
            "if",
            "patternProperties",
            "propertyNames",
            "maxProperties",
            "unevaluatedProperties",
            "dependentSchemas",
            "dependencies",
        ]
        .iter()
        .any(|key| schema.get(key).is_some())
    {
        return false;
    }
    let Some(schema) = schema.as_object_mut() else {
        return false;
    };
    let Some(properties) = schema
        .entry("properties")
        .or_insert_with(|| json!({}))
        .as_object_mut()
    else {
        return false;
    };
    if properties.contains_key(PURPOSE_FIELD) {
        return false;
    }
    properties.insert(
        PURPOSE_FIELD.into(),
        json!({
            "type": "string",
            "description": "Briefly state this call's purpose in the user's language. Use one \
                short phrase, not a result claim. Do not repeat it as a separate progress message."
        }),
    );
    true
}

/// Removes display-only metadata before argument validation and policy evaluation.
/// Invalid or missing intent never causes an otherwise valid operation to be retried.
pub fn extract(call: &mut ToolCall) {
    let Ok(Value::Object(mut arguments)) = call.arguments_json() else {
        return;
    };
    let Some(purpose) = arguments.remove(PURPOSE_FIELD) else {
        return;
    };
    call.purpose = purpose.as_str().and_then(|text| {
        let text = text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let text: String = text
            .chars()
            .filter(|character| !character.is_control())
            .take(MAX_PURPOSE_CHARS)
            .collect();
        (!text.is_empty()).then_some(text)
    });
    call.function.arguments = Value::Object(arguments).to_string();
}

impl ToolCall {
    /// Restores intent only in model history; executors receive the original business object.
    pub fn model_arguments(&self) -> Result<Value, serde_json::Error> {
        let mut arguments = self.arguments_json()?;
        if let (Some(purpose), Some(object)) = (&self.purpose, arguments.as_object_mut()) {
            object.insert(PURPOSE_FIELD.into(), Value::String(purpose.clone()));
        }
        Ok(arguments)
    }

    pub fn model_arguments_text(&self) -> String {
        if self.purpose.is_none() {
            return self.function.arguments.clone();
        }
        self.model_arguments()
            .map(|arguments| arguments.to_string())
            .unwrap_or_else(|_| self.function.arguments.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ChatMessage, ChatRequest};

    #[test]
    fn provider_history_restores_purpose_only_inside_tool_arguments() {
        let mut call =
            ToolCall::function("generated-call", "inspect", json!({"entry": "module.rs"}));
        call.purpose = Some("Inspect module".into());
        let request = ChatRequest::new(vec![ChatMessage::Assistant {
            content: None,
            tool_calls: vec![call],
        }]);
        let (chat, _) = crate::openai::chat_completions::encode(
            "fixture",
            request.clone(),
            None,
            &Value::Null,
            false,
        )
        .unwrap();
        let wire_call = &chat["messages"][0]["tool_calls"][0];
        assert!(wire_call.get("purpose").is_none());
        let args: Value = serde_json::from_str(
            wire_call["function"]["arguments"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(args[PURPOSE_FIELD], "Inspect module");
        let (responses, _) =
            crate::openai::responses::encode("fixture", request.clone(), None, &Value::Null, false)
                .unwrap();
        assert!(
            responses["input"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| {
                    item["arguments"]
                        .as_str()
                        .is_some_and(|text| text.contains(PURPOSE_FIELD))
                })
        );
        let (anthropic, _) = crate::anthropic::messages::encode(
            "fixture",
            request.clone(),
            None,
            &Value::Null,
            false,
        )
        .unwrap();
        assert_eq!(
            anthropic["messages"][0]["content"][0]["input"][PURPOSE_FIELD],
            "Inspect module"
        );
        let (google, _) = crate::google::encode("fixture", request).unwrap();
        assert_eq!(
            google["request"]["contents"][0]["parts"][0]["functionCall"]["args"][PURPOSE_FIELD],
            "Inspect module"
        );
    }

    #[test]
    fn intent_round_trips_without_becoming_a_business_argument() {
        let arguments = json!({"entry": "module.rs", "_orbit_purpose": "Inspect entry points"});
        let mut call = ToolCall::function("inspect-1", "inspect", arguments.clone());
        extract(&mut call);
        assert_eq!(
            call.arguments_json().unwrap(),
            json!({"entry": "module.rs"})
        );
        assert_eq!(call.purpose.as_deref(), Some("Inspect entry points"));
        assert_eq!(call.model_arguments().unwrap(), arguments);
        let restored: ToolCall =
            serde_json::from_str(&serde_json::to_string(&call).unwrap()).unwrap();
        assert_eq!(restored, call);
    }

    #[test]
    fn invalid_intent_does_not_invalidate_execution_arguments() {
        for purpose in [Value::Null, json!(31), json!(" \n ")] {
            let mut call = ToolCall::function(
                "inspect-2",
                "inspect",
                json!({
                    "entry": "other.rs", "_orbit_purpose": purpose
                }),
            );
            extract(&mut call);
            assert!(call.purpose.is_none());
            assert_eq!(call.arguments_json().unwrap(), json!({"entry": "other.rs"}));
        }
    }

    #[test]
    fn schema_extension_preserves_required_fields_and_reserved_business_names() {
        let mut definition = ToolDefinition::function(
            "inspect",
            "Inspect",
            json!({
                "type": "object", "properties": {"entry": {"type": "string"}},
                "required": ["entry"], "additionalProperties": false
            }),
        );
        assert!(advertise(&mut definition));
        assert_eq!(definition.function.parameters["required"], json!(["entry"]));
        let before = definition.clone();
        assert!(!advertise(&mut definition));
        assert_eq!(before, definition);
    }

    #[test]
    fn composed_and_constrained_schemas_keep_the_original_contract() {
        for parameters in [
            json!({"type": "object", "maxProperties": 1}),
            json!({"type": "object", "allOf": [{"required": ["entry"]}]}),
            json!({"type": "object", "propertyNames": {"pattern": "^business_"}}),
            json!({"type": "object", "properties": {"_orbit_purpose": {"type": "number"}}}),
        ] {
            let mut definition = ToolDefinition::function("dynamic", "Inspect", parameters.clone());
            assert!(!advertise(&mut definition));
            assert_eq!(definition.function.parameters, parameters);
        }
    }
}
