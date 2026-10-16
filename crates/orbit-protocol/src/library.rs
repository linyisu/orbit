//! Global catalog management and immutable packages explicitly applied to one session.
use serde::{Deserialize, Serialize};

/// Decoded package payload budget, leaving room for base64 and JSON framing.
pub const MAX_BUNDLE_BYTES: usize = 16 * 1_024 * 1_024;
pub const MAX_BUNDLE_FILES: usize = 10_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExtensionKind {
    Skill,
    Plugin(super::plugins::Selection),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extension {
    pub key: String,
    pub name: String,
    pub kind: ExtensionKind,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackageFile {
    pub path: String,
    pub content: String,
    pub executable: bool,
}

/// Skill roots are package-relative directories; no path refers to the client filesystem.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Bundle {
    pub extension: Extension,
    pub skill_roots: Vec<String>,
    pub files: Vec<PackageFile>,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Action {
    Skills(super::resources::Action),
    Plugins(super::plugins::Action),
    ExportSkill { name: String },
    ExportPlugin { plugin: super::plugins::Selection },
}

#[derive(Debug, Serialize, Deserialize)]
pub enum Snapshot {
    Skills(super::resources::Snapshot),
    Plugins(super::plugins::Catalog),
    Bundle(Bundle),
}
