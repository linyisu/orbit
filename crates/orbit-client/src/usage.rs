//! Usage query and evidence normalization for both presentation clients.
use crate::{
    protocol::{Request, Response},
    transport::Connection,
};
use anyhow::{Result, bail};
use orbit_protocol::usage::Snapshot;
use std::time::Duration;

pub const REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);
pub const CACHE_AGE: Duration = Duration::from_secs(60);

/// Query on an independent connection so quota latency cannot block approvals or cancellation.
pub async fn fetch(connection: &Connection, session: String) -> Result<Snapshot> {
    match connection
        .connect()
        .await?
        .request(Request::Usage { session })
        .await?
    {
        Response::Usage(snapshot) => Ok(snapshot),
        Response::Error(error) => bail!(error),
        _ => bail!("工作区服务未返回用量信息"),
    }
}

/// Preserve successful account evidence only for the same account after a provider failure.
pub fn retain_account(mut snapshot: Snapshot, previous: Option<&Snapshot>) -> Snapshot {
    if snapshot.account.error.is_some()
        && let Some(old) = previous
        && old.account.identity == snapshot.account.identity
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
}

impl ContextUsage {
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
        match event {
            AgentEvent::ContextUsage { estimated_input } => Some(
                self.calibration(model)
                    .estimate(*estimated_input),
            ),
            AgentEvent::RequestMetrics {
                profile,
                estimated_input,
                usage,
                ..
            } => {
                let reported_input = usage
                    .as_ref()
                    .and_then(|usage| usage.prompt_tokens);
                self.calibrations
                    .entry(profile.clone())
                    .or_default()
                    .observe(*estimated_input, reported_input);
                Some(
                    reported_input
                        .and_then(|tokens| usize::try_from(tokens).ok())
                        .filter(|tokens| *tokens > 0)
                        .unwrap_or_else(|| {
                            self.calibration(model)
                                .estimate(*estimated_input)
                        }),
                )
            }
            AgentEvent::ContextCompacted { after_tokens, .. } => Some(
                self.calibration(model)
                    .estimate(*after_tokens),
            ),
            _ => None,
        }
    }
}

/// Input occupancy uses the independently resolved input budget, never total output capacity.
pub fn context_percent(used: usize, input_budget: usize) -> usize {
    used.saturating_mul(100) / input_budget.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(
            reset_timestamp("2027-01-15T08:00:00+08:00"),
            reset_timestamp("2027-01-15T00:00:00Z")
        );
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
        failed.account.identity = "source-b".into();
        assert!(
            retain_account(failed, Some(&old))
                .account
                .limits
                .is_empty()
        );
    }
}
