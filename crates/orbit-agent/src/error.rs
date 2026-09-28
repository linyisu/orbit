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
    #[error(
        "agent reached the maximum number of turns (已达单次任务轮数上限；可在 config.toml 配置 max_turns = 0 取消限制，或发送“继续”)"
    )]
    MaxTurns,
    #[error(
        "agent reached the maximum number of tool calls (已达工具调用上限；可在 config.toml 配置 max_tool_calls = 0 取消限制)"
    )]
    MaxToolCalls,
    #[error(
        "estimated context {estimated} tokens exceeds budget {limit}; use /compact or raise context_tokens"
    )]
    ContextBudget { estimated: usize, limit: usize },
    #[error("context compaction failed: {0}")]
    Compaction(String),
    #[error("context compaction failed: {message}")]
    CompactionFailed {
        kind: crate::compaction::CompactionFailureKind,
        message: String,
    },
    #[error("agent was cancelled")]
    Cancelled,
    #[error("initial request must contain at least one message")]
    InvalidInitialRequest,
    #[error("agent event channel is closed")]
    EventChannelClosed,
}

impl AgentError {
    pub fn compaction_kind(&self) -> Option<crate::compaction::CompactionFailureKind> {
        match self {
            Self::CompactionFailed { kind, .. } => Some(*kind),
            _ => None,
        }
    }
}
