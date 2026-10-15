use super::dialogs::{InfoDialog, ReportPage, ReportSource};
use orbit_protocol::usage::{Account, Limit, Scope, Snapshot};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Gauge, Widget},
};
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use unicode_width::UnicodeWidthStr;

use orbit_client::usage::{CACHE_AGE, REFRESH_INTERVAL};
const BAR_WIDTH: usize = 20;
const MAX_QUOTA_LABEL_WIDTH: usize = 24;

#[derive(Debug, Default)]
pub(super) struct State {
    pub snapshot: Option<Snapshot>,
    pub loading: bool,
    pub error: Option<String>,
    refreshed: Option<Instant>,
    requested: Option<Instant>,
    display_second: u64,
}

pub(super) fn is_open(dialog: &Option<InfoDialog>) -> bool {
    matches!(
        dialog
            .as_ref()
            .and_then(|dialog| dialog.source.as_ref()),
        Some(ReportSource::Usage)
    )
}

impl State {
    pub fn refresh_due(&self) -> bool {
        self.requested
            .is_none_or(|time| time.elapsed() >= REFRESH_INTERVAL)
    }

    pub fn begin_refresh(&mut self) {
        self.requested = Some(Instant::now());
        self.loading = true;
        self.error = None;
    }

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

    pub fn receive(&mut self, snapshot: Snapshot) {
        self.snapshot = Some(orbit_client::usage::retain_account(
            snapshot,
            self.snapshot.as_ref(),
        ));
        self.refreshed = Some(Instant::now());
        self.requested = self.refreshed;
        self.loading = false;
        self.error = None;
        self.display_second = now();
    }

    pub fn tick(&mut self, dialog_open: bool) -> bool {
        let second = now();
        // The dialog shows seconds since the fetch; the footer only shows
        // minute-resolution reset times and needs no per-second redraw.
        let changed = self.snapshot.is_some()
            && if dialog_open {
                self.display_second != second
            } else {
                self.display_second / 60 != second / 60
            };
        self.display_second = second;
        changed
    }

    pub fn update_dialog(&self, dialog: &mut InfoDialog) {
        let status = if self.loading { " · 刷新中" } else { "" };
        let mut account_page = Text::default();
        let mut session_page = String::new();
        if let Some(snapshot) = &self.snapshot {
            let identity = [&snapshot.account.provider, &snapshot.account.name]
                .into_iter()
                .filter(|part| !part.is_empty())
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(" · ");
            dialog.title = format!("用量 · {identity}{status}");
            account_page = account_content(&snapshot.account, Scope::Account);
            let session = &snapshot.session;
            let amount = |value: Option<u64>| {
                value
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "未提供".into())
            };
            session_page = format!(
                "当前主会话 · {}\n\n运行次数      {}\n模型请求      {}\n输入 Token    {}\n输出 Token    {}\n缓存读取      \
                    {}\n缓存写入      {}\n\n用量记录覆盖 {} / {} 次运行\n仅汇总服务器已返回的字段；缺失字段不视为零。\n子代理及旁支问答不计入本页。",
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
            account_page
                .lines
                .splice(0..0, Text::from(format!("{error}\n\n")).lines);
        }
        if self.loading {
            if self.snapshot.is_none() {
                account_page
                    .lines
                    .splice(0..0, Text::from("正在查询当前账号…\n\n").lines);
            }
            if session_page.is_empty() {
                session_page = "正在读取会话用量…".into();
            }
        }
        let mut pages = vec![
            ReportPage {
                label: "账号额度".into(),
                content: account_page,
            },
            ReportPage {
                label: "会话用量".into(),
                content: session_page.into(),
            },
        ];
        if let Some(snapshot) = &self.snapshot
            && snapshot
                .account
                .limits
                .iter()
                .any(|limit| limit.scope == Scope::Model)
        {
            pages.push(ReportPage {
                label: "模型额度".into(),
                content: account_content(&snapshot.account, Scope::Model),
            });
        }
        dialog.pages(pages);
    }

    pub fn indicator(&self, max_width: usize) -> Line<'static> {
        let fallback = || Line::from(Span::styled("用量", super::components::accent()));
        let Some(snapshot) = &self.snapshot else {
            return fallback();
        };
        let limits = orbit_client::usage::summary_limits(snapshot)
            .into_iter()
            .take(2)
            .collect::<Vec<_>>();
        if limits.is_empty() {
            return fallback();
        }
        let time = now();
        for width in [12, 8] {
            let mut line = Line::default();
            for (index, limit) in limits.iter().enumerate() {
                if index > 0 {
                    line.spans.push(Span::raw("  "));
                }
                line.spans
                    .extend(footer_limit(limit, width, time).spans);
            }
            if snapshot.account.error.is_some() || self.error.is_some() {
                line.spans
                    .push(Span::styled(" · 旧数据", super::components::muted()));
            }
            if line.width() <= max_width {
                return line;
            }
        }
        fallback()
    }
}

fn footer_limit(limit: &Limit, width: usize, time: u64) -> Line<'static> {
    let mut line = Line::from(vec![Span::styled(
        format!("{} ", orbit_client::usage::summary_label(limit)),
        super::components::accent(),
    )]);
    if let Some(fraction) = limit.remaining_fraction {
        line.spans
            .extend(quota_bar(fraction, width).spans);
    } else {
        line.spans
            .push(Span::raw(super::render::safe_output(
                &orbit_client::usage::amount(limit),
            )));
    }
    if let Some(reset) = limit
        .resets_at
        .as_deref()
        .and_then(|reset| reset_countdown(reset, time))
    {
        let reset = compact_reset(&reset);
        line.spans.push(Span::styled(
            format!(" ({})", reset.trim_end_matches("后重置")),
            super::components::muted(),
        ));
    }
    line
}

fn compact_reset(reset: &str) -> String {
    reset
        .replace("小时", "时")
        .replace("分钟", "分")
}

use orbit_client::usage::quota_label;

fn quota_bar(fraction: f64, width: usize) -> Line<'static> {
    use super::theme::THEME;
    let fraction = fraction.clamp(0.0, 1.0);
    let color = if fraction <= 0.1 {
        THEME.danger
    } else if fraction <= 0.25 {
        THEME.accent
    } else {
        THEME.success
    };
    // Center the actual percentage with one clear cell on each side. Padding
    // the value to a fixed width would put extra whitespace on just one side.
    let area = Rect::new(0, 0, width as u16, 1);
    let mut buffer = Buffer::empty(area);
    let label = format!(" {:.0}% ", fraction * 100.0);
    let label_start = (width - label.len()) / 2;
    let label_range = label_start..label_start + label.len();
    Gauge::default()
        .ratio(fraction)
        .use_unicode(true)
        .label("")
        .gauge_style(Style::default().fg(color))
        .render(area, &mut buffer);
    let spans = buffer
        .content
        .iter()
        .enumerate()
        .map(|(index, cell)| {
            if label_range.contains(&index) {
                let foreground = if fraction <= 0.25 {
                    color
                } else {
                    THEME.strong
                };
                Span::styled(
                    label[index - label_start..index - label_start + 1].to_owned(),
                    Style::default()
                        .fg(foreground)
                        .bg(Color::Reset)
                        .add_modifier(Modifier::BOLD),
                )
            } else if cell.symbol() == " " && cell.bg != color {
                Span::styled("━", Style::default().fg(THEME.border_muted))
            } else {
                Span::styled(
                    "━",
                    Style::default().fg(super::theme::quota_color(
                        fraction,
                        index as f32 / (width - 1) as f32,
                    )),
                )
            }
        })
        .collect::<Vec<_>>();
    Line::from(spans)
}

fn account_content(account: &Account, scope: Scope) -> Text<'static> {
    let mut rows = Vec::new();
    if scope == Scope::Model {
        rows.push(Line::from(
            "供应商返回的各模型额度；不同模型可能共享配额，不能相加。",
        ));
        rows.push("这些数据不是本会话逐模型消耗记录。".into());
    }
    if let Some(error) = &account.error {
        rows.push(format!("刷新失败：{error}").into());
        if account.fetched_at.is_some() {
            rows.push("以下为上次成功查询的结果。".into());
        }
        rows.push(Line::default());
    }
    if let Some(plan) = &account.plan {
        rows.push(format!("套餐：{plan}").into());
    }
    if let Some(time) = account.fetched_at {
        let age = now().saturating_sub(time);
        rows.push(format!("更新时间：{age} 秒前").into());
    }
    if let Some(note) = &account.note {
        rows.push(note.clone().into());
    }
    rows.push(Line::default());
    if scope == Scope::Account
        && !account.limits.is_empty()
        && account
            .limits
            .iter()
            .all(|limit| limit.scope == Scope::Model)
    {
        rows.push("服务未提供共享套餐总额度；供应商返回的数据见「模型额度」。".into());
    }
    let limits = orbit_client::usage::account_limits(account, scope).collect::<Vec<_>>();
    let label_width = limits
        .iter()
        .map(|limit| quota_label(&limit.label).width())
        .max()
        .unwrap_or(0)
        .min(MAX_QUOTA_LABEL_WIDTH);
    for limit in limits {
        let label = super::render::fit_label(&quota_label(&limit.label), label_width, false);
        let padding = " ".repeat(label_width.saturating_sub(label.width()) + 2);
        let mut row = Line::from(vec![
            Span::styled(label, Style::default().fg(super::theme::THEME.foreground)),
            Span::raw(padding),
        ]);
        let unit = limit.unit.as_deref().unwrap_or("");
        if let Some(fraction) = limit.remaining_fraction {
            row.spans
                .extend(quota_bar(fraction, BAR_WIDTH).spans);
        }
        if let Some(value) = &limit.remaining {
            row.spans
                .push(Span::raw(format!("  剩余 {value} {unit}")));
        }
        if let Some(value) = &limit.used {
            row.spans
                .push(Span::raw(format!("  已用 {value} {unit}")));
        }
        if let Some(value) = &limit.total {
            row.spans
                .push(Span::raw(format!("  总量 {value} {unit}")));
        }
        if limit.remaining_fraction.is_none() && limit.remaining.is_none() && limit.used.is_none() {
            row.spans
                .push(Span::raw("服务器未提供剩余额度"));
        }
        if let Some(reset) = limit
            .resets_at
            .as_deref()
            .and_then(|reset| reset_countdown(reset, now()))
        {
            row.spans.push(Span::styled(
                format!("  {}", compact_reset(&reset)),
                Style::default().fg(super::theme::THEME.muted),
            ));
        }
        rows.push(row);
    }
    Text::from(rows)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn reset_countdown(reset: &str, current: u64) -> Option<String> {
    let time: u64 = orbit_client::usage::reset_timestamp(reset)?
        .try_into()
        .ok()?;
    let seconds = time.saturating_sub(current);
    if seconds == 0 {
        Some("等待刷新".into())
    } else if seconds >= 86_400 {
        Some(format!(
            "{}天{}小时后重置",
            seconds / 86_400,
            seconds % 86_400 / 3_600
        ))
    } else if seconds >= 3_600 {
        Some(format!(
            "{}小时{}分钟后重置",
            seconds / 3_600,
            seconds % 3_600 / 60
        ))
    } else if seconds >= 60 {
        Some(format!("{}分钟后重置", seconds / 60))
    } else {
        Some("不足1分钟后重置".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use orbit_protocol::usage::Limit;
    #[test]
    fn background_refresh_waits_between_attempts_including_failures() {
        let mut state = State::default();
        assert!(state.refresh_due());
        state.begin_refresh();
        assert!(!state.refresh_due());
        state.loading = false;
        state.error = Some("network unavailable".into());
        assert!(!state.refresh_due());
        state.requested = Some(Instant::now() - REFRESH_INTERVAL);
        assert!(state.refresh_due());
        state.receive(Snapshot::default());
        assert!(!state.refresh_due());
    }

    #[test]
    fn usage_separates_model_counters_and_keeps_shared_summary_compact() {
        let full = quota_bar(1.0, BAR_WIDTH);
        let empty = quota_bar(0.0, BAR_WIDTH);
        assert!(full.to_string().contains("100%"));
        assert!(empty.to_string().contains("0%"));
        for width in [8, 12, BAR_WIDTH] {
            for fraction in [0.0, 0.09, 0.73, 0.89, 1.0] {
                let bar = quota_bar(fraction, width);
                assert_eq!(bar.width(), width);
                let text = bar.to_string();
                let value = format!("{:.0}%", fraction * 100.0);
                let start = text
                    .chars()
                    .position(|ch| ch.is_ascii_digit())
                    .unwrap();
                let end = text
                    .chars()
                    .position(|ch| ch == '%')
                    .unwrap()
                    + 1;
                assert_eq!(
                    text.chars()
                        .skip(start)
                        .take(end - start)
                        .collect::<String>(),
                    value
                );
                assert_eq!(text.chars().nth(start - 1), Some(' '));
                assert_eq!(text.chars().nth(end), Some(' '));
                assert_ne!(text.chars().nth(start - 2), Some(' '));
                assert_ne!(text.chars().nth(end + 1), Some(' '));
                // Whole-cell positioning can differ from the exact midpoint
                // by half a cell when the percentage has an odd width.
                assert!(start.abs_diff(width - end) <= 1);
                let gap = bar
                    .spans
                    .iter()
                    .enumerate()
                    .filter_map(|(index, span)| {
                        span.style
                            .add_modifier
                            .contains(Modifier::BOLD)
                            .then_some(index)
                    })
                    .collect::<Vec<_>>();
                assert_eq!(gap, (start - 1..end + 1).collect::<Vec<_>>());
            }
        }
        assert_eq!(
            empty.spans[9].style.fg,
            Some(super::super::theme::THEME.danger)
        );
        assert_eq!(
            quota_bar(0.2, BAR_WIDTH).spans[0]
                .style
                .fg,
            Some(super::super::theme::THEME.md_mark)
        );
        assert_eq!(
            full.spans.first().unwrap().style.fg,
            Some(super::super::theme::THEME.success)
        );
        assert_eq!(full.width(), BAR_WIDTH);
        assert_eq!(empty.width(), BAR_WIDTH);
        assert_ne!(
            full.spans.first().unwrap().style.fg,
            full.spans.last().unwrap().style.fg
        );
        assert!(
            full.spans
                .iter()
                .chain(&empty.spans)
                .all(|span| span
                    .style
                    .bg
                    .is_none_or(|color| color == Color::Reset))
        );
        let mut state = State::default();
        state.receive(Snapshot {
            model: "gemini-3.8-flash".into(),
            account: Account {
                provider: "Antigravity".into(),
                name: "main".into(),
                limits: vec![
                    Limit {
                        label: "Gemini Models · Weekly Limit Remaining · weekly".into(),
                        remaining_fraction: Some(0.59),
                        ..Default::default()
                    },
                    Limit {
                        label: "Gemini Models · Five Hour Limit Remaining · 5h".into(),
                        remaining_fraction: Some(0.96),
                        ..Default::default()
                    },
                    Limit {
                        label: "Gemini Pro".into(),
                        scope: Scope::Model,
                        remaining_fraction: Some(0.2),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
            ..Default::default()
        });
        let indicator = state.indicator(80).to_string();
        assert!(indicator.contains("59%") && indicator.contains("96%"));
        assert_eq!(indicator.matches('%').count(), 2);
        assert!(!indicator.contains("Limit Remaining"));
        assert!(!indicator.contains("Gemini Pro"));
        assert!(!indicator.contains('('));
        assert_eq!(state.indicator(8).to_string(), "用量");
        let mut dialog = InfoDialog::new("用量", String::new());
        state.update_dialog(&mut dialog);
        assert!(!dialog.content.contains("Gemini Pro"));
        assert!(dialog.content.contains("Gemini · 周"));
        assert!(
            !dialog
                .content
                .contains("Limit Remaining")
        );
        assert!(!dialog.content.contains("未知"));
        let content = dialog.content.clone();
        state.begin_refresh();
        state.update_dialog(&mut dialog);
        assert_eq!(dialog.content, content);
        assert!(dialog.title.contains("刷新中"));
        assert!(
            account_content(&state.snapshot.as_ref().unwrap().account, Scope::Model)
                .to_string()
                .contains("Gemini Pro")
        );
        let mut account = state
            .snapshot
            .as_ref()
            .unwrap()
            .account
            .clone();
        account
            .limits
            .retain(|limit| limit.scope == Scope::Account);
        for window in ["weekly", "5h"] {
            account.limits.push(Limit {
                label: format!("Claude and GPT models · {window}"),
                remaining_fraction: Some(1.0),
                ..Default::default()
            });
        }
        let content = account_content(&account, Scope::Account);
        let rows = content
            .lines
            .iter()
            .filter(|row| row.width() > 0)
            .collect::<Vec<_>>();
        assert_eq!(rows.len(), 4);
        assert!(
            rows.iter()
                .all(|row| row.to_string().contains('%') && row.width() <= 80)
        );
        assert!(!content.to_string().contains("未知"));
        state
            .snapshot
            .as_mut()
            .unwrap()
            .account
            .limits
            .retain(|limit| limit.scope == Scope::Model);
        assert_eq!(state.indicator(80).to_string(), "用量");
    }

    #[test]
    fn quota_countdown_accepts_server_timestamps_and_never_guesses_missing_times() {
        let current = 1_700_000_000;
        let reset = current + 2 * 86_400 + 3 * 3_600;
        for timestamp in [reset.to_string(), (reset * 1_000).to_string()] {
            assert_eq!(
                reset_countdown(&timestamp, current).as_deref(),
                Some("2天3小时后重置")
            );
        }
        for timestamp in ["1970-01-01T05:30:00+02:00", "1970-01-01T03:30:00Z"] {
            assert_eq!(
                reset_countdown(timestamp, 3_600).as_deref(),
                Some("2小时30分钟后重置")
            );
        }
        assert_eq!(reset_countdown("59", 0).as_deref(), Some("不足1分钟后重置"));
        assert_eq!(reset_countdown("60", 0).as_deref(), Some("1分钟后重置"));
        assert_eq!(reset_countdown("60", 60).as_deref(), Some("等待刷新"));
        assert!(reset_countdown("unavailable", current).is_none());
        assert!(reset_countdown("0", current).is_none());
        let limit = Limit {
            label: "5小时".into(),
            remaining_fraction: Some(0.96),
            resets_at: Some("12600".into()),
            ..Default::default()
        };
        let indicator = footer_limit(&limit, 12, 3_600).to_string();
        assert!(indicator.starts_with("5h "));
        assert!(indicator.contains("96%"));
        assert!(indicator.ends_with(" (2时30分)"));
        assert!(!indicator.contains("后重置"));
        assert_eq!(
            quota_label("Claude and GPT models · Weekly Limit Remaining · weekly"),
            "Claude / GPT · 周"
        );
        assert_eq!(quota_label("每周"), "周");
        assert_eq!(quota_label("附加 Credits"), "附加 Credits");
    }

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
        let summary = state.indicator(80).to_string();
        assert!(summary.contains("40%"));
        assert!(summary.ends_with(" · 旧数据"));
        assert!(
            account_content(&state.snapshot.as_ref().unwrap().account, Scope::Account)
                .to_string()
                .contains("上次成功")
        );
        let mut other = Snapshot::default();
        other.account.identity = "source-b".into();
        other.account.error = Some("另一个账号不可用".into());
        state.receive(other);
        assert_eq!(state.indicator(80).to_string(), "用量");
    }
}
