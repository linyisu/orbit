use std::{collections::HashMap, time::Duration};

use anyhow::Result;
use orbit_agent::{agent::AgentResult, error::AgentError, events::AgentEvent, tool::ToolRegistry};
use orbit_model::types::ChatMessage;
use tokio::{sync::mpsc, time};
use tokio_util::sync::CancellationToken;

use super::{
    LocalTask, MarketplaceTask, ModelOption, ModelRefreshTask, RunTask,
    app::{TranscriptItem, UiState},
    apply_live_event, queue_approval, refresh_context_usage, session_sync, start_agent_run,
    steering_history, tabs,
};

pub(super) struct BackgroundRun {
    pub last_publish: std::time::Instant,
    pub mcp_statuses: Vec<orbit_tools::mcp::McpServerStatus>,
    pub model_options: Vec<ModelOption>,
    pub task: Option<RunTask>,
    pub local_task: Option<LocalTask>,
    pub local_cancellation: Option<CancellationToken>,
    pub history_replay: Option<crate::session::HistoryReplay>,
    pub move_notice: Option<String>,
    pub marketplace_task: Option<MarketplaceTask>,
    pub model_refresh: Option<ModelRefreshTask>,
    pub retry_submit: bool,
    pub refresh_before_retry: bool,
    pub receiver: Option<mpsc::Receiver<AgentEvent>>,
    pub cancellation: Option<CancellationToken>,
    pub pending: Option<orbit_tools::approval::ApprovalRequest>,
    pub guidance: Vec<String>,
    pub active_context: Option<ChatMessage>,
    pub tools: ToolRegistry,
    pub undo_writer: orbit_tools::write_file::WriteFile,
    pub approvals: orbit_tools::approval::ApprovalReceiver,
    pub instructions: crate::instructions::Instructions,
    pub skills: crate::skills::Skills,
    pub memory_scope: crate::project_state::MemoryScope,
    pub auto_compact: bool,
    pub max_turns: usize,
    pub max_tool_calls: usize,
    pub compaction: orbit_agent::compaction::CompactionRuntime,
}

pub(super) type Runs = HashMap<String, BackgroundRun>;

impl BackgroundRun {
    pub fn busy(&self) -> bool {
        self.task.is_some()
            || self.local_task.is_some()
            || self.history_replay.is_some()
            || self.marketplace_task.is_some()
            || self.model_refresh.is_some()
    }
}

pub(super) fn local_result(
    result: std::result::Result<
        Result<String, orbit_agent::tool::ToolError>,
        tokio::task::JoinError,
    >,
) -> TranscriptItem {
    match result {
        Ok(Ok(result)) => TranscriptItem::CommandResult(super::format_undo_result(&result)),
        Ok(Err(error)) => TranscriptItem::CommandError(error.to_string()),
        Err(error) => TranscriptItem::CommandError(format!("回退操作中断：{error}")),
    }
}

pub(super) fn marketplace_result(
    result: std::result::Result<Result<crate::marketplaces::Marketplace>, tokio::task::JoinError>,
) -> TranscriptItem {
    match result {
        Ok(Ok(market)) => TranscriptItem::CommandResult(format!(
            "已添加市场 {}，发现 {} 个插件。\n{}",
            market.name,
            market.plugins.len(),
            market.directory.display()
        )),

        Ok(Err(error)) => TranscriptItem::CommandError(format!("无法添加市场：{error}")),
        Err(error) => TranscriptItem::CommandError(format!("市场下载任务中断：{error}")),
    }
}

pub(super) fn model_refresh_result(
    result: std::result::Result<Result<super::ModelRefreshOutput>, tokio::task::JoinError>,
    state: &mut UiState,
    model_options: &mut Vec<ModelOption>,
) {
    match result {
        Ok(Ok((models, diagnostics))) => {
            *model_options = models;
            state.model_completions = model_options
                .iter()
                .map(ModelOption::qualified_name)
                .collect();
            for diagnostic in diagnostics {
                state.report_local(TranscriptItem::CommandError(diagnostic));
            }
            state.model_picker = Some(super::dialogs::ModelPicker::new(
                model_options.clone(),
                state.model.clone(),
                state.reasoning_effort,
            ));
        }

        Ok(Err(error)) => state.report_local(TranscriptItem::CommandError(format!(
            "模型刷新失败：{error}"
        ))),
        Err(error) => state.report_local(TranscriptItem::CommandError(format!(
            "模型刷新任务失败：{error}"
        ))),
    }
}

pub(super) fn outcome(
    result: std::result::Result<Result<AgentResult, AgentError>, tokio::task::JoinError>,
    cancellation: Option<&CancellationToken>,
) -> (&'static str, AgentEvent, Option<Vec<ChatMessage>>) {
    match result {
        Ok(Ok(result)) if matches!(&result.final_message, ChatMessage::Assistant { content, tool_calls } if content.as_deref().unwrap_or_default().trim().is_empty() && tool_calls.is_empty()) => {
            (
                "failed",
                AgentEvent::Failed {
                    error: "模型已结束响应，但未返回回答或工具调用；请检查服务端返回内容。".into(),
                },
                None,
            )
        }

        Ok(Ok(result)) => (
            "completed",
            AgentEvent::Completed {
                turns: result.turns,
                tool_calls: result.tool_calls,
            },
            Some(result.history),
        ),

        Ok(Err(_)) if cancellation.is_some_and(CancellationToken::is_cancelled) => {
            ("cancelled", AgentEvent::Cancelled, None)
        }
        Ok(Err(AgentError::Cancelled)) => ("cancelled", AgentEvent::Cancelled, None),
        Ok(Err(AgentError::CompactionFailed { kind, .. })) => (
            "failed",
            AgentEvent::Failed {
                error: format!("上下文整理未完成：{}。原上下文已保留。", kind.label()),
            },
            None,
        ),

        Ok(Err(AgentError::Compaction(_))) => (
            "failed",
            AgentEvent::Failed {
                error: "上下文整理未完成，原上下文已保留。".into(),
            },
            None,
        ),

        Ok(Err(error)) => (
            "failed",
            AgentEvent::Failed {
                error: error.to_string(),
            },
            None,
        ),

        Err(error) => (
            "failed",
            AgentEvent::Failed {
                error: format!("后台任务中断：{error}"),
            },
            None,
        ),
    }
}

pub(super) async fn poll(
    runs: &mut Runs,
    tabs: &mut tabs::Tabs,
    sync: &mut session_sync::SessionSync,
    model_options: &mut Vec<ModelOption>,
) -> Result<(bool, bool)> {
    let mut changed = false;
    let mut refreshed_models = None;
    for (id, run) in runs.iter_mut() {
        let Some(tab) = tabs.parked_mut(id) else {
            continue;
        };
        if run
            .marketplace_task
            .as_ref()
            .is_some_and(|task| task.is_finished())
        {
            let result = run.marketplace_task.take().unwrap().await;
            tab.state.report_local(marketplace_result(result));
            changed = true;
        }

        if run
            .model_refresh
            .as_ref()
            .is_some_and(|task| task.is_finished())
        {
            let result = run.model_refresh.take().unwrap().await;
            let refreshed = matches!(&result, Ok(Ok(_)));
            model_refresh_result(result, &mut tab.state, &mut run.model_options);
            if refreshed {
                refreshed_models = Some(run.model_options.clone());
            }
            changed = true;
        }

        if run
            .local_task
            .as_ref()
            .is_some_and(|task| task.is_finished())
        {
            let result = run.local_task.take().unwrap().await;
            run.local_cancellation = None;
            tab.state.report_local(local_result(result));
            changed = true;
        }

        if let Some(replay) = &mut run.history_replay {
            if replay.advance(
                &tab.session.snapshot,
                &mut tab.state,
                2048,
                Duration::from_millis(4),
            ) {
                run.history_replay = None;
                tab.state.resume_loading = None;
                session_sync::sync_mode(&tab.session.store, id, &mut tab.state).await?;
                if let Some(notice) = run.move_notice.take() {
                    tab.state
                        .report_local(TranscriptItem::CommandResult(notice));
                }

                refresh_context_usage(&mut tab.state, &tab.history, &run.tools);
                session_sync::sync_runtime(&tab.session.store, id, &mut tab.state).await?;
            } else {
                tab.state.resume_loading = Some(format!(
                    "正在显示会话历史…  {}/{}",
                    replay.completed_runs(),
                    tab.session.snapshot.runs.len()
                ));
            }

            changed = true;
        }

        if run.task.is_none() {
            if run.local_task.is_none()
                && run.history_replay.is_none()
                && super::model_catalog::reconcile(
                    &mut tab.state,
                    &mut tab.model,
                    &run.model_options,
                )
            {
                refresh_context_usage(&mut tab.state, &tab.history, &run.tools);
                changed = true;
            }

            continue;
        }

        if let Some(receiver) = &mut run.receiver {
            for _ in 0..256 {
                let Ok(event) = receiver.try_recv() else {
                    break;
                };
                apply_live_event(event, &mut tab.state);
                changed = true;
            }
        }

        if run
            .pending
            .as_ref()
            .is_some_and(|request| request.decision.is_closed())
        {
            run.pending = None;
            tab.state.approval = None;
            tab.state.resume_clock();
        }

        if run.pending.is_none()
            && let Ok(request) = run.approvals.try_recv()
        {
            session_sync::sync_mode(&tab.session.store, id, &mut tab.state).await?;
            queue_approval(request, &mut tab.state, &tab.session, &mut run.pending);
            changed = true;
        }

        if run.last_publish.elapsed() >= Duration::from_millis(200) {
            tab.session.flush_events().await?;
            session_sync::publish(&tab.session, &tab.state, tab.session.active_run_id()).await?;
            run.last_publish = std::time::Instant::now();
            changed = true;
        }

        if !run.task.as_ref().is_some_and(|task| task.is_finished()) {
            continue;
        }
        let result = run.task.take().unwrap().await;
        if let Some(receiver) = &mut run.receiver {
            while let Ok(event) = receiver.try_recv() {
                apply_live_event(event, &mut tab.state);
            }
        }

        run.pending = None;
        tab.state.approval = None;
        let (status, event, history) = outcome(result, run.cancellation.as_ref());
        let finished_run = tab.session.active_run_id();
        tab.session.finish(status, history.clone()).await?;
        sync.reset(id);
        if history.is_some() || tab.session.snapshot.recovery.is_some() {
            tab.history = tab.session.snapshot.history.clone();
            run.active_context = None;
            crate::instructions::inject(&mut tab.history, &run.instructions);
            run.skills.inject(&mut tab.history);
            crate::project_state::inject(
                &mut tab.history,
                &tab.session.store,
                &tab.session.snapshot.session.project_id,
                id,
            )
            .await?;
        }

        tab.state.apply(event);
        super::model_catalog::reconcile(&mut tab.state, &mut tab.model, &run.model_options);
        refresh_context_usage(&mut tab.state, &tab.history, &run.tools);
        session_sync::publish(&tab.session, &tab.state, finished_run).await?;
        run.receiver = None;
        run.cancellation = None;
        if !run.guidance.is_empty() {
            let guidance = std::mem::take(&mut run.guidance).join("\n\n");
            let history = steering_history(&tab.history, run.active_context.take());
            run.active_context = Some(ChatMessage::user(guidance.clone()));
            let context_tokens = tab.state.context_tokens;
            match start_agent_run(
                guidance.clone(),
                Vec::new(),
                false,
                &mut tab.state,
                &mut tab.session,
                &history,
                &tab.model,
                &run.tools,
                context_tokens,
                run.auto_compact,
                run.max_turns,
                run.max_tool_calls,
                &run.compaction,
            )
            .await
            {
                Ok(started) => {
                    run.task = Some(started.task);
                    run.receiver = Some(started.receiver);
                    run.cancellation = Some(started.cancellation);
                }

                Err(error) => {
                    tab.state.input.set(guidance);
                    tab.state
                        .report_local(TranscriptItem::CommandError(error.to_string()));
                    run.active_context = None;
                }
            }
        } else {
            run.active_context = None;
        }

        changed = true;
    }

    let catalog_refreshed = refreshed_models.is_some();
    if let Some(models) = refreshed_models {
        super::model_catalog::publish_background(models, model_options, runs, tabs);
    }
    for id in tabs.prune_closed(runs) {
        runs.remove(&id);
    }
    Ok((changed, catalog_refreshed))
}

pub(super) async fn shutdown(runs: &mut Runs, tabs: &mut tabs::Tabs) -> Result<()> {
    for run in runs.values_mut() {
        run.pending = None;
        run.model_refresh = None;
        if let Some(token) = &run.cancellation {
            token.cancel();
        }
        if let Some(token) = &run.local_cancellation {
            token.cancel();
        }
    }

    for (id, run) in runs.iter_mut() {
        if let Some(mut task) = run.task.take()
            && time::timeout(Duration::from_millis(500), &mut task)
                .await
                .is_err()
        {
            task.abort();
            let _ = task.await;
        }

        if let Some(mut task) = run.local_task.take()
            && time::timeout(Duration::from_millis(500), &mut task)
                .await
                .is_err()
        {
            task.abort();
            let _ = task.await;
        }

        if let Some(tab) = tabs.parked_mut(id) {
            tab.session.close().await?;
        }
        if let Some(mut task) = run.marketplace_task.take()
            && time::timeout(Duration::from_millis(500), &mut task)
                .await
                .is_err()
        {
            task.abort();
            let _ = task.await;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests;
