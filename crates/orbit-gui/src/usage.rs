use gpui_kit::component::{ActiveTheme, Sizable, h_flex, progress::Progress, v_flex};
use gpui_kit::prelude::FluentBuilder;
use gpui_kit::*;
use orbit_protocol::usage::{Scope, Snapshot};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) struct Usage {
    account: SharedString,
    rows: Vec<Quota>,
    note: Option<SharedString>,
    totals: SharedString,
    fetched_at: Option<u64>,
    _clock: Task<()>,
}
struct Quota {
    id: SharedString,
    label: SharedString,
    short: SharedString,
    percent: Option<f32>,
    amount: SharedString,
    reset: Option<i64>,
    shared: bool,
}
impl Usage {
    pub(crate) fn new(cx: &mut Context<Self>) -> Self {
        let clock = cx.spawn(async move |weak, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                if weak.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });
        Self {
            account: "账号额度".into(),
            rows: Vec::new(),
            note: None,
            totals: "".into(),
            fetched_at: None,
            _clock: clock,
        }
    }

    pub(crate) fn set(&mut self, snapshot: Snapshot, cx: &mut Context<Self>) {
        self.rows = quota_rows(&snapshot);
        let account = snapshot.account;
        self.account = format!("{} · {}", account.provider, account.name).into();
        self.fetched_at = account.fetched_at;
        self.note = account.error.or(account.note).map(Into::into);
        self.totals = format!(
            "会话输入 {} · 输出 {}",
            snapshot
                .session
                .input
                .map(|v| v.to_string())
                .unwrap_or_else(|| "—".into()),
            snapshot
                .session
                .output
                .map(|v| v.to_string())
                .unwrap_or_else(|| "—".into())
        )
        .into();
        cx.notify();
    }

    pub(crate) fn error(&mut self, error: String, cx: &mut Context<Self>) {
        self.note = Some(error.into());
        cx.notify();
    }

    pub(crate) fn summary(&self, cx: &App) -> AnyElement {
        let limits = self
            .rows
            .iter()
            .filter(|row| row.shared)
            .take(2)
            .collect::<Vec<_>>();
        if limits.is_empty() {
            return div().child("用量…").into_any_element();
        }
        h_flex()
            .gap_4()
            .children(limits.into_iter().map(|row| {
                h_flex()
                    .gap_2()
                    .child(row.short.clone())
                    .when_some(row.percent, |view, percent| {
                        view.child(
                            div()
                                .w_16()
                                .child(Progress::new(row.id.clone()).small().value(percent)),
                        )
                        .child(format!("{percent:.0}%"))
                    })
                    .when_some(
                        row.reset.and_then(|at| remaining(at, now())),
                        |view, text| {
                            view.child(
                                div()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("（{text}）")),
                            )
                        },
                    )
            }))
            .into_any_element()
    }
}
impl Render for Usage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .w_full()
            .min_w_0()
            .gap_4()
            .child(self.account.clone())
            .when_some(self.fetched_at, |view, fetched| {
                view.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!(
                            "更新于 {} 秒前",
                            now().saturating_sub(fetched as i64)
                        )),
                )
            })
            .when(self.rows.is_empty(), |view| {
                view.child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child("暂无共享额度数据"),
                )
            })
            .children(self.rows.iter().map(|row| {
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_3()
                    .child(
                        div()
                            .w(rems(14.))
                            .min_w_0()
                            .text_ellipsis()
                            .child(row.label.clone()),
                    )
                    .when_some(row.percent, |view, percent| {
                        view.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(Progress::new(row.id.clone()).value(percent)),
                        )
                        .child(
                            div()
                                .w_12()
                                .flex_shrink_0()
                                .whitespace_nowrap()
                                .text_right()
                                .child(format!("{percent:.0}%")),
                        )
                    })
                    .when(row.percent.is_none(), |view| {
                        view.child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_ellipsis()
                                .text_right()
                                .child(row.amount.clone()),
                        )
                    })
                    .when_some(
                        row.reset.and_then(|at| remaining(at, now())),
                        |view, text| {
                            view.child(
                                div()
                                    .w(rems(9.))
                                    .flex_shrink_0()
                                    .whitespace_nowrap()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("{text}后重置")),
                            )
                        },
                    )
            }))
            .when_some(self.note.clone(), |view, note| view.child(note))
            .child(
                div()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(self.totals.clone()),
            )
    }
}
fn quota_rows(snapshot: &Snapshot) -> Vec<Quota> {
    let selected = snapshot.summary_limits().collect::<Vec<_>>();
    snapshot
        .account
        .limits
        .iter()
        .filter(|limit| limit.scope == Scope::Account)
        .map(|limit| {
            let short = if limit.label.to_ascii_lowercase().contains("week") {
                "周"
            } else if limit.label.to_ascii_lowercase().contains("five")
                || limit.label.contains("5h")
            {
                "5h"
            } else {
                "额度"
            };
            let label = quota_label(&limit.label);
            Quota {
                id: format!("{}:{}", snapshot.account.identity, limit.label).into(),
                label: label.into(),
                short: short.into(),
                percent: limit
                    .remaining_fraction
                    .filter(|v| v.is_finite())
                    .map(|v| (v.clamp(0., 1.) * 100.) as f32),
                amount: limit.remaining.clone().unwrap_or_else(|| "—".into()).into(),
                reset: limit.resets_at.as_deref().and_then(reset_timestamp),
                shared: selected
                    .iter()
                    .any(|selected| std::ptr::eq(*selected, limit)),
            }
        })
        .collect()
}
fn quota_label(label: &str) -> String {
    let mut parts = label
        .split('·')
        .map(|part| match part.trim().to_ascii_lowercase().as_str() {
            "weekly limit remaining" | "weekly" | "week" => "周".to_owned(),
            "five hour limit remaining" | "five_hour" | "5h" => "5h".to_owned(),
            _ => part
                .trim()
                .replace("Gemini Models", "Gemini")
                .replace("Claude and GPT models", "Claude / GPT"),
        })
        .collect::<Vec<_>>();
    parts.dedup();
    parts.join(" · ")
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}
fn reset_timestamp(value: &str) -> Option<i64> {
    value.parse().ok().or_else(|| {
        chrono::DateTime::parse_from_rfc3339(value)
            .ok()
            .map(|date| date.timestamp())
    })
}
fn remaining(at: i64, now: i64) -> Option<String> {
    let seconds = at.checked_sub(now)?;
    if seconds <= 0 {
        return None;
    }
    let minutes = (seconds + 59) / 60;
    Some(if minutes >= 1440 {
        format!("{}天{}时", minutes / 1440, minutes % 1440 / 60)
    } else if minutes >= 60 {
        format!("{}时{}分", minutes / 60, minutes % 60)
    } else {
        format!("{minutes}分")
    })
}
#[cfg(test)]
mod tests {
    use super::{quota_label, quota_rows, remaining, reset_timestamp};
    use orbit_protocol::usage::{Account, Limit, Scope, Snapshot};

    #[test]
    fn account_rows_hide_model_counters_and_follow_the_active_pool() {
        let mut snapshot = Snapshot {
            account: Account {
                provider: "Antigravity".into(),
                limits: [
                    (
                        "Gemini Models · Weekly Limit Remaining · weekly",
                        0.89,
                        Scope::Account,
                    ),
                    ("模型 · chat_20706", 1., Scope::Model),
                    (
                        "Gemini Models · Five Hour Limit Remaining · 5h",
                        0.73,
                        Scope::Account,
                    ),
                    (
                        "Claude and GPT models · Weekly Limit Remaining · weekly",
                        1.,
                        Scope::Account,
                    ),
                    (
                        "Claude and GPT models · Five Hour Limit Remaining · 5h",
                        0.5,
                        Scope::Account,
                    ),
                    ("模型 · Gemini Flash", 1., Scope::Model),
                ]
                .into_iter()
                .map(|(label, fraction, scope)| Limit {
                    label: label.into(),
                    scope,
                    remaining_fraction: Some(fraction),
                    ..Default::default()
                })
                .collect(),
                ..Default::default()
            },
            ..Default::default()
        };
        for (model, expected) in [
            ("account::gemini-flash", vec![89., 73.]),
            ("account::claude-opus", vec![100., 50.]),
            ("account::gpt-oss", vec![100., 50.]),
        ] {
            snapshot.model = model.into();
            let rows = quota_rows(&snapshot);
            assert_eq!(
                rows.iter()
                    .map(|row| row.label.as_ref())
                    .collect::<Vec<_>>(),
                [
                    "Gemini · 周",
                    "Gemini · 5h",
                    "Claude / GPT · 周",
                    "Claude / GPT · 5h"
                ]
            );
            assert_eq!(
                rows.iter()
                    .filter(|row| row.shared)
                    .map(|row| row.percent.unwrap())
                    .collect::<Vec<_>>(),
                expected
            );
        }
        snapshot
            .account
            .limits
            .retain(|limit| limit.scope == Scope::Model);
        assert!(
            quota_rows(&snapshot).is_empty(),
            "model counters must never substitute for shared quota"
        );
    }
    #[test]
    fn resets_use_server_time_and_omit_absent_or_elapsed_windows() {
        assert_eq!(reset_timestamp("2026-10-11T00:00:00Z"), Some(1791676800));
        assert_eq!(reset_timestamp("unknown"), None);
        assert_eq!(remaining(3601, 0).as_deref(), Some("1时1分"));
        assert_eq!(remaining(42, 42), None);
        assert_eq!(remaining(1, 42), None);
    }

    #[test]
    fn quota_labels_keep_distinct_details_and_accept_already_compact_windows() {
        assert_eq!(quota_label("Gemini · 周"), "Gemini · 周");
        assert_eq!(quota_label("Claude · weekly"), "Claude · 周");
        assert_eq!(quota_label("请求额度 · daily"), "请求额度 · daily");
        assert_eq!(quota_label("总余额"), "总余额");
    }
}
