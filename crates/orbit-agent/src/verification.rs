//! Host-side verification evidence; never inserted into model tool output.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceFingerprint {
    pub digest: String,
    pub files: BTreeMap<String, FileFingerprint>,
    pub directories: Vec<String>,
    pub skipped_entries: usize,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileFingerprint {
    pub sha256: String,
    pub executable: bool,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationEvidence {
    pub input: WorkspaceFingerprint,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<WorkspaceFingerprint>,
    pub platform: String,
    pub isolation: String,
    pub rust_toolchain: Option<String>,
    pub writable_snapshot: bool,
    pub persistent_scratch: bool,
    #[serde(default)]
    pub dependency_caches: Vec<String>,
}
