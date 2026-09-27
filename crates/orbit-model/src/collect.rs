use std::collections::BTreeMap;

use futures_util::StreamExt;

use crate::{error::ModelError, types::*};

/// SSE-only account endpoints also serve non-interactive and summary callers.
pub(crate) async fn completion(
    mut stream: ModelStream,
    model: &str,
) -> Result<ChatCompletion, ModelError> {
    let mut text = String::new();
    let mut calls = BTreeMap::<usize, ToolCall>::new();
    let mut finished = None;

    while let Some(event) = stream.next().await {
        match event? {
            StreamEvent::TextDelta(delta) => text.push_str(&delta),
            StreamEvent::ReasoningDelta(_) => {}
            StreamEvent::ToolCallDelta(delta) => {
                let call = calls.entry(delta.index).or_insert_with(|| ToolCall {
                    metadata: None,
                    id: String::new(),
                    kind: "function".into(),
                    function: FunctionCall {
                        name: String::new(),
                        arguments: String::new(),
                    },
                });
                if let Some(id) = delta.id {
                    call.id = id;
                }
                if let Some(name) = delta.name {
                    call.function.name.push_str(&name);
                }
                if let Some(metadata) = delta.metadata {
                    call.metadata = Some(metadata);
                }
                call.function.arguments.push_str(&delta.arguments);
            }
            StreamEvent::Completed {
                finish_reason,
                usage,
            } => finished = Some((finish_reason, usage)),
        }
    }
    let (finish_reason, usage) = finished
        .ok_or_else(|| ModelError::Protocol("account response ended without completion".into()))?;

    Ok(ChatCompletion {
        id: None,
        model: Some(model.into()),
        message: ChatMessage::Assistant {
            content: (!text.is_empty()).then_some(text),
            tool_calls: calls.into_values().collect(),
        },
        finish_reason,
        usage,
    })
}
