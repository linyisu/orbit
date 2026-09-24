use orbit_model::types::{ChatMessage, StreamEvent, ToolCall};

use crate::context::ContextCheckpoint;

#[derive(Clone, Debug)]
pub enum AgentEvent {
    TurnStarted {
        turn: usize,
    },
    ModelRequestPrepared {
        estimated_input_tokens: usize,
    },
    ModelUsageObserved {
        local_estimate: usize,
        estimated_input_tokens: usize,
        actual_input_tokens: Option<u64>,
    },
    Model(StreamEvent),
    MessageCommitted {
        message: ChatMessage,
    },
    ContextCheckpoint {
        checkpoint: ContextCheckpoint,
    },
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
    Cancelled,
}
