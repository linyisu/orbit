//! Display-safe quota evidence; no credentials or raw account payloads.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Snapshot {
    pub model: String,
    pub account: Account,
    pub session: Session,
}

impl Snapshot {
    /// Shared quota windows relevant to this model. Antigravity has separate
    /// Gemini and Claude/GPT pools; their order in the response is not meaningful.
    /// Keep all limits in `account` so the details view can still show both pools.
    pub fn summary_limits(&self) -> impl Iterator<Item = &Limit> {
        let antigravity = self.account.provider.eq_ignore_ascii_case("antigravity");
        let model = self.model.rsplit("::").next().unwrap_or(&self.model);
        let family = quota_family(model);
        self.account.limits.iter().filter(move |limit| {
            limit.scope == Scope::Account
                && (!antigravity
                    || quota_family(&limit.label).is_none_or(|group| Some(group) == family))
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum QuotaFamily {
    Gemini,
    ClaudeGpt,
}

fn quota_family(name: &str) -> Option<QuotaFamily> {
    let name = name.to_ascii_lowercase();
    if name.contains("gemini") {
        Some(QuotaFamily::Gemini)
    } else if name.contains("claude") || name.contains("gpt") {
        Some(QuotaFamily::ClaudeGpt)
    } else {
        None
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_uses_the_active_antigravity_pool_without_hiding_details() {
        let mut snapshot = Snapshot {
            account: Account {
                provider: "Antigravity".into(),
                limits: [
                    ("Gemini Models · Weekly Limit Remaining", 0.77),
                    ("Gemini Models · Five Hour Limit Remaining", 0.0),
                    ("Claude and GPT models · Weekly Limit Remaining", 0.98),
                    ("Claude and GPT models · Five Hour Limit Remaining", 0.93),
                ]
                .into_iter()
                .map(|(label, fraction)| Limit {
                    label: label.into(),
                    remaining_fraction: Some(fraction),
                    ..Default::default()
                })
                .collect(),
                ..Default::default()
            },
            ..Default::default()
        };
        for (model, expected) in [
            (
                "antigravity-account::gemini-3.8-flash-high",
                vec![0.77, 0.0],
            ),
            ("claude-opus-4-6", vec![0.98, 0.93]),
            ("gpt-oss-120b", vec![0.98, 0.93]),
            ("Gemini-3.8-pro", vec![0.77, 0.0]),
            ("unknown-model", vec![]),
        ] {
            snapshot.model = model.into();
            let remaining = snapshot
                .summary_limits()
                .map(|limit| limit.remaining_fraction.unwrap())
                .collect::<Vec<_>>();
            assert_eq!(remaining, expected, "{model}");
            assert_eq!(snapshot.account.limits.len(), 4);
        }
        snapshot.account.limits.reverse();
        snapshot.model = "gemini-pro".into();
        assert!(
            snapshot
                .summary_limits()
                .all(|limit| limit.label.starts_with("Gemini"))
        );
    }

    #[test]
    fn summary_preserves_generic_shared_limits_and_excludes_model_counters() {
        let snapshot = Snapshot {
            model: "arbitrary-model".into(),
            account: Account {
                provider: "Other".into(),
                limits: vec![
                    Limit {
                        label: "weekly".into(),
                        ..Default::default()
                    },
                    Limit {
                        label: "per-model".into(),
                        scope: Scope::Model,
                        ..Default::default()
                    },
                    Limit {
                        label: "5h".into(),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            snapshot
                .summary_limits()
                .map(|limit| limit.label.as_str())
                .collect::<Vec<_>>(),
            ["weekly", "5h"]
        );
    }
}
