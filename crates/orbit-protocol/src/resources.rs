//! Structured settings operations, scoped to the connected session.
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub enum Action {
    Skills,
    ScanSkills {
        source: String,
    },
    ImportSkills {
        token: String,
        paths: Vec<String>,
    },
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
    /// Validated by the service before any process is started. Configuration
    /// travels from the client so remote sessions use the same saved settings.
    ReloadMcp {
        config: serde_json::Value,
        allow_start: bool,
    },
    ScanMcp {
        source: String,
    },
    InstallMcp {
        token: String,
        id: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpLaunch {
    pub command: String,
    pub args: Vec<String>,
    pub env: std::collections::BTreeMap<String, String>,
    pub tools: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpCandidate {
    pub id: String,
    pub name: String,
    pub launch: McpLaunch,
    pub install: Vec<Vec<String>>,
    pub description: String,
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

/// A skill discovered in a session-owned, immutable source snapshot.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillCandidate {
    pub path: String,
    pub name: String,
    pub title: String,
    pub description: String,
    pub conflict: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Snapshot {
    Skills(Vec<Skill>),
    SkillSource {
        token: String,
        source: String,
        skills: Vec<SkillCandidate>,
    },
    Skill {
        name: String,
        content: String,
        editable: bool,
    },
    Memories(Vec<super::project::SessionMemory>),
    Mcp(Vec<McpServer>),
    McpSource {
        token: String,
        source: String,
        candidates: Vec<McpCandidate>,
    },
    McpInstalled {
        name: String,
        launch: McpLaunch,
    },
}
