//! Display-safe quota evidence; no credentials or raw account payloads.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub model: String,
    pub account: Account,
    pub session: Session,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Account {
    /// Opaque source fingerprint, used only to keep stale data account-scoped.
    pub identity: String,
    pub provider: String,
    pub name: String,
    pub plan: Option<String>,
    pub fetched_at: Option<u64>,
    pub limits: Vec<Limit>,
    pub note: Option<String>,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Limit {
    pub label: String,
    #[serde(default)]
    pub scope: Scope,
    /// Fraction remaining. Missing is distinct from exhausted.
    pub remaining_fraction: Option<f64>,
    /// Preserve decimal amounts/currencies instead of converting to tokens.
    pub remaining: Option<String>,
    pub used: Option<String>,
    pub total: Option<String>,
    pub unit: Option<String>,
    /// Server's absolute timestamp (Unix seconds or ISO 8601).
    pub resets_at: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scope {
    #[default]
    Account,
    Model,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Session {
    pub runs: usize,
    pub requests: usize,
    pub runs_with_usage: usize,
    pub input: Option<u64>,
    pub output: Option<u64>,
    pub cached_input: Option<u64>,
    pub cache_write: Option<u64>,
    pub error: Option<String>,
}
