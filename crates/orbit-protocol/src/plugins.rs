use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Marketplace {
    pub name: String,
    pub source: String,
    pub directory: PathBuf,
    /// The immutable commit acquired for a Git source. Local directories have
    /// no revision because their owner can edit them in place.
    #[serde(default)]
    pub revision: Option<String>,
    pub plugins: Vec<Plugin>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Plugin {
    pub name: String,
    pub description: String,
    /// Relative package path declared by the marketplace. Older stored
    /// catalogues may not have it, so it remains optional until refreshed.
    #[serde(default)]
    pub source: Option<String>,
}

/// Package declarations only; inspection never starts hooks, commands or MCP servers.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginPackage {
    pub name: String,
    pub version: Option<String>,
    pub description: Option<String>,
    pub directory: PathBuf,
    pub skills: Vec<String>,
    pub commands: Vec<String>,
    pub agents: Vec<String>,
    pub declares_hooks: bool,
    pub declares_mcp: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    List,
    Add {
        source: String,
    },
    Enable {
        plugin: super::project::ProjectPlugin,
    },
    Disable {
        marketplace_source: String,
        plugin_name: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Inspection {
    pub source: String,
    pub name: String,
    pub package: Result<PluginPackage, String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Catalog {
    pub marketplaces: Vec<Marketplace>,
    pub project_id: String,
    pub enabled: Vec<super::project::ProjectPlugin>,
    pub inspections: Vec<Inspection>,
}
