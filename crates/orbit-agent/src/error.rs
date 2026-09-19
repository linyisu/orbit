use orbit_model::error::ModelError;
use thiserror::Error;

use crate::tool::ToolError;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("execution checkpoint failed: {0}")]
    Checkpoint(String),
    #[error("model failed: {0}")]
    Model(#[from] ModelError),
    #[error("tool failed: {0}")]
    Tool(#[from] ToolError),
    #[error("agent reached the maximum number of turns")]
    MaxTurns,
    #[error("agent reached the maximum number of tool calls")]
    MaxToolCalls,
    #[error(
        "estimated context {estimated} tokens exceeds budget {limit}; use /compact or raise context_tokens"
    )]
    ContextBudget { estimated: usize, limit: usize },
    #[error("context compaction failed: {0}")]
    Compaction(String),
    #[error("agent was cancelled")]
    Cancelled,
    #[error("initial request must contain at least one message")]
    InvalidInitialRequest,
    #[error("agent event channel is closed")]
    EventChannelClosed,
}
