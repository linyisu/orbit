use orbit_model::types::{StreamEvent, ToolCall};

use crate::tool::FileChange;

/// Maximum UTF-8 byte size after coalescing adjacent stream fragments.
pub const MAX_STREAM_FRAGMENT_BYTES: usize = 64 * 1_024;

/// Coalesce adjacent stream fragments without crossing semantic barriers.
/// Live delivery may use this only before publishing the previous event's cursor.
pub fn coalesce(previous: &mut AgentEvent, next: &AgentEvent) -> bool {
    match (previous, next) {
        (
            AgentEvent::Model(StreamEvent::TextDelta(a)),
            AgentEvent::Model(StreamEvent::TextDelta(b)),
        )
        | (
            AgentEvent::Model(StreamEvent::ReasoningDelta(a)),
            AgentEvent::Model(StreamEvent::ReasoningDelta(b)),
        ) if a.len().saturating_add(b.len()) <= MAX_STREAM_FRAGMENT_BYTES => {
            a.push_str(b);
            true
        }
        (
            AgentEvent::ContextUsage { estimated_input: a },
            AgentEvent::ContextUsage { estimated_input: b },
        ) => {
            *a = *b;
            true
        }
        (
            AgentEvent::Model(StreamEvent::ToolCallDelta(a)),
            AgentEvent::Model(StreamEvent::ToolCallDelta(b)),
        ) if a.index == b.index
            && a.arguments
                .len()
                .saturating_add(b.arguments.len())
                <= MAX_STREAM_FRAGMENT_BYTES
            && a.id
                .as_ref()
                .zip(b.id.as_ref())
                .is_none_or(|(a, b)| a == b)
            && a.metadata
                .as_ref()
                .zip(b.metadata.as_ref())
                .is_none_or(|(a, b)| a == b) =>
        {
            if let Some(id) = &b.id {
                a.id = Some(id.clone());
            }
            if let Some(metadata) = &b.metadata {
                a.metadata = Some(metadata.clone());
            }
            if let Some(name) = &b.name {
                a.name
                    .get_or_insert_default()
                    .push_str(name);
            }
            a.arguments.push_str(&b.arguments);
            true
        }
        _ => false,
    }
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum AgentEvent {
    TurnStarted {
        turn: usize,
    },
    Model(StreamEvent),
    ReasoningFinished {
        elapsed_ms: u64,
    },
    /// Estimated size of the request about to be sent, including tool schemas.
    ContextUsage {
        estimated_input: usize,
    },
    RequestMetrics {
        profile: String,
        estimated_input: usize,
        #[serde(default)]
        retries: Option<u32>,
        usage: Option<orbit_model::types::Usage>,
        elapsed_ms: u64,
    },
    ToolStarted {
        call: ToolCall,
    },
    ToolBackgrounded {
        call: ToolCall,
        job_id: String,
        content: String,
    },
    WaitingForCommands {
        job_ids: Vec<String>,
    },
    WaitingForTasks {
        job_ids: Vec<String>,
    },
    InputReceived {
        message_id: i64,
        content: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<orbit_model::types::Attachment>,
    },
    ToolFinished {
        call: ToolCall,
        content: String,
        /// Absent only in legacy receipts; new tools report their outcome explicitly.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outcome: Option<crate::tool::ToolOutcome>,
        /// Old event records omit this field; they remain readable and simply
        /// have no reversible workspace mutation attached.
        #[serde(default)]
        changes: Vec<FileChange>,
        /// Missing only in legacy events, whose receipts are reconstructed on read.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        files: Option<crate::memory::FileActivity>,
    },
    VerificationRecorded {
        call_id: String,
        evidence: orbit_runtime::verification::VerificationEvidence,
    },
    ToolFailed {
        call: ToolCall,
        error: String,
    },
    Completed {
        turns: usize,
        tool_calls: usize,
    },
    Failed {
        error: String,
    },
    CompactionStarted,
    CompactionRecorded {
        record: crate::memory::CompactionRecord,
    },
    ContextCompacted {
        #[serde(default)]
        strategy: crate::memory::CompactionStrategy,
        before_tokens: usize,
        after_tokens: usize,
        /// The provider could not produce a usable structured summary, so the
        /// local deterministic fallback retained only the safe working set.
        #[serde(default)]
        fallback: bool,
        /// Sanitized persistence-only detail for diagnostics. It is omitted
        /// from normal TUI status text so maintenance failures do not drown
        /// out the conversation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fallback_reason: Option<String>,
    },
    CompactionFailed {
        #[serde(default)]
        kind: Option<crate::compaction::CompactionFailureKind>,
        error: String,
    },
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_tool_events_without_changes_remain_readable() {
        let event: AgentEvent = serde_json::from_value(serde_json::json!({
            "ToolFinished": {
                "call": {
                    "id": "call-1",
                    "function": {
                        "name": "WriteFile",
                        "arguments": "{}"
                    }
                },
                "content": "{\"path\":\"a.txt\"}"
            }
        }))
        .unwrap();

        assert!(matches!(
            event,
            AgentEvent::ToolFinished { changes, .. } if changes.is_empty()
        ));
    }
}
