//! Structured settings operations, scoped to the connected session.
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub enum Action {
    Skills,
    ReadSkill {
        name: String,
    },
    SaveSkill {
        name: String,
        original: Option<String>,
        content: String,
    },
    Memories,
    Memory(super::project::MemoryAction),
    Mcp,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpServer {
    pub name: String,
    pub connected: bool,
    pub detail: String,
    pub tools: Vec<McpTool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpTool {
    pub name: String,
    pub description: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Skill {
    pub name: String,
    pub title: String,
    pub description: String,
    pub source: String,
    pub editable: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Snapshot {
    Skills(Vec<Skill>),
    Skill {
        name: String,
        content: String,
        editable: bool,
    },
    Memories(Vec<super::project::SessionMemory>),
    Mcp(Vec<McpServer>),
}
