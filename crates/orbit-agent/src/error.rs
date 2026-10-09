use orbit_model::error::ModelError;
use thiserror::Error;

use crate::tool::ToolError;

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("runtime state refresh failed: {0}")]
    RuntimeState(String),
    #[error("execution checkpoint failed: {0}")]
    Checkpoint(String),
    #[error("model failed: {0}")]
    Model(#[from] ModelError),
    #[error("tool failed: {0}")]
    Tool(#[from] ToolError),
    #[error(
        "已达配置的任务轮数上限；在 config.toml 将主代理的 max_turns 或 [subagents] 的 max_turns 设为 0 可取消限制，也可发送消息续跑"
    )]
    MaxTurns,
    #[error(
        "已达配置的工具调用上限；在 config.toml 将主代理的 max_tool_calls 或 [subagents] 的 max_tool_calls 设为 0 可取消限制"
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
