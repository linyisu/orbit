use crate::tool::FileChange;
use orbit_model::types::{StreamEvent, ToolCall};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum AgentEvent {
    TurnStarted {
        turn: usize,
    },
    Model(StreamEvent),
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
    ToolFinished {
        call: ToolCall,
        content: String,
        /// Old event records omit this field; they remain readable and simply
        /// have no reversible workspace mutation attached.
        #[serde(default)]
        changes: Vec<FileChange>,
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
                    "function": { "name": "write_file", "arguments": "{}" }
                },
                "content": "{\"path\":\"a.txt\"}"
            }
        }))
        .unwrap();

        assert!(matches!(event, AgentEvent::ToolFinished { changes, .. } if changes.is_empty()));
    }
}
