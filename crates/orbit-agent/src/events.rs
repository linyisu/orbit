use orbit_model::types::{StreamEvent, ToolCall};

#[derive(Clone, Debug)]
pub enum AgentEvent {
    TurnStarted { turn: usize },
    Model(StreamEvent),
    ToolStarted { call: ToolCall },
    ToolFinished { call: ToolCall, content: String },
    ToolFailed { call: ToolCall, error: String },
    Completed { turns: usize, tool_calls: usize },
    Failed { error: String },
    Cancelled,
}
