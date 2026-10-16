//! Provider-independent stream assembly and terminal-response validation.

use std::collections::BTreeMap;

use crate::{
    error::ModelError,
    types::{
        ChatCompletion, ChatMessage, FunctionCall, StreamEvent, ToolCall, ToolCallDelta, Usage,
    },
};

/// Accumulates sparse tool fragments and partial usage until the provider confirms completion.
/// A usage-only `Completed` event never constitutes terminal evidence.
#[derive(Default)]
pub struct StreamAccumulator {
    text: String,
    calls: BTreeMap<usize, ToolCall>,
    finish_reason: Option<String>,
    usage: Option<Usage>,
}

impl StreamAccumulator {
    pub fn push(&mut self, event: StreamEvent) {
        match event {
            StreamEvent::TextDelta(delta) => self.text.push_str(&delta),
            StreamEvent::ReasoningDelta(_) => {}
            StreamEvent::ToolCallDelta(delta) => self.merge_tool_call(delta),
            StreamEvent::Completed {
                finish_reason,
                usage,
            } => {
                if finish_reason
                    .as_ref()
                    .is_some_and(|reason| !reason.trim().is_empty())
                {
                    self.finish_reason = finish_reason;
                }
                self.merge_usage(usage);
            }
        }
    }

    /// Rejects truncated streams before their partial tool calls can be executed.
    pub fn finish(self) -> Result<ChatCompletion, ModelError> {
        let finish_reason = self.finish_reason.ok_or_else(|| {
            ModelError::Protocol("model stream ended without a terminal finish reason".into())
        })?;
        Ok(ChatCompletion {
            id: None,
            model: None,
            message: ChatMessage::Assistant {
                content: (!self.text.is_empty()).then_some(self.text),
                tool_calls: self.calls.into_values().collect(),
            },
            finish_reason: Some(finish_reason),
            usage: self.usage,
        })
    }

    fn merge_usage(&mut self, update: Option<Usage>) {
        let Some(update) = update else { return };
        if let Some(current) = &mut self.usage {
            current.prompt_tokens = update
                .prompt_tokens
                .or(current.prompt_tokens);
            current.completion_tokens = update
                .completion_tokens
                .or(current.completion_tokens);
            current.total_tokens = update
                .total_tokens
                .or(current.total_tokens);
            current.cached_input_tokens = update
                .cached_input_tokens
                .or(current.cached_input_tokens);
            current.cache_write_input_tokens = update
                .cache_write_input_tokens
                .or(current.cache_write_input_tokens);
        } else {
            self.usage = Some(update);
        }
    }

    /// Providers index all output blocks; sparse indices must not create phantom tool calls.
    fn merge_tool_call(&mut self, delta: ToolCallDelta) {
        let call = self
            .calls
            .entry(delta.index)
            .or_insert_with(|| ToolCall {
                purpose: None,
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
        call.function
            .arguments
            .push_str(&delta.arguments);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{GoogleToolMetadata, ToolCallMetadata};

    #[test]
    fn partial_usage_preserves_terminal_reason_and_earlier_counts() {
        let mut stream = StreamAccumulator::default();
        stream.push(StreamEvent::Completed {
            finish_reason: None,
            usage: Some(Usage {
                prompt_tokens: Some(17),
                ..Default::default()
            }),
        });
        stream.push(StreamEvent::Completed {
            finish_reason: Some("length".into()),
            usage: Some(Usage {
                completion_tokens: Some(6),
                ..Default::default()
            }),
        });
        stream.push(StreamEvent::Completed {
            finish_reason: None,
            usage: Some(Usage {
                total_tokens: Some(23),
                ..Default::default()
            }),
        });
        let completion = stream.finish().unwrap();
        assert_eq!(completion.finish_reason.as_deref(), Some("length"));
        let usage = completion.usage.unwrap();
        assert_eq!(usage.prompt_tokens, Some(17));
        assert_eq!(usage.completion_tokens, Some(6));
        assert_eq!(usage.total_tokens, Some(23));
    }

    #[test]
    fn content_and_usage_without_terminal_evidence_remain_incomplete() {
        for event in [
            StreamEvent::TextDelta("partial result".into()),
            StreamEvent::ToolCallDelta(ToolCallDelta {
                metadata: None,
                index: 0,
                id: Some("operation".into()),
                name: Some("inspect".into()),
                arguments: "{}".into(),
            }),
        ] {
            let mut stream = StreamAccumulator::default();
            stream.push(event);
            stream.push(StreamEvent::Completed {
                finish_reason: None,
                usage: Some(Usage::default()),
            });
            assert!(stream.finish().is_err());
        }
        assert!(
            StreamAccumulator::default()
                .finish()
                .is_err()
        );
    }

    #[test]
    fn sparse_tool_fragments_retain_provider_metadata_and_history_shape() {
        let metadata = ToolCallMetadata {
            google: GoogleToolMetadata {
                model: "synthetic-provider".into(),
                thought_signature: Some("opaque-evidence".into()),
                native_id: None,
            },
        };
        let mut stream = StreamAccumulator::default();
        for (index, id, arguments) in [
            (usize::MAX, Some("later"), "{\"count\":"),
            (2, Some("first"), "{}"),
            (usize::MAX, None, "3}"),
        ] {
            stream.push(StreamEvent::ToolCallDelta(ToolCallDelta {
                metadata: id.map(|_| metadata.clone()),
                index,
                id: id.map(str::to_owned),
                name: id.map(|_| "inspect".into()),
                arguments: arguments.into(),
            }));
        }
        stream.push(StreamEvent::Completed {
            finish_reason: Some("tool_calls".into()),
            usage: None,
        });
        let completion = stream.finish().unwrap();
        let restored: ChatMessage =
            serde_json::from_str(&serde_json::to_string(&completion.message).unwrap()).unwrap();
        assert_eq!(restored, completion.message);
        let ChatMessage::Assistant { tool_calls, .. } = restored else {
            panic!("assistant expected")
        };
        assert_eq!(tool_calls.len(), 2);
        assert_eq!(tool_calls[0].id, "first");
        assert_eq!(tool_calls[1].function.arguments, "{\"count\":3}");
        assert_eq!(tool_calls[1].metadata, Some(metadata));
    }
}
