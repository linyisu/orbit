use super::dialogs::{InfoDialog, ReportPage, ReportSource};
use orbit_protocol::usage::{Account, Snapshot};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CACHE_AGE: Duration = Duration::from_secs(60);
const BAR_WIDTH: usize = 16;

#[derive(Debug, Default)]
pub(super) struct State {
    pub snapshot: Option<Snapshot>,
    pub loading: bool,
    pub error: Option<String>,
    refreshed: Option<Instant>,
}

pub(super) fn is_open(dialog: &Option<InfoDialog>) -> bool {
    matches!(
        dialog.as_ref().and_then(|dialog| dialog.source.as_ref()),
        Some(ReportSource::Usage)
    )
}

impl State {
    pub fn fresh(&self) -> bool {
        self.error.is_none()
            && self
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.account.error.is_none())
            && self
                .refreshed
                .is_some_and(|time| time.elapsed() < CACHE_AGE)
    }
    pub fn receive(&mut self, mut snapshot: Snapshot) {
        if snapshot.account.error.is_some()
            && let Some(old) = &self.snapshot
            && old.account.identity == snapshot.account.identity
            && (snapshot.account.provider.is_empty()
                || old.account.provider == snapshot.account.provider)
        {
            let error = snapshot.account.error.take();
            snapshot.account = old.account.clone();
            snapshot.account.error = error;
        }
        self.snapshot = Some(snapshot);
        self.refreshed = Some(Instant::now());
        self.loading = false;
        self.error = None;
    }
    pub fn update_dialog(&self, dialog: &mut InfoDialog) {
        let status = if self.loading { " · 刷新中" } else { "" };
        let mut account_page = String::new();
        let mut session_page = String::new();
        if let Some(snapshot) = &self.snapshot {
            let identity = [&snapshot.account.provider, &snapshot.account.name]
                .into_iter()
                .filter(|part| !part.is_empty())
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(" · ");
            dialog.title = format!("用量 · {identity}{status}");
            account_page = account_content(&snapshot.account);
            let session = &snapshot.session;
            let amount = |value: Option<u64>| {
                value
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "未提供".into())
            };
            session_page = format!(
                "当前主会话 · {}\n\n运行次数      {}\n模型请求      {}\n输入 Token    {}\n输出 Token    {}\n缓存读取      {}\n缓存写入      {}\n\n用量记录覆盖 {} / {} 次运行\n仅汇总服务器已返回的字段；缺失字段不视为零。\n子代理及旁支问答不计入本页。",
                snapshot.model,
                session.runs,
                session.requests,
                amount(session.input),
                amount(session.output),
                amount(session.cached_input),
                amount(session.cache_write),
                session.runs_with_usage,
                session.runs
            );
            if let Some(error) = &session.error {
                session_page.push_str(&format!("\n\n{error}"));
            }
        } else {
            dialog.title = format!("用量{status}");
        }
        if let Some(error) = &self.error {
            account_page.insert_str(0, &format!("{error}\n\n"));
        }
        if self.loading {
            account_page.insert_str(
                0,
                if self.snapshot.is_some() {
                    "正在刷新，仍可浏览上次结果。\n\n"
                } else {
                    "正在查询当前账号…\n\n"
                },
            );
            if session_page.is_empty() {
                session_page = "正在读取会话用量…".into();
            }
        }
        dialog.pages(vec![
            ReportPage {
                label: "账号额度".into(),
                content: account_page,
            },
            ReportPage {
                label: "会话用量".into(),
                content: session_page,
            },
        ]);
    }
    pub fn summary(&self) -> Option<String> {
        let account = &self.snapshot.as_ref()?.account;
        let limit = account.limits.first()?;
        let stale = if account.error.is_some() || self.error.is_some() {
            " · 旧数据"
        } else {
            ""
        };
        let value = if let Some(fraction) = limit.remaining_fraction {
            let mut values = vec![format!("{} 剩余{:.0}%", limit.label, fraction * 100.0)];
            if let Some(second) = account.limits.get(1)
                && let Some(fraction) = second.remaining_fraction
            {
                values.push(format!("{} {:.0}%", second.label, fraction * 100.0));
            }
            values.join(" · ")
        } else {
            format!(
                "{} {} {}",
                limit.label,
                limit.remaining.as_deref()?,
                limit.unit.as_deref().unwrap_or("")
            )
        };
        Some(format!("{value}{stale} · /usage"))
    }
}

fn account_content(account: &Account) -> String {
    let mut rows = Vec::new();
    if let Some(error) = &account.error {
        rows.push(format!("刷新失败：{error}"));
        if account.fetched_at.is_some() {
            rows.push("以下为上次成功查询的结果。".into());
        }
        rows.push(String::new());
    }
    if let Some(plan) = &account.plan {
        rows.push(format!("套餐：{plan}"));
    }
    if let Some(time) = account.fetched_at {
        let age = now().saturating_sub(time);
        rows.push(format!("更新时间：{age} 秒前"));
    }
    if let Some(note) = &account.note {
        rows.push(note.clone());
    }
    rows.push(String::new());
    for limit in &account.limits {
        rows.push(limit.label.clone());
        let unit = limit.unit.as_deref().unwrap_or("");
        if let Some(fraction) = limit.remaining_fraction {
            let filled = (fraction * BAR_WIDTH as f64).round() as usize;
            rows.push(format!(
                "  [{}{}]  剩余 {:.0}%",
                "=".repeat(filled.min(BAR_WIDTH)),
                "-".repeat(BAR_WIDTH.saturating_sub(filled)),
                fraction * 100.0
            ));
        }
        if let Some(value) = &limit.remaining {
            rows.push(format!("  剩余 {value} {unit}"));
        }
        if let Some(value) = &limit.used {
            rows.push(format!("  已用 {value} {unit}"));
        }
        if let Some(value) = &limit.total {
            rows.push(format!("  总量 {value} {unit}"));
        }
        if limit.remaining_fraction.is_none() && limit.remaining.is_none() && limit.used.is_none() {
            rows.push("  服务器未提供剩余额度".into());
        }
        if let Some(reset) = &limit.resets_at {
            rows.push(format!("  重置：{}", reset_label(reset)));
        }
        rows.push(String::new());
    }
    rows.join("\n")
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
fn reset_label(reset: &str) -> String {
    let Ok(time) = reset.parse::<u64>() else {
        return reset.into();
    };
    let seconds = time.saturating_sub(now());
    if seconds == 0 {
        "等待服务器更新".into()
    } else if seconds >= 86400 {
        format!("{}天{}小时后", seconds / 86400, seconds % 86400 / 3600)
    } else {
        format!("{}小时{}分钟后", seconds / 3600, seconds % 3600 / 60)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_protocol::usage::Limit;
    #[test]
    fn failed_refresh_preserves_previous_evidence_and_marks_it_stale() {
        let mut state = State::default();
        let mut snapshot = Snapshot {
            account: Account {
                identity: "source-a".into(),
                provider: "Gemini".into(),
                name: "main".into(),
                fetched_at: Some(now()),
                limits: vec![Limit {
                    label: "Pro".into(),
                    remaining_fraction: Some(0.4),
                    ..Default::default()
                }],
                ..Default::default()
            },
            ..Default::default()
        };
        state.receive(snapshot.clone());
        snapshot.account.limits.clear();
        snapshot.account.fetched_at = None;
        snapshot.account.error = Some("网络错误".into());
        state.receive(snapshot);
        assert!(state.summary().unwrap().contains("40% · 旧数据"));
        assert!(account_content(&state.snapshot.as_ref().unwrap().account).contains("上次成功"));
        let mut other = Snapshot::default();
        other.account.identity = "source-b".into();
        other.account.error = Some("另一个账号不可用".into());
        state.receive(other);
        assert!(state.summary().is_none());
    }
}
