use anyhow::{Result, ensure};
use orbit_auth::UsageProvider;
use orbit_protocol::usage::{Account, Limit, Scope};
use serde_json::Value;

const MAX_LIMITS: usize = 128;

fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|n| n.is_finite() && *n >= 0.0)
}
fn text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}
fn fraction(value: &Value) -> Option<f64> {
    number(value).map(|n| n.clamp(0.0, 1.0))
}
fn remaining_percent(value: &Value) -> Option<f64> {
    number(value).map(|n| (1.0 - n / 100.0).clamp(0.0, 1.0))
}
fn duration(seconds: u64) -> String {
    if seconds > 0 && seconds.is_multiple_of(604800) {
        format!("{}周", seconds / 604800)
    } else if seconds > 0 && seconds.is_multiple_of(86400) {
        format!("{}天", seconds / 86400)
    } else if seconds > 0 && seconds.is_multiple_of(3600) {
        format!("{}小时", seconds / 3600)
    } else {
        format!("{}分钟", seconds / 60)
    }
}

pub(super) fn parse(provider: UsageProvider, payload: &Value, account: &mut Account) -> Result<()> {
    match provider {
        UsageProvider::Chatgpt => {
            account.plan = text(&payload["plan_type"]);
            chatgpt_windows(&payload["rate_limit"], "", &mut account.limits);
            if let Some(additional) = payload["additional_rate_limits"].as_array() {
                for entry in additional {
                    let label = text(&entry["limit_name"])
                        .or_else(|| text(&entry["metered_feature"]))
                        .unwrap_or_else(|| "附加额度".into());
                    chatgpt_windows(
                        &entry["rate_limit"],
                        &format!("{label} · "),
                        &mut account.limits,
                    );
                }
            }
            let credits = &payload["credits"];
            if credits["unlimited"].as_bool() == Some(true) {
                account.limits.push(Limit {
                    label: "附加 Credits".into(),
                    remaining: Some("不限量".into()),
                    ..Default::default()
                });
            } else if let Some(balance) = text(&credits["balance"]) {
                account.limits.push(Limit {
                    label: "附加 Credits".into(),
                    remaining: Some(balance),
                    unit: Some("credits".into()),
                    ..Default::default()
                });
            }
        }
        UsageProvider::Claude => {
            for (key, label) in [
                ("five_hour", "5小时"),
                ("seven_day", "每周"),
                ("seven_day_opus", "Opus · 每周"),
                ("seven_day_sonnet", "Sonnet · 每周"),
            ] {
                let bucket = &payload[key];
                if bucket.is_object() {
                    account.limits.push(Limit {
                        label: label.into(),
                        scope: if key.ends_with("_opus") || key.ends_with("_sonnet") {
                            Scope::Model
                        } else {
                            Scope::Account
                        },
                        remaining_fraction: remaining_percent(&bucket["utilization"]),
                        resets_at: text(&bucket["resets_at"]),
                        ..Default::default()
                    });
                }
            }
            if let Some(limits) = payload["limits"].as_array() {
                for entry in limits {
                    // is_active marks the currently binding cap, not whether
                    // the quota exists. Keep inactive but reported windows.
                    let kind = entry["kind"].as_str().unwrap_or("额度");
                    // Legacy shared windows and the new representation describe
                    // the same counters. Do not show them twice.
                    if (kind == "session" && payload["five_hour"].is_object())
                        || (kind == "weekly_all" && payload["seven_day"].is_object())
                    {
                        continue;
                    }
                    let label = match kind {
                        "session" => "5小时",
                        "weekly_all" => "每周",
                        other => other,
                    };
                    let model = text(&entry["scope"]["model"]["display_name"]);
                    account.limits.push(Limit {
                        scope: if model.is_some() {
                            Scope::Model
                        } else {
                            Scope::Account
                        },
                        label: model
                            .map(|m| format!("{m} · {label}"))
                            .unwrap_or_else(|| label.into()),
                        remaining_fraction: remaining_percent(&entry["percent"]),
                        resets_at: text(&entry["resets_at"]),
                        ..Default::default()
                    });
                }
            }
            let extra = &payload["extra_usage"];
            if extra["is_enabled"].as_bool() == Some(true) {
                account.limits.push(Limit {
                    label: "额外用量".into(),
                    used: text(&extra["used_credits"]),
                    total: text(&extra["monthly_limit"]),
                    unit: Some("credits".into()),
                    ..Default::default()
                });
            }
        }
        UsageProvider::Gemini => {
            if let Some(buckets) = payload["buckets"].as_array() {
                for bucket in buckets {
                    let scope = if text(&bucket["modelId"]).is_some() {
                        Scope::Model
                    } else {
                        Scope::Account
                    };
                    let model = text(&bucket["modelId"]).unwrap_or_else(|| "共享额度".into());
                    let kind = text(&bucket["tokenType"]);
                    account.limits.push(Limit {
                        scope,
                        label: kind
                            .map(|kind| format!("{model} · {kind}"))
                            .unwrap_or(model),
                        remaining_fraction: fraction(&bucket["remainingFraction"]),
                        remaining: text(&bucket["remainingAmount"]),
                        resets_at: text(&bucket["resetTime"]),
                        ..Default::default()
                    });
                }
            }
        }
        UsageProvider::Antigravity => {
            let summary = payload.get("summary").unwrap_or(payload);
            antigravity_buckets(summary, "", &mut account.limits);
            if let Some(groups) = summary["groups"].as_array() {
                for group in groups {
                    let name = text(&group["displayName"]).unwrap_or_else(|| "共享额度".into());
                    antigravity_buckets(group, &format!("{name} · "), &mut account.limits);
                }
            }
            if account.limits.is_empty()
                && let Some(models) = payload["models"].as_object()
            {
                for (name, model) in models {
                    let quota = &model["quotaInfo"];
                    if !quota.is_object() {
                        continue;
                    }
                    account.limits.push(Limit {
                        scope: Scope::Model,
                        label: format!(
                            "模型 · {}",
                            text(&model["displayName"]).unwrap_or_else(|| name.clone())
                        ),
                        remaining_fraction: fraction(&quota["remainingFraction"]),
                        remaining: text(&quota["remainingAmount"]),
                        resets_at: text(&quota["resetTime"]),
                        ..Default::default()
                    });
                }
            }
        }
        UsageProvider::Deepseek => {
            if let Some(balances) = payload["balance_infos"].as_array() {
                for balance in balances {
                    let unit = text(&balance["currency"]);
                    for (key, label) in [
                        ("total_balance", "总余额"),
                        ("granted_balance", "赠送余额"),
                        ("topped_up_balance", "充值余额"),
                    ] {
                        if let Some(remaining) = text(&balance[key]) {
                            account.limits.push(Limit {
                                label: label.into(),
                                remaining: Some(remaining),
                                unit: unit.clone(),
                                ..Default::default()
                            });
                        }
                    }
                }
            }
        }
        UsageProvider::Zhipu => {
            ensure!(
                payload["success"].as_bool() == Some(true),
                "智谱额度查询未成功，请稍后刷新"
            );
            account.plan = text(&payload["data"]["level"]);
            if let Some(limits) = payload["data"]["limits"].as_array() {
                for limit in limits {
                    let (label, unit) = match limit["type"].as_str() {
                        Some("TOKENS_LIMIT") => ("Token 额度", "tokens"),
                        Some("TIME_LIMIT") => ("请求额度", "次"),
                        Some("CREDIT_LIMIT") => ("Credits 额度", "credits"),
                        _ => continue,
                    };
                    let count = text(&limit["number"]);
                    let window = match (limit["unit"].as_u64(), count) {
                        (Some(3), Some(n)) => format!("{n}小时"),
                        (Some(4), Some(n)) => format!("{n}天"),
                        (Some(5), Some(n)) => format!("{n}月"),
                        (Some(6), _) => "每周".into(),
                        _ => "".into(),
                    };
                    let remaining_fraction =
                        match (number(&limit["currentValue"]), number(&limit["usage"])) {
                            (Some(used), Some(total)) if total > 0.0 => {
                                Some((1.0 - used / total).clamp(0.0, 1.0))
                            }
                            _ => remaining_percent(&limit["percentage"]),
                        };
                    let reset = number(&limit["nextResetTime"]).map(|n| {
                        if n > 1e12 {
                            (n / 1000.0).to_string()
                        } else {
                            n.to_string()
                        }
                    });
                    account.limits.push(Limit {
                        label: format!("{label} {window}").trim().into(),
                        remaining_fraction,
                        remaining: text(&limit["remaining"]),
                        used: text(&limit["currentValue"]),
                        total: text(&limit["usage"]),
                        unit: Some(unit.into()),
                        resets_at: reset,
                        ..Default::default()
                    });
                }
            }
        }
    }
    ensure!(!account.limits.is_empty(), "服务器未返回可识别的额度信息");
    account.limits.truncate(MAX_LIMITS);
    Ok(())
}

fn chatgpt_windows(rate: &Value, prefix: &str, limits: &mut Vec<Limit>) {
    for (key, fallback) in [
        ("primary_window", "主要窗口"),
        ("secondary_window", "次要窗口"),
    ] {
        let window = &rate[key];
        if !window.is_object() {
            continue;
        }
        let label = window["limit_window_seconds"]
            .as_u64()
            .map(duration)
            .unwrap_or_else(|| fallback.into());
        limits.push(Limit {
            label: format!("{prefix}{label}"),
            remaining_fraction: remaining_percent(&window["used_percent"]),
            resets_at: text(&window["reset_at"]),
            ..Default::default()
        });
    }
}

fn antigravity_buckets(value: &Value, prefix: &str, limits: &mut Vec<Limit>) {
    if let Some(buckets) = value["buckets"].as_array() {
        for bucket in buckets {
            if bucket["disabled"].as_bool() == Some(true) {
                continue;
            }
            let name = text(&bucket["displayName"])
                .or_else(|| text(&bucket["bucketId"]))
                .unwrap_or_else(|| "额度".into());
            let window = text(&bucket["window"])
                .map(|w| format!(" · {w}"))
                .unwrap_or_default();
            limits.push(Limit {
                label: format!("{prefix}{name}{window}"),
                remaining_fraction: fraction(&bucket["remainingFraction"]),
                remaining: text(&bucket["remainingAmount"]),
                resets_at: text(&bucket["resetTime"]),
                ..Default::default()
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn parse_fixture(provider: UsageProvider, value: Value) -> Account {
        let mut account = Account::default();
        parse(provider, &value, &mut account).unwrap();
        account
    }
    #[test]
    fn chatgpt_keeps_windows_and_money_separate() {
        let account = parse_fixture(
            UsageProvider::Chatgpt,
            json!({"plan_type":"plus", "rate_limit":{"primary_window":{"used_percent":32,"limit_window_seconds":18000,"reset_at":1800000000},"secondary_window":{"used_percent":100,"limit_window_seconds":604800}},"credits":{"balance":"3.50"}}),
        );
        assert_eq!(account.plan.as_deref(), Some("plus"));
        assert_eq!(account.limits[0].label, "5小时");
        assert_eq!(account.limits[1].remaining_fraction, Some(0.0));
        assert!(account.limits[2].remaining_fraction.is_none());
    }
    #[test]
    fn google_missing_is_not_exhausted_and_models_are_not_combined() {
        let account = parse_fixture(
            UsageProvider::Gemini,
            json!({"buckets":[{"modelId":"gemini-pro","remainingFraction":0.0},{"modelId":"gemini-flash","remainingAmount":"300"}]}),
        );
        assert_eq!(account.limits[0].remaining_fraction, Some(0.0));
        assert_eq!(account.limits[1].remaining_fraction, None);
        assert_eq!(account.limits[1].remaining.as_deref(), Some("300"));
        assert!(
            account
                .limits
                .iter()
                .all(|limit| limit.scope == Scope::Model)
        );
        let account = parse_fixture(
            UsageProvider::Antigravity,
            json!({"groups":[{"displayName":"Claude","buckets":[{"bucketId":"weekly","remainingFraction":0.6,"resetTime":"2026-10-11T00:00:00Z"}]}]}),
        );
        assert_eq!(account.limits[0].label, "Claude · weekly");
    }
    #[test]
    fn antigravity_shared_quota_takes_precedence_over_model_catalog() {
        let models = json!({"gemini-pro":{"displayName":"Gemini Pro","quotaInfo":{"remainingFraction":0.6}}});
        let shared = parse_fixture(
            UsageProvider::Antigravity,
            json!({
                "summary":{"groups":[{"displayName":"Gemini Models","buckets":[{"displayName":"Weekly Limit Remaining","window":"weekly","remainingFraction":0.59}]}]},
                "models":models
            }),
        );
        assert_eq!(shared.limits.len(), 1);
        assert_eq!(shared.limits[0].scope, Scope::Account);
        assert_eq!(shared.limits[0].remaining_fraction, Some(0.59));
        let fallback = parse_fixture(
            UsageProvider::Antigravity,
            json!({"summary":null,"models":models}),
        );
        assert_eq!(fallback.limits.len(), 1);
        assert_eq!(fallback.limits[0].scope, Scope::Model);
        assert_eq!(fallback.limits[0].remaining_fraction, Some(0.6));
    }
    #[test]
    fn money_preserves_precision_and_currency() {
        let account = parse_fixture(
            UsageProvider::Deepseek,
            json!({"balance_infos":[{"currency":"CNY","total_balance":"0.000123","granted_balance":"0","topped_up_balance":"0.000123"}]}),
        );
        assert_eq!(account.limits[0].remaining.as_deref(), Some("0.000123"));
        assert_eq!(account.limits[0].unit.as_deref(), Some("CNY"));
        assert!(account.limits[0].remaining_fraction.is_none());
    }
    #[test]
    fn zhipu_prefers_exact_ratio_and_preserves_units() {
        let account = parse_fixture(
            UsageProvider::Zhipu,
            json!({"success":true,"data":{"limits":[{"type":"CREDIT_LIMIT","usage":12000,"currentValue":1438,"percentage":11,"unit":3,"number":5,"nextResetTime":1800000000000u64}]}}),
        );
        assert_eq!(account.limits[0].label, "Credits 额度 5小时");
        assert!(
            (account.limits[0].remaining_fraction.unwrap() - (1.0 - 1438.0 / 12000.0)).abs() < 1e-9
        );
        assert_eq!(account.limits[0].resets_at.as_deref(), Some("1800000000"));
    }
    #[test]
    fn claude_does_not_duplicate_shared_windows() {
        let account = parse_fixture(
            UsageProvider::Claude,
            json!({"five_hour":{"utilization":20},"limits":[{"kind":"session","percent":20},{"kind":"weekly_model","percent":80,"scope":{"model":{"display_name":"Opus"}}}]}),
        );
        assert_eq!(account.limits.len(), 2);
        assert_eq!(account.limits[1].label, "Opus · weekly_model");
        assert!(parse(UsageProvider::Gemini, &json!({}), &mut Account::default()).is_err());
    }
}
