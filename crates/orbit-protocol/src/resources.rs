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
}
