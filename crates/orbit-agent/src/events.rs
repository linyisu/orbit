use orbit_model::types::{ChatMessage, StreamEvent, ToolCall};

#[derive(Clone, Debug)]
pub enum AgentEvent {
    TurnStarted { turn: usize },
    Model(StreamEvent),
    MessageCommitted { message: ChatMessage },
    ToolStarted { call: ToolCall },
    ToolFinished { call: ToolCall, content: String },
    ToolFailed { call: ToolCall, error: String },
    Completed { turns: usize, tool_calls: usize },
    Failed { error: String },
    Cancelled,
}
