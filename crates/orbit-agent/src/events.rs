use orbit_model::types::{StreamEvent, ToolCall};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub enum AgentEvent {
    TurnStarted {
        turn: usize,
    },
    Model(StreamEvent),
    ToolStarted {
        call: ToolCall,
    },
    ToolFinished {
        call: ToolCall,
        content: String,
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
        before_tokens: usize,
        after_tokens: usize,
        /// The provider could not produce a usable structured summary, so the
        /// local deterministic fallback retained only the safe working set.
        #[serde(default)]
        fallback: bool,
    },
    CompactionFailed {
        error: String,
    },
    Cancelled,
}
