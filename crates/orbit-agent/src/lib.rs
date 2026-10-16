//! Model and tool execution, streaming events, checkpoints and context compaction.
//! The host supplies persistence, permissions and execution resources.

pub mod agent;
pub mod archive;
pub mod autocompact;
pub mod background;
pub mod calibration;
pub mod checkpoint;
pub mod compaction;
pub mod context;
pub mod error;
pub mod events;
pub mod inbox;
pub mod memory;
pub mod model;
pub mod runtime_state;
pub mod tool;

pub use agent::{Agent, AgentConfig, AgentResult, model_messages};
pub use orbit_tool_macros::tool;

/// Implementation dependencies of the tool attribute; not an application-facing API.
#[doc(hidden)]
pub mod __private {
    pub use orbit_model::types::{ToolCall, ToolDefinition};
    pub use serde_json::from_str;
}
