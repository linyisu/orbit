//! Display-safe quota evidence; no credentials or raw account payloads.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub model: String,
    pub account: Account,
    pub session: Session,
}

impl Snapshot {
    /// Account limits selected by the service for this model; old snapshots include all pools.
    pub fn summary_limits(&self) -> impl Iterator<Item = &Limit> {
        self.account
            .limits
            .iter()
            .filter(|limit| {
                limit.scope == Scope::Account && limit.include_in_summary.unwrap_or(true)
            })
    }
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
    pub kind: LimitKind,
    #[serde(default)]
    pub scope: Scope,
    /// Provider adapter selection for the active model; absent in older snapshots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include_in_summary: Option<bool>,
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

/// Identifies monetary evidence without inferring its meaning from display labels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LimitKind {
    #[default]
    Quota,
    Balance(BalanceKind),
}

/// Provider-reported balance components; the total includes granted and topped-up funds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BalanceKind {
    Total,
    Granted,
    ToppedUp,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_uses_service_selection_and_excludes_model_counters() {
        let mut snapshot = Snapshot::default();
        snapshot.account.limits = [
            (Scope::Account, None),
            (Scope::Account, Some(true)),
            (Scope::Account, Some(false)),
            (Scope::Model, None),
            (Scope::Model, Some(true)),
            (Scope::Model, Some(false)),
        ]
        .into_iter()
        .enumerate()
        .map(|(index, (scope, selected))| Limit {
            label: format!("generated-pool-{index}"),
            scope,
            include_in_summary: selected,
            ..Default::default()
        })
        .collect();
        assert_eq!(
            snapshot
                .summary_limits()
                .map(|limit| limit.label.as_str())
                .collect::<Vec<_>>(),
            ["generated-pool-0", "generated-pool-1"]
        );
    }
}
