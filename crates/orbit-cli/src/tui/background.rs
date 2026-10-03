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
                state
                    .transcript
                    .push(TranscriptItem::CommandError(diagnostic));
            }
            state.model_picker = Some(super::dialogs::ModelPicker::new(
                model_options.clone(),
                state.model.clone(),
                state.reasoning_effort,
            ));
        }
        Ok(Err(error)) => state.transcript.push(TranscriptItem::CommandError(format!(
            "模型刷新失败：{error}"
        ))),
        Err(error) => state.transcript.push(TranscriptItem::CommandError(format!(
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
) -> Result<bool> {
    let mut changed = false;
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
            tab.state.transcript.push(marketplace_result(result));
            changed = true;
        }
        if run
            .model_refresh
            .as_ref()
            .is_some_and(|task| task.is_finished())
        {
            let result = run.model_refresh.take().unwrap().await;
            model_refresh_result(result, &mut tab.state, &mut run.model_options);
            changed = true;
        }
        if run
            .local_task
            .as_ref()
            .is_some_and(|task| task.is_finished())
        {
            let result = run.local_task.take().unwrap().await;
            run.local_cancellation = None;
            tab.state.transcript.push(local_result(result));
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
                        .transcript
                        .push(TranscriptItem::CommandResult(notice));
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
                        .transcript
                        .push(TranscriptItem::CommandError(error.to_string()));
                    run.active_context = None;
                }
            }
        } else {
            run.active_context = None;
        }
        changed = true;
    }
    for id in tabs.prune_closed(runs) {
        runs.remove(&id);
    }
    Ok(changed)
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
mod tests {
    use super::super::app::{self, UiState};
    use super::*;
    use crate::session::SessionState;
    use crossterm::event::{KeyCode, KeyModifiers};
    use orbit_model::{client::ModelClient, types::StreamEvent};
    use orbit_tools::approval::{ApprovalPreview, ApprovalRequest, McpPreview};
    use tokio::{select, sync::oneshot};

    fn options(dir: &std::path::Path) -> crate::TuiOptions {
        let config = dir.join("config.toml");
        std::fs::write(
            &config,
            r#"default_provider = "test"
[providers.test]
base_url = "http://127.0.0.1:1/v1"
api_key = "test-only"
models = [{ id = "mock" }]
"#,
        )
        .unwrap();
        crate::TuiOptions {
            initial_directory: dir.to_path_buf(),
            auth_directory: dir.join("auth"),
            config,
            database: dir.join("sessions.db"),
            allow_mcp_start: false,
            home: None,
            config_home: None,
            instruction_dir: None,
            editor_command: "vi".into(),
        }
    }

    async fn running(
        setup: &mut crate::PreparedTui,
        label: &str,
    ) -> (
        BackgroundRun,
        oneshot::Sender<()>,
        orbit_tools::approval::ApprovalSender,
    ) {
        setup
            .session
            .begin(label.into(), "test::mock".into())
            .await
            .unwrap();
        let journal = setup.session.execution_journal().unwrap();
        let (sender, receiver) = mpsc::channel(4);
        let (release, wait) = oneshot::channel();
        let (approval_sender, approvals) = orbit_tools::approval::channel();
        let cancellation = CancellationToken::new();
        let token = cancellation.clone();
        let label = label.to_owned();
        let task = tokio::spawn(async move {
            for index in 0..32 {
                let event = AgentEvent::Model(StreamEvent::TextDelta(format!("{label}-{index} ")));
                journal.record(event.clone());
                sender
                    .send(event)
                    .await
                    .map_err(|_| AgentError::Cancelled)?;
            }
            select! {
                _ = wait => {},
                _ = token.cancelled() => return Err(AgentError::Cancelled),
            }
            let final_message = ChatMessage::assistant(format!("{label} completed"));
            Ok(AgentResult {
                history: vec![ChatMessage::user(label), final_message.clone()],
                final_message,
                turns: 1,
                tool_calls: 0,
            })
        });
        let mut run = runtime(setup);
        run.task = Some(task);
        run.receiver = Some(receiver);
        run.cancellation = Some(cancellation);
        run.approvals = approvals;
        (run, release, approval_sender)
    }

    fn runtime(setup: &crate::PreparedTui) -> BackgroundRun {
        let resources = &setup.resources;
        BackgroundRun {
            task: None,
            local_task: None,
            local_cancellation: None,
            history_replay: None,
            move_notice: None,
            marketplace_task: None,
            model_refresh: None,
            retry_submit: false,
            refresh_before_retry: false,
            receiver: None,
            cancellation: None,
            pending: None,
            guidance: Vec::new(),
            active_context: None,
            tools: resources.tools.clone(),
            undo_writer: resources.undo_writer.clone(),
            approvals: orbit_tools::approval::channel().1,
            instructions: resources.budget.instructions.clone(),
            skills: resources.budget.skills.clone(),
            memory_scope: resources.budget.memory_scope.clone(),
            auto_compact: false,
            max_turns: 300,
            max_tool_calls: 2000,
            compaction: resources.budget.compaction.clone(),
            last_publish: std::time::Instant::now(),
            mcp_statuses: Vec::new(),
            model_options: Vec::new(),
        }
    }

    #[tokio::test]
    async fn two_background_runs_stream_finish_and_keep_resources_and_approvals_separate() {
        let dir = tempfile::tempdir().unwrap();
        let options = options(dir.path());
        let first_dir = dir.path().join("first");
        let second_dir = dir.path().join("second");
        std::fs::create_dir_all(&first_dir).unwrap();
        std::fs::create_dir_all(&second_dir).unwrap();
        let mut first = crate::prepare_tui(&options, &first_dir, None)
            .await
            .unwrap();
        let first_id = first.session.id().to_owned();
        let scratch = first.session.scratch.clone();
        let first_scope = first.resources.budget.memory_scope.clone();
        let (first_run, first_release, approval_sender) = running(&mut first, "alpha").await;
        let mut state = UiState::new("test::mock");
        state.workspace = first_dir.to_str().unwrap().into();
        state.submit("alpha".into());
        let mut history = Vec::new();
        let mut model = first.resources.model;
        let mut session = first.session;
        let mut tabs = tabs::Tabs::new(&session);
        let mut second = crate::prepare_tui(&options, &second_dir, None)
            .await
            .unwrap();
        let second_id = second.session.id().to_owned();
        let (second_run, second_release, _) = running(&mut second, "beta").await;
        let prepared =
            SessionState::prepare_shared(session.store.clone(), String::new(), second_id.clone())
                .await
                .unwrap();
        tabs.activate(
            &mut session,
            &mut state,
            &mut model,
            &mut history,
            (
                prepared,
                UiState::new("test::mock"),
                second.resources.model.clone(),
                Vec::new(),
            ),
        );
        session = second.session;
        state.submit("beta".into());
        let third = session.prepare_new("test::mock".into()).await.unwrap();
        tabs.activate(
            &mut session,
            &mut state,
            &mut model,
            &mut history,
            (
                third,
                UiState::new("test::mock"),
                ModelClient::new(Default::default()).unwrap(),
                Vec::new(),
            ),
        );
        let third_id = session.id().to_owned();
        let mut sync = session_sync::SessionSync::default();
        let mut runs = Runs::from([
            (first_id.clone(), first_run),
            (second_id.clone(), second_run),
        ]);
        let (decision, mut answer) = oneshot::channel();
        approval_sender
            .send(ApprovalRequest {
                preview: ApprovalPreview::Mcp(McpPreview {
                    server: "fixture".into(),
                    tool: "write".into(),
                    arguments: "{}".into(),
                }),
                decision,
            })
            .await
            .unwrap();
        for _ in 0..40 {
            tokio::task::yield_now().await;
            poll(&mut runs, &mut tabs, &mut sync).await.unwrap();
        }
        assert_eq!(session.id(), third_id);
        assert!(state.transcript.is_empty());
        assert_eq!(first_scope.session_id(), first_id);
        assert!(scratch.read().unwrap().ends_with(&first_id));
        assert!(tabs.parked_mut(&first_id).unwrap().state.approval.is_some());
        assert!(
            tabs.parked_mut(&second_id)
                .unwrap()
                .state
                .approval
                .is_none()
        );
        assert!(answer.try_recv().is_err());
        assert!(tabs.close_parked(&first_id));
        tabs.sync_ui(&mut state, &session);
        assert!(!state.tabs.iter().any(|tab| tab.id == first_id));
        assert!(tabs.contains(&first_id));
        assert!(tabs.restore(
            &first_id,
            &mut session,
            &mut state,
            &mut model,
            &mut history
        ));
        assert!(session.active_run_id().is_some());
        assert!(session.store.acquire(&first_id).await.is_err());
        assert!(session.scratch.read().unwrap().ends_with(&first_id));
        assert!(state.approval.is_some());
        let event = crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Char('2'),
            KeyModifiers::ALT,
        ));
        assert_eq!(
            super::super::tab_target(&event, &state, &tabs),
            Some(super::super::TabAction::Select(third_id.clone()))
        );
        assert!(tabs.restore(
            &third_id,
            &mut session,
            &mut state,
            &mut model,
            &mut history
        ));
        tabs.persist(&session).await.unwrap();
        assert!(
            tabs.parked_mut(&first_id)
                .unwrap()
                .session
                .active_run_id()
                .is_some()
        );

        tabs.sync_ui(&mut state, &session);
        assert!(
            state
                .tabs
                .iter()
                .find(|tab| tab.id == first_id)
                .unwrap()
                .waiting_approval
        );
        let request = runs.get_mut(&first_id).unwrap().pending.take().unwrap();
        request.decision.send(true).unwrap();
        assert!(answer.await.unwrap());
        first_release.send(()).unwrap();
        second_release.send(()).unwrap();
        time::timeout(Duration::from_secs(2), async {
            loop {
                poll(&mut runs, &mut tabs, &mut sync).await.unwrap();
                if runs.values().all(|run| run.task.is_none()) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        for (id, own, other) in [(&first_id, "alpha", "beta"), (&second_id, "beta", "alpha")] {
            let tab = tabs.parked_mut(id).unwrap();
            assert_eq!(tab.state.status, app::RunStatus::Completed);
            let text = format!("{:?}", tab.state.transcript);
            assert!(text.contains(own), "{text}");
            assert!(!text.contains(other), "{text}");
            let snapshot = session.store.load(id).await.unwrap();
            assert_eq!(snapshot.runs.last().unwrap().status, "completed");
            assert!(format!("{:?}", snapshot.history).contains(own));
            assert!(session.store.acquire(id).await.is_ok());
        }
        assert!(tabs.restore(
            &first_id,
            &mut session,
            &mut state,
            &mut model,
            &mut history
        ));
        assert_eq!(session.id(), first_id);
        assert!(session.scratch.read().unwrap().ends_with(&first_id));
        assert_eq!(state.status, app::RunStatus::Completed);
    }

    #[tokio::test]
    async fn closing_the_active_tab_keeps_its_run_until_completion() {
        let dir = tempfile::tempdir().unwrap();
        let options = options(dir.path());
        let mut setup = crate::prepare_tui(&options, dir.path(), None)
            .await
            .unwrap();
        let id = setup.session.id().to_owned();
        let (run, release, _) = running(&mut setup, "hidden task").await;
        let token = run.cancellation.clone().unwrap();
        let mut session = setup.session;
        let mut model = setup.resources.model;
        let mut state = UiState::new("test::mock");
        state.submit("hidden task".into());
        let mut history = Vec::new();
        let mut tabs = tabs::Tabs::new(&session);
        let next = session.prepare_new("test::mock".into()).await.unwrap();
        tabs.activate(
            &mut session,
            &mut state,
            &mut model,
            &mut history,
            (
                next,
                UiState::new("test::mock"),
                ModelClient::new(Default::default()).unwrap(),
                Vec::new(),
            ),
        );
        assert!(tabs.restore(&id, &mut session, &mut state, &mut model, &mut history));
        assert!(tabs.close_active(&mut session, &mut state, &mut model, &mut history));
        assert_ne!(session.id(), id);
        let mut runs = Runs::from([(id.clone(), run)]);
        assert!(tabs.prune_closed(&runs).is_empty());
        assert!(session.store.acquire(&id).await.is_err());
        release.send(()).unwrap();
        let mut sync = session_sync::SessionSync::default();
        time::timeout(Duration::from_secs(2), async {
            while runs.contains_key(&id) {
                poll(&mut runs, &mut tabs, &mut sync).await.unwrap();
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(!token.is_cancelled());
        assert!(!tabs.contains(&id));
        assert!(state.transcript.is_empty());
        assert_eq!(
            session
                .store
                .load(&id)
                .await
                .unwrap()
                .runs
                .last()
                .unwrap()
                .status,
            "completed"
        );
        assert!(session.store.acquire(&id).await.is_ok());
    }

    #[tokio::test]
    async fn local_results_and_history_replay_stay_with_their_background_tab() {
        let dir = tempfile::tempdir().unwrap();
        let options = options(dir.path());
        let mut setup = crate::prepare_tui(&options, dir.path(), None)
            .await
            .unwrap();
        setup
            .session
            .begin("saved prompt".into(), "test::mock".into())
            .await
            .unwrap();
        setup
            .session
            .execution_journal()
            .unwrap()
            .record(AgentEvent::Model(StreamEvent::TextDelta(
                "saved answer".into(),
            )));
        setup
            .session
            .finish(
                "completed",
                Some(vec![ChatMessage::assistant("saved answer")]),
            )
            .await
            .unwrap();
        let id = setup.session.id().to_owned();
        let mut run = runtime(&setup);
        run.history_replay = Some(crate::session::HistoryReplay::default());
        let (release, wait) = oneshot::channel();
        run.local_task = Some(tokio::spawn(async {
            wait.await.unwrap();
            Ok("undo from old tab".into())
        }));
        let mut session = setup.session;
        let mut model = setup.resources.model;
        let mut state = UiState::new("test::mock");
        state.resume_loading = Some("正在显示会话历史…".into());
        let mut history = Vec::new();
        let mut tabs = tabs::Tabs::new(&session);
        let next = session.prepare_new("test::mock".into()).await.unwrap();
        tabs.activate(
            &mut session,
            &mut state,
            &mut model,
            &mut history,
            (
                next,
                UiState::new("test::mock"),
                ModelClient::new(Default::default()).unwrap(),
                Vec::new(),
            ),
        );
        let mut runs = Runs::from([(id.clone(), run)]);
        let mut sync = session_sync::SessionSync::default();
        release.send(()).unwrap();
        time::timeout(Duration::from_secs(2), async {
            while runs[&id].busy() {
                poll(&mut runs, &mut tabs, &mut sync).await.unwrap();
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(state.transcript.is_empty());
        assert!(tabs.restore(&id, &mut session, &mut state, &mut model, &mut history));
        assert!(state.resume_loading.is_none());
        let transcript = format!("{:?}", state.transcript);
        assert!(
            transcript.contains("saved prompt") && transcript.contains("saved answer"),
            "{transcript}"
        );
        assert!(transcript.contains("undo from old tab"), "{transcript}");
    }

    #[tokio::test]
    async fn shutdown_cancels_background_tasks_and_releases_the_writer() {
        let dir = tempfile::tempdir().unwrap();
        let options = options(dir.path());
        let mut setup = crate::prepare_tui(&options, dir.path(), None)
            .await
            .unwrap();
        let id = setup.session.id().to_owned();
        let (run, _release, _) = running(&mut setup, "cancelled fixture").await;
        let token = run.cancellation.clone().unwrap();
        let mut session = setup.session;
        let mut model = setup.resources.model;
        let mut state = UiState::new("test::mock");
        state.submit("cancelled fixture".into());
        let mut history = Vec::new();
        let mut tabs = tabs::Tabs::new(&session);
        let next = session.prepare_new("test::mock".into()).await.unwrap();
        tabs.activate(
            &mut session,
            &mut state,
            &mut model,
            &mut history,
            (
                next,
                UiState::new("test::mock"),
                ModelClient::new(Default::default()).unwrap(),
                Vec::new(),
            ),
        );
        let mut runs = Runs::from([(id.clone(), run)]);
        shutdown(&mut runs, &mut tabs).await.unwrap();
        assert!(token.is_cancelled());
        assert!(runs[&id].task.is_none());
        assert_eq!(
            session
                .store
                .load(&id)
                .await
                .unwrap()
                .runs
                .last()
                .unwrap()
                .status,
            "cancelled"
        );
        assert!(session.store.acquire(&id).await.is_ok());
    }
}
