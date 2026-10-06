use crossterm::event::{KeyCode, KeyModifiers};
use orbit_model::{client::ModelClient, types::StreamEvent};
use orbit_tools::approval::{ApprovalPreview, ApprovalRequest, McpPreview};
use tokio::{select, sync::oneshot};

use super::super::app::{self, UiState};
use super::*;
use crate::session::SessionState;

fn options(dir: &std::path::Path) -> crate::WorkspaceOptions {
    let config = dir.join("config.toml");
    std::fs::write(
        &config,
        r#"default_provider = "test"
[providers.test]
base_url = "http://127.0.0.1:1/v1"
auto_refresh_models = false
api_key = "test-only"
models = [{ id = "mock" }]
"#,
    )
    .unwrap();
    crate::WorkspaceOptions {
        model: Some("test::mock".into()),
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
    setup: &mut crate::PreparedWorkspace,
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

fn runtime(setup: &crate::PreparedWorkspace) -> BackgroundRun {
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
async fn refreshed_capabilities_reach_parked_tabs_after_their_reply_finishes() {
    let dir = tempfile::tempdir().unwrap();
    let options = options(dir.path());
    let mut setup = crate::prepare_workspace(&options, dir.path(), None)
        .await
        .unwrap();
    let id = setup.session.id().to_owned();
    let (run, release, _) = running(&mut setup, "orbit-refresh").await;
    let mut choices = setup.resources.budget.model_options.clone();
    choices[0].reasoning_efforts = vec![orbit_model::types::ReasoningEffort::High];
    choices[0].config.reasoning_adapter = Some(orbit_model::types::ReasoningAdapter::OpenaiChat);
    choices[0].config.reasoning_effort = Some(orbit_model::types::ReasoningEffort::High);
    let mut session = setup.session;
    let mut model = setup.resources.model;
    let mut state = UiState::new("test::mock");
    state.submit("orbit-refresh".into());
    let mut history = Vec::new();
    let mut tabs = tabs::Tabs::new(&session);
    let next = crate::tui::session_actions::prepare_test_session(&session, "test::mock".into())
        .await
        .unwrap();
    let next_model = model.clone();
    tabs.activate(
        &mut session,
        &mut state,
        &mut model,
        &mut history,
        (next, UiState::new("test::mock"), next_model, Vec::new()),
    );
    let mut runs = Runs::from([(id.clone(), run)]);
    let mut model_options = Vec::new();
    super::super::model_catalog::publish(
        choices,
        &mut state,
        &mut model_options,
        &mut runs,
        &mut tabs,
    );
    let mut sync = session_sync::SessionSync::default();
    for _ in 0..40 {
        tokio::task::yield_now().await;
        poll(&mut runs, &mut tabs, &mut sync, &mut model_options)
            .await
            .unwrap();
    }

    let tab = tabs.parked_mut(&id).unwrap();
    assert_eq!(tab.model.config().reasoning_effort, None);
    assert_eq!(tab.state.reasoning_effort, None);
    assert_eq!(tab.state.model_completions, vec!["test::mock"]);
    release.send(()).unwrap();
    time::timeout(Duration::from_secs(2), async {
        while runs[&id].task.is_some() {
            poll(&mut runs, &mut tabs, &mut sync, &mut model_options)
                .await
                .unwrap();
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let tab = tabs.parked_mut(&id).unwrap();
    assert_eq!(
        tab.model.config().reasoning_effort,
        Some(orbit_model::types::ReasoningEffort::High)
    );
    assert_eq!(
        tab.state.reasoning_effort,
        tab.model.config().reasoning_effort
    );
    assert_eq!(
        tab.state.reasoning_efforts,
        model_options[0].reasoning_efforts
    );
}

#[tokio::test]
async fn two_background_runs_stream_finish_and_keep_resources_and_approvals_separate() {
    let dir = tempfile::tempdir().unwrap();
    let options = options(dir.path());
    let first_dir = dir.path().join("first");
    let second_dir = dir.path().join("second");
    std::fs::create_dir_all(&first_dir).unwrap();
    std::fs::create_dir_all(&second_dir).unwrap();
    let mut first = crate::prepare_workspace(&options, &first_dir, None)
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
    let mut second = crate::prepare_workspace(&options, &second_dir, None)
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
    let third = crate::tui::session_actions::prepare_test_session(&session, "test::mock".into())
        .await
        .unwrap();
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
        poll(&mut runs, &mut tabs, &mut sync, &mut Vec::new())
            .await
            .unwrap();
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
    tabs.sync_ui(&mut state, &session);
    let event = crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
        KeyCode::Char('2'),
        KeyModifiers::ALT,
    ));
    assert_eq!(
        super::super::tab_target(&event, &state),
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
            poll(&mut runs, &mut tabs, &mut sync, &mut Vec::new())
                .await
                .unwrap();
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
    let mut setup = crate::prepare_workspace(&options, dir.path(), None)
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
    let next = crate::tui::session_actions::prepare_test_session(&session, "test::mock".into())
        .await
        .unwrap();
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
    let closing_id = session.id().to_owned();
    assert!(tabs.close_tab(
        &closing_id,
        &mut session,
        &mut state,
        &mut model,
        &mut history
    ));
    assert_ne!(session.id(), id);
    let mut runs = Runs::from([(id.clone(), run)]);
    assert!(tabs.prune_closed(&runs).is_empty());
    assert!(session.store.acquire(&id).await.is_err());
    release.send(()).unwrap();
    let mut sync = session_sync::SessionSync::default();
    time::timeout(Duration::from_secs(2), async {
        while runs.contains_key(&id) {
            poll(&mut runs, &mut tabs, &mut sync, &mut Vec::new())
                .await
                .unwrap();
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
    let mut setup = crate::prepare_workspace(&options, dir.path(), None)
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
    run.history_replay = Some(crate::tui::replay::HistoryReplay::default());
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
    let next = crate::tui::session_actions::prepare_test_session(&session, "test::mock".into())
        .await
        .unwrap();
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
            poll(&mut runs, &mut tabs, &mut sync, &mut Vec::new())
                .await
                .unwrap();
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
    let mut setup = crate::prepare_workspace(&options, dir.path(), None)
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
    let next = crate::tui::session_actions::prepare_test_session(&session, "test::mock".into())
        .await
        .unwrap();
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
