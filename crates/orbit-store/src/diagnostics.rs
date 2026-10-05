//! Local execution diagnostics derived from the durable event log; no telemetry export.

use anyhow::{Context, Result};
use orbit_agent::events::AgentEvent;
use orbit_model::types::{StreamEvent, Usage};
use serde::{Deserialize, Serialize};

use crate::store::Store;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SessionDiagnostics {
    pub session_id: String,
    pub runs: Vec<RunDiagnostic>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunDiagnostic {
    pub run_id: i64,
    pub status: String,
    pub elapsed_ms: Option<u64>,
    pub first_event_at_ms: Option<i64>,
    pub last_event_at_ms: Option<i64>,
    pub turns: usize,
    pub model_events: usize,
    pub text_characters: usize,
    pub reasoning_characters: usize,
    pub tool_started: usize,
    pub tool_finished: usize,
    pub tool_failed: usize,
    pub compactions: usize,
    pub compaction_failures: usize,
    /// Sanitized fallback reason, when recorded by the original run.
    pub compaction_fallback: Option<String>,
    pub event_decode_failures: usize,
    pub usage: Option<Usage>,
    pub requests: usize,
    pub known_retries: u64,
    pub model_elapsed_ms: u64,
    pub latest_estimated_input: Option<usize>,
    pub failure: Option<String>,
}

impl Store {
    pub async fn diagnostics(&self, session_id: &str) -> Result<SessionDiagnostics> {
        let session_id = session_id.to_owned();

        self.read_access(move |conn| {
            conn.query_row("SELECT id FROM sessions WHERE id=?1", [&session_id], |_| {
                Ok(())
            })
            .context("session not found")?;
            let mut query = conn
                .prepare("SELECT id,status,elapsed_ms FROM runs WHERE session_id=?1 ORDER BY id")?;
            let runs = query
                .query_map([&session_id], |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<i64>>(2)?
                            .map(|value| value.max(0) as u64),
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let mut diagnostics = Vec::with_capacity(runs.len());

            for (run_id, status, elapsed_ms) in runs {
                let mut events = conn.prepare(
                    "SELECT payload,created_at_ms FROM events WHERE run_id=?1 ORDER BY id",
                )?;
                let events = events
                    .query_map([run_id], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;

                diagnostics.push(summarize(run_id, status, elapsed_ms, events));
            }

            Ok(SessionDiagnostics {
                session_id,
                runs: diagnostics,
            })
        })
        .await
    }
}

fn summarize(
    run_id: i64,
    status: String,
    elapsed_ms: Option<u64>,
    events: Vec<(String, Option<i64>)>,
) -> RunDiagnostic {
    let mut result = RunDiagnostic {
        run_id,
        status,
        elapsed_ms,
        first_event_at_ms: None,
        last_event_at_ms: None,
        turns: 0,
        model_events: 0,
        text_characters: 0,
        reasoning_characters: 0,
        tool_started: 0,
        tool_finished: 0,
        tool_failed: 0,
        compactions: 0,
        compaction_failures: 0,
        compaction_fallback: None,
        event_decode_failures: 0,
        usage: None,
        requests: 0,
        known_retries: 0,
        model_elapsed_ms: 0,
        latest_estimated_input: None,
        failure: None,
    };
    let mut compaction_records = 0;
    let mut compaction_statuses = 0;

    for (payload, timestamp) in events {
        if let Some(timestamp) = timestamp {
            result.first_event_at_ms.get_or_insert(timestamp);
            result.last_event_at_ms = Some(timestamp);
        }

        let Ok(event) = serde_json::from_str::<AgentEvent>(&payload) else {
            result.event_decode_failures += 1;
            continue;
        };

        match event {
            AgentEvent::TurnStarted { .. } => result.turns += 1,

            AgentEvent::Model(event) => {
                result.model_events += 1;

                match event {
                    StreamEvent::TextDelta(text) => result.text_characters += text.chars().count(),
                    StreamEvent::ReasoningDelta(text) => {
                        result.reasoning_characters += text.chars().count()
                    }
                    StreamEvent::Completed { usage, .. } => {
                        if usage.is_some() {
                            result.usage = usage;
                        }
                    }

                    StreamEvent::ToolCallDelta(_) => {}
                }
            }

            AgentEvent::ToolStarted { .. } => result.tool_started += 1,
            AgentEvent::ToolFinished { .. } => result.tool_finished += 1,
            AgentEvent::ToolFailed { error, .. } => {
                result.tool_failed += 1;
                result.failure = Some(error);
            }

            AgentEvent::CompactionRecorded { record } => {
                compaction_records += 1;

                if record.fallback {
                    result.compaction_fallback = record
                        .fallback_reason
                        .or_else(|| Some("旧记录未保存回退原因".into()));
                }
            }

            AgentEvent::ContextCompacted {
                fallback,
                fallback_reason,
                ..
            } => {
                compaction_statuses += 1;

                if fallback {
                    result.compaction_fallback =
                        fallback_reason.or_else(|| Some("旧记录未保存回退原因".into()));
                }
            }

            AgentEvent::CompactionFailed { error, .. } => {
                result.compaction_failures += 1;
                result.failure = Some(error);
            }

            AgentEvent::Failed { error } => result.failure = Some(error),

            AgentEvent::RequestMetrics {
                estimated_input,
                retries,
                usage,
                elapsed_ms,
                ..
            } => {
                result.requests += 1;
                result.known_retries = result
                    .known_retries
                    .saturating_add(u64::from(retries.unwrap_or(0)));
                result.model_elapsed_ms = result.model_elapsed_ms.saturating_add(elapsed_ms);
                result.latest_estimated_input = Some(estimated_input);

                if usage.is_some() {
                    result.usage = usage;
                }
            }

            AgentEvent::ReasoningFinished { .. }
            | AgentEvent::ToolBackgrounded { .. }
            | AgentEvent::WaitingForCommands { .. }
            | AgentEvent::WaitingForTasks { .. }
            | AgentEvent::InputReceived { .. }
            | AgentEvent::VerificationRecorded { .. }
            | AgentEvent::ContextUsage { .. }
            | AgentEvent::Completed { .. }
            | AgentEvent::CompactionStarted
            | AgentEvent::Cancelled => {}
        }
    }

    // A successful compaction emits both an audit record and a UI-status
    // event. They describe one operation, not two. Keep support for older
    // sessions that have only the status event.
    result.compactions = compaction_records.max(compaction_statuses);

    result
}

pub fn text(diagnostics: &SessionDiagnostics) -> String {
    if diagnostics.runs.is_empty() {
        return "该会话尚无运行记录。".into();
    }

    let mut output = format!("会话诊断\n{} 次运行\n", diagnostics.runs.len());

    for run in &diagnostics.runs {
        use std::fmt::Write;

        let elapsed = run
            .elapsed_ms
            .map(format_duration)
            .unwrap_or_else(|| "耗时未记录".into());
        let _ = writeln!(
            output,
            "\n#{}  {} · {}\n  轮次 {} · 模型事件 {} · 正文 {} 字 · 思考 {} 字\n  工具 启动 {} / 成功 {} / 失败 {} · 上下文整理 {}{}",
            run.run_id,
            run.status,
            elapsed,
            run.turns,
            run.model_events,
            run.text_characters,
            run.reasoning_characters,
            run.tool_started,
            run.tool_finished,
            run.tool_failed,
            run.compactions,
            if run.compaction_failures > 0 {
                format!("，失败 {}", run.compaction_failures)
            } else {
                String::new()
            },
        );

        if run.requests > 0 {
            let _ = writeln!(
                output,
                "  模型请求 {} · 已记录重试 {} · 累计 {} ms · 最近本地估算 {} tokens",
                run.requests,
                run.known_retries,
                run.model_elapsed_ms,
                run.latest_estimated_input.unwrap_or(0)
            );
        }

        if let Some(usage) = &run.usage {
            let _ = writeln!(
                output,
                "  服务端用量 输入 {} · 输出 {} · 总计 {}",
                usage
                    .prompt_tokens
                    .map_or_else(|| "未提供".into(), |value| value.to_string()),
                usage
                    .completion_tokens
                    .map_or_else(|| "未提供".into(), |value| value.to_string()),
                usage
                    .total_tokens
                    .map_or_else(|| "未提供".into(), |value| value.to_string()),
            );
        }

        if let Some(error) = &run.failure {
            let _ = writeln!(output, "  最后异常：{error}");
        }

        if let Some(reason) = &run.compaction_fallback {
            let _ = writeln!(output, "  最近整理回退：{reason}");
        }

        if run.event_decode_failures > 0 {
            let _ = writeln!(
                output,
                "  注意：{} 条旧事件无法按当前格式解析。",
                run.event_decode_failures
            );
        }
    }

    output
}

fn format_duration(milliseconds: u64) -> String {
    if milliseconds < 1_000 {
        return format!("{milliseconds}ms");
    }

    format!("{:.1}s", milliseconds as f64 / 1_000.0)
}

#[cfg(test)]
mod tests {
    use orbit_model::types::{ChatMessage, ToolCall};
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;

    #[tokio::test]
    async fn diagnostics_distinguish_model_tool_and_compaction_events() {
        let directory = tempdir().unwrap();
        let store = Store::open(directory.path().join("orbit.db"))
            .await
            .unwrap();
        let mut writer = store
            .create(
                directory.path().to_string_lossy().into_owned(),
                "test".into(),
                false,
            )
            .await
            .unwrap();
        let id = writer.id().to_owned();
        writer.begin("inspect".into(), "test".into()).await.unwrap();
        let call = ToolCall::function("call", "read_file", json!({ "path": "missing" }));
        writer
            .append(vec![
                AgentEvent::TurnStarted { turn: 1 },
                AgentEvent::Model(StreamEvent::ReasoningDelta("why".into())),
                AgentEvent::ToolStarted { call: call.clone() },
                AgentEvent::ToolFailed {
                    call,
                    error: "not found".into(),
                },
                AgentEvent::ContextCompacted {
                    strategy: Default::default(),
                    before_tokens: 100,
                    after_tokens: 50,
                    fallback: true,
                    fallback_reason: Some("摘要请求超时".into()),
                },
            ])
            .await
            .unwrap();
        writer
            .finish(
                "completed",
                vec![AgentEvent::Completed {
                    turns: 1,
                    tool_calls: 1,
                }],
                Some(vec![ChatMessage::user("inspect")]),
            )
            .await
            .unwrap();
        let run = &store.diagnostics(&id).await.unwrap().runs[0];

        assert_eq!(run.turns, 1);
        assert_eq!(run.reasoning_characters, 3);
        assert_eq!(run.tool_started, 1);
        assert_eq!(run.tool_failed, 1);
        assert_eq!(run.compactions, 1);
        assert_eq!(run.compaction_fallback.as_deref(), Some("摘要请求超时"));
        assert_eq!(run.failure.as_deref(), Some("not found"));
        assert!(run.first_event_at_ms.is_some());
        assert!(text(&store.diagnostics(&id).await.unwrap()).contains("最后异常：not found"));
    }
}
