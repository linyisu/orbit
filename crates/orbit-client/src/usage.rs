//! Usage query and evidence normalization for both presentation clients.
use crate::{
    protocol::{Request, Response},
    transport::Connection,
};
use anyhow::{Result, bail};
use orbit_protocol::usage::{Account, BalanceKind, Limit, LimitKind, Scope, Snapshot};
use std::time::Duration;

pub const REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);
pub const CACHE_AGE: Duration = Duration::from_secs(60);
const DISPLAY_BALANCE_CURRENCY: &str = "CNY";

/// Present DeepSeek's CNY evidence while retaining all currencies in the source snapshot.
fn visible_limit(account: &Account, limit: &Limit) -> bool {
    !account
        .provider
        .eq_ignore_ascii_case("deepseek")
        || limit
            .unit
            .as_deref()
            .is_some_and(|unit| unit.eq_ignore_ascii_case(DISPLAY_BALANCE_CURRENCY))
}

/// Account details use the same currency policy in both frontends.
pub fn account_limits(account: &Account, scope: Scope) -> impl Iterator<Item = &Limit> {
    account
        .limits
        .iter()
        .filter(move |limit| limit.scope == scope && visible_limit(account, limit))
}

/// Summarize independent quota windows and currency totals without their balance components.
pub fn summary_limits(snapshot: &Snapshot) -> Vec<&Limit> {
    snapshot
        .summary_limits()
        .filter(|limit| {
            visible_limit(&snapshot.account, limit)
                && matches!(
                    limit.kind,
                    LimitKind::Quota | LimitKind::Balance(BalanceKind::Total)
                )
        })
        .collect()
}

/// Keep exact provider decimals and their currency or count unit together.
pub fn amount(limit: &Limit) -> String {
    format!(
        "{} {}",
        limit
            .remaining
            .as_deref()
            .unwrap_or("—"),
        limit.unit.as_deref().unwrap_or("")
    )
    .trim()
    .into()
}

/// Use one readable quota vocabulary in both clients without dropping unfamiliar labels.
pub fn quota_label(label: &str) -> String {
    let mut parts = label
        .split('·')
        .map(|part| {
            match part
                .trim()
                .to_ascii_lowercase()
                .as_str()
            {
                "weekly limit remaining" | "weekly" | "week" | "每周" | "周" => "周".to_owned(),
                "five hour limit remaining" | "five_hour" | "5h" | "5小时" => "5h".to_owned(),
                _ => part
                    .trim()
                    .replace("Gemini Models", "Gemini")
                    .replace("Claude and GPT models", "Claude / GPT"),
            }
        })
        .collect::<Vec<_>>();
    parts.dedup();
    parts.join(" · ")
}

/// Compact recurring windows, retaining a meaningful label for other quota evidence.
pub fn summary_label(limit: &Limit) -> String {
    if let LimitKind::Balance(kind) = limit.kind {
        return match kind {
            BalanceKind::Total => "总余额",
            BalanceKind::Granted => "赠送余额",
            BalanceKind::ToppedUp => "充值余额",
        }
        .into();
    }
    let label = quota_label(&limit.label);
    if label
        .split('·')
        .any(|part| part.trim() == "周")
    {
        "周".into()
    } else if label
        .split('·')
        .any(|part| part.trim() == "5h")
    {
        "5h".into()
    } else {
        label
    }
}

/// Query on an independent connection so quota latency cannot block approvals or cancellation.
pub async fn fetch(connection: &Connection, session: String) -> Result<Snapshot> {
    match connection
        .connect()
        .await?
        .request(Request::Usage { session })
        .await?
    {
        Response::Usage(snapshot) => Ok(snapshot),
        _ => bail!("工作区服务未返回用量信息"),
    }
}

/// Preserve successful evidence only for the same account and model after a provider failure.
/// Summary selection belongs to that model and cannot be reused across model changes.
pub fn retain_account(mut snapshot: Snapshot, previous: Option<&Snapshot>) -> Snapshot {
    if snapshot.account.error.is_some()
        && let Some(old) = previous
        && old.account.identity == snapshot.account.identity
        && old.model == snapshot.model
        && (snapshot.account.provider.is_empty()
            || old.account.provider == snapshot.account.provider)
    {
        let error = snapshot.account.error.take();
        snapshot.account = old.account.clone();
        snapshot.account.error = error;
    }
    snapshot
}

/// Normalize the server's absolute Unix seconds, Unix milliseconds or RFC 3339 timestamp.
pub fn reset_timestamp(value: &str) -> Option<i64> {
    const MILLISECOND_TIMESTAMP_THRESHOLD: i64 = 1_000_000_000_000;
    value
        .parse::<i64>()
        .ok()
        .map(|time| {
            if time > MILLISECOND_TIMESTAMP_THRESHOLD {
                time / 1_000
            } else {
                time
            }
        })
        .or_else(|| {
            chrono::DateTime::parse_from_rfc3339(value)
                .ok()
                .map(|time| time.timestamp())
        })
        .filter(|time| *time > 0)
}

/// Reconstruct input-token evidence from the same stream in every frontend.
#[derive(Clone, Debug, Default)]
pub struct ContextUsage {
    calibrations: std::collections::BTreeMap<String, orbit_agent::calibration::TokenCalibration>,
    profile: Option<ContextProfile>,
    estimated_input: Option<usize>,
    used_tokens: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ContextProfile {
    model: String,
    reasoning_effort: Option<String>,
    context_tokens: usize,
    input_context_tokens: usize,
    max_output_tokens: u32,
}

impl ContextUsage {
    /// Apply a confirmed profile and refresh occupancy without replaying a model request.
    /// Return whether a previously selected execution profile changed.
    pub fn select_profile(&mut self, info: &crate::protocol::SessionInfo) -> bool {
        let profile = ContextProfile {
            model: info.model.clone(),
            reasoning_effort: info.reasoning_effort.clone(),
            context_tokens: info.context_tokens,
            input_context_tokens: info.input_context_tokens,
            max_output_tokens: info.max_output_tokens,
        };
        let changed = self
            .profile
            .as_ref()
            .is_some_and(|previous| previous != &profile);
        self.used_tokens = info.context_used_tokens.or_else(|| {
            self.estimated_input.map(|estimated| {
                self.calibration(&info.model)
                    .estimate(estimated)
            })
        });
        self.profile = Some(profile);
        changed
    }

    pub fn used_tokens(&self) -> Option<usize> {
        self.used_tokens
    }

    pub fn calibration(&self, model: &str) -> orbit_agent::calibration::TokenCalibration {
        self.calibrations
            .get(model)
            .cloned()
            .unwrap_or_default()
    }

    /// Prefer reported provider input, retaining bounded per-profile calibration for estimates.
    pub fn observe(
        &mut self,
        model: &str,
        event: &orbit_agent::events::AgentEvent,
    ) -> Option<usize> {
        use orbit_agent::events::AgentEvent;
        let used = match event {
            AgentEvent::ContextUsage { estimated_input } => {
                self.estimated_input = Some(*estimated_input);
                Some(
                    self.calibration(model)
                        .estimate(*estimated_input),
                )
            }
            AgentEvent::RequestMetrics {
                profile,
                estimated_input,
                usage,
                ..
            } => {
                self.estimated_input = Some(*estimated_input);
                let reported_input = usage
                    .as_ref()
                    .and_then(|usage| usage.prompt_tokens);
                self.calibrations
                    .entry(profile.clone())
                    .or_default()
                    .observe(*estimated_input, reported_input);
                Some(
                    reported_input
                        .filter(|_| profile == model)
                        .and_then(|tokens| usize::try_from(tokens).ok())
                        .filter(|tokens| *tokens > 0)
                        .unwrap_or_else(|| {
                            self.calibration(model)
                                .estimate(*estimated_input)
                        }),
                )
            }
            AgentEvent::ContextCompacted { after_tokens, .. } => {
                self.estimated_input = Some(*after_tokens);
                Some(
                    self.calibration(model)
                        .estimate(*after_tokens),
                )
            }
            _ => None,
        };
        if let Some(used) = used {
            self.used_tokens = Some(used);
        }
        used
    }
}

/// Input occupancy uses the independently resolved input budget, never total output capacity.
pub fn context_percent(used: usize, input_budget: usize) -> usize {
    used.saturating_mul(100) / input_budget.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context_profile(model: &str, used: Option<usize>) -> crate::protocol::SessionInfo {
        serde_json::from_value(serde_json::json!({
            "id": "generated-session",
            "title": "Generated task",
            "workspace": "/generated/workspace",
            "model": model,
            "models": [],
            "supports_vision": false,
            "reasoning_effort": null,
            "reasoning_efforts": [],
            "context_tokens": 10_000,
            "input_context_tokens": 9_000,
            "max_output_tokens": 1_000,
            "context_used_tokens": used
        }))
        .unwrap()
    }

    #[test]
    fn confirmed_profiles_refresh_occupancy_and_only_invalidate_execution_changes() {
        let mut context = ContextUsage::default();
        let mut info = context_profile("alpha", Some(700));
        assert!(!context.select_profile(&info));
        assert_eq!(context.used_tokens(), Some(700));
        info.title = "Renamed generated task".into();
        info.mode = orbit_protocol::Mode::Ask;
        info.context_used_tokens = Some(800);
        assert!(!context.select_profile(&info));
        assert_eq!(context.used_tokens(), Some(800));
        info.model = "beta".into();
        info.context_used_tokens = Some(300);
        assert!(context.select_profile(&info));
        assert_eq!(context.used_tokens(), Some(300));
        info.input_context_tokens = 8_000;
        assert!(context.select_profile(&info));
        assert!(!context.select_profile(&info));
    }

    #[test]
    fn legacy_profiles_recalibrate_event_evidence_without_reusing_another_models_usage() {
        use orbit_agent::events::AgentEvent;
        let mut context = ContextUsage::default();
        context.select_profile(&context_profile("alpha", None));
        let metrics = AgentEvent::RequestMetrics {
            profile: "alpha".into(),
            estimated_input: 120,
            retries: None,
            usage: Some(orbit_model::types::Usage {
                prompt_tokens: Some(240),
                ..Default::default()
            }),
            elapsed_ms: 10,
        };
        assert_eq!(context.observe("alpha", &metrics), Some(240));
        assert!(context.select_profile(&context_profile("beta", None)));
        assert_eq!(context.used_tokens(), Some(120));
        assert_eq!(context.observe("beta", &metrics), Some(120));
        assert!(context.select_profile(&context_profile("alpha", None)));
        assert_eq!(context.used_tokens(), Some(240));
    }

    #[test]
    fn balance_summary_keeps_currency_totals_without_counting_their_components() {
        let mut snapshot = Snapshot::default();
        snapshot.account.provider = "DeepSeek".into();
        snapshot.account.limits = [("USD", "8.1250"), ("CNY", "0.00")]
            .into_iter()
            .flat_map(|(unit, total)| {
                [
                    BalanceKind::Total,
                    BalanceKind::Granted,
                    BalanceKind::ToppedUp,
                ]
                .map(|kind| Limit {
                    label: format!("{unit} · {kind:?}"),
                    kind: LimitKind::Balance(kind),
                    remaining: Some(total.into()),
                    unit: Some(unit.into()),
                    ..Default::default()
                })
            })
            .collect();
        let limits = summary_limits(&snapshot);
        assert_eq!(limits.len(), 1);
        assert_eq!(amount(limits[0]), "0.00 CNY");
        assert_eq!(account_limits(&snapshot.account, Scope::Account).count(), 3);
        assert_eq!(summary_label(limits[0]), "总余额");
        assert_eq!(summary_label(&snapshot.account.limits[4]), "赠送余额");
        assert_eq!(summary_label(&snapshot.account.limits[5]), "充值余额");
        assert_eq!(snapshot.account.limits.len(), 6);
        snapshot.account.limits[3].remaining = Some("6.0400".into());
        assert_eq!(amount(summary_limits(&snapshot)[0]), "6.0400 CNY");
        assert_eq!(summary_limits(&snapshot).len(), 1);
        for limit in &mut snapshot.account.limits {
            limit.remaining = Some("0.00".into());
        }
        assert_eq!(summary_limits(&snapshot).len(), 1);
        let other = Limit {
            label: "每日请求".into(),
            remaining: Some("15".into()),
            unit: Some("次".into()),
            ..Default::default()
        };
        assert_eq!(summary_label(&other), "每日请求");
        assert_eq!(amount(&other), "15 次");
    }

    #[test]
    fn context_evidence_is_calibrated_per_profile_and_compaction_updates_the_same_count() {
        use orbit_agent::events::AgentEvent;
        let mut context = ContextUsage::default();
        let estimate = |tokens| AgentEvent::ContextUsage {
            estimated_input: tokens,
        };
        assert_eq!(context.observe("alpha", &estimate(150)), Some(150));
        let reported = AgentEvent::RequestMetrics {
            profile: "alpha".into(),
            estimated_input: 150,
            retries: None,
            usage: Some(orbit_model::types::Usage {
                prompt_tokens: Some(300),
                ..Default::default()
            }),
            elapsed_ms: 20,
        };
        assert_eq!(context.observe("alpha", &reported), Some(300));
        assert_eq!(context.observe("alpha", &estimate(200)), Some(400));
        assert_eq!(context.observe("beta", &estimate(200)), Some(200));
        let compacted = AgentEvent::ContextCompacted {
            before_tokens: 200,
            after_tokens: 40,
            strategy: Default::default(),
            fallback: false,
            fallback_reason: None,
        };
        assert_eq!(context.observe("alpha", &compacted), Some(80));
        assert_eq!(context_percent(80, 100), 80);
        assert_eq!(context.calibration("alpha").samples(), 1);
    }

    #[test]
    fn quota_timestamps_have_one_unit_policy_for_every_frontend() {
        assert_eq!(reset_timestamp("1800000000"), Some(1_800_000_000));
        assert_eq!(reset_timestamp("1800000000000"), Some(1_800_000_000));
        for value in ["2027-01-15T08:00:00+08:00", "2027-01-15T00:00:00Z"] {
            assert_eq!(reset_timestamp(value), Some(1_799_971_200));
        }
        for value in ["", "invalid", "0", "-4"] {
            assert_eq!(reset_timestamp(value), None);
        }
    }

    #[test]
    fn stale_account_limits_do_not_cross_account_boundaries() {
        let mut old = Snapshot::default();
        old.account.identity = "source-a".into();
        old.account.provider = "provider".into();
        old.account
            .limits
            .push(Default::default());
        let mut failed = old.clone();
        failed.account.limits.clear();
        failed.account.error = Some("offline".into());
        assert_eq!(
            retain_account(failed.clone(), Some(&old))
                .account
                .limits
                .len(),
            1
        );
        failed.model = "generated-other-model".into();
        assert!(
            retain_account(failed.clone(), Some(&old))
                .account
                .limits
                .is_empty()
        );
        failed.model = old.model.clone();
        failed.account.identity = "source-b".into();
        assert!(
            retain_account(failed, Some(&old))
                .account
                .limits
                .is_empty()
        );
    }
}
