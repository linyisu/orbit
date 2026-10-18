use orbit_agent::{
    events::AgentEvent,
    model::{CompletionFuture, MockModel, StreamFuture},
    tool::ClosureTool,
};
use orbit_config::test_support::{TestConfig, TestModel, TestProvider};
use orbit_model::{error::ModelError, types::ChatCompletion};
use tokio::sync::Notify;

use super::*;
#[path = "tests/native_controls.rs"]
mod native_controls;

fn response(message: ChatMessage) -> ChatCompletion {
    ChatCompletion {
        id: None,
        model: Some("orbit-model".into()),
        message,
        finish_reason: Some("stop".into()),
        usage: None,
    }
}

#[derive(Clone)]
struct GatedModel {
    requests: Arc<std::sync::Mutex<Vec<ChatRequest>>>,
    gate: Arc<Semaphore>,
    changed: Arc<Notify>,
    answer: Arc<str>,
}

impl GatedModel {
    fn new() -> Self {
        Self {
            requests: Default::default(),
            gate: Arc::new(Semaphore::new(0)),
            changed: Arc::new(Notify::new()),
            answer: "Orbit findings".into(),
        }
    }

    async fn received(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let notified = self.changed.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if self.requests.lock().unwrap().len() >= count {
                    return;
                }
                notified.await;
            }
        })
        .await
        .unwrap();
    }
}

impl ModelBackend for GatedModel {
    fn complete<'a>(
        &'a self,
        request: ChatRequest,
        token: CancellationToken,
    ) -> CompletionFuture<'a> {
        Box::pin(async move {
            MockModel::new([response(ChatMessage::assistant(self.answer.to_string()))])
                .complete(request, token)
                .await
        })
    }

    fn stream<'a>(&'a self, request: ChatRequest, token: CancellationToken) -> StreamFuture<'a> {
        Box::pin(async move {
            self.requests
                .lock()
                .unwrap()
                .push(request.clone());
            self.changed.notify_waiters();
            tokio::select! {
                _ = token.cancelled() => return Err(ModelError::Cancelled),
                permit = self.gate.acquire() => permit.unwrap().forget(),
            }
            MockModel::new([response(ChatMessage::assistant(self.answer.to_string()))])
                .stream(request, token)
                .await
        })
    }
}

async fn fixture(
    dir: &std::path::Path,
    model: GatedModel,
    limits: SubagentConfig,
) -> (Arc<Service>, SessionWriter) {
    let path = dir.join("config.toml");
    let mut provider = TestProvider::new("orbit").models([TestModel::new("orbit-model")
        .reasoning(
            orbit_model::client::ApiFormat::OpenaiChatCompletions,
            &[
                orbit_model::types::ReasoningEffort::Low,
                orbit_model::types::ReasoningEffort::High,
            ],
            orbit_model::types::ReasoningEffort::Low,
        )]);
    provider.spec.max_retries = 0;
    let mut fixture = TestConfig::new().provider(provider);
    fixture.config.auto_compact = false;
    fixture.write(&path).unwrap();
    let mut parent_model = fixture
        .config
        .select_model("orbit::orbit-model")
        .unwrap();
    parent_model.config.reasoning_effort = Some(ReasoningEffort::High);
    let store = Store::open(dir.join("orbit.db"))
        .await
        .unwrap();
    let mut parent = store
        .create(dir.display().to_string(), "orbit::orbit-model".into())
        .await
        .unwrap();

    parent
        .set_model_profile("orbit::orbit-model".into(), Some("high".into()))
        .await
        .unwrap();
    parent
        .begin("private parent context".into(), "orbit::orbit-model".into())
        .await
        .unwrap();
    let service = Arc::new(Service {
        tool_policy: None,
        approvals: None,
        user_input: None,
        parent: parent.id().into(),
        store,
        config_path: path,
        auth_directory: dir.join("auth"),
        instructions: Default::default(),
        jobs: BackgroundJobs::with_capacity(limits.max_concurrent).unwrap(),
        slots: Arc::new(Semaphore::new(limits.max_concurrent)),
        limits,
        allowed_tools: ["ReadFile".into(), "ReadHistory".into()]
            .into_iter()
            .collect(),
        active: Mutex::new(HashMap::new()),
        backend: Arc::new(move |_| Arc::new(model.clone())),
        parent_model: std::sync::RwLock::new(parent_model),
    });
    (service, parent)
}

fn install(service: &Arc<Service>) -> ToolRegistry {
    let mut tools = ToolRegistry::new();
    tools.add_task_group(service.jobs.clone());
    for action in [
        Action::Spawn,
        Action::Send,
        Action::Wait,
        Action::Read,
        Action::Cancel,
    ] {
        tools.register(AgentTool {
            service: service.clone(),
            action,
        });
    }

    tools
}

async fn spawn(service: &Arc<Service>, name: &str, token: CancellationToken) -> Value {
    let call = ToolCall::function(
        name,
        "SpawnAgent",
        json!({
            "task": format!("inspect {name}")
        }),
    );
    serde_json::from_str(
        &service
            .clone()
            .execute(
                Action::Spawn,
                call.clone(),
                call.arguments_json().unwrap(),
                token,
            )
            .await
            .unwrap()
            .content,
    )
    .unwrap()
}

async fn read(service: &Arc<Service>, args: Value) -> Result<Value> {
    let output = service
        .clone()
        .execute(
            Action::Read,
            ToolCall::function("orbit-read", "ReadAgent", args.clone()),
            args,
            CancellationToken::new(),
        )
        .await?;
    Ok(serde_json::from_str(&output.content)?)
}

#[tokio::test]
async fn inherited_routes_survive_catalog_changes_and_respect_explicit_child_switches() {
    let dir = tempfile::tempdir().unwrap();
    let model = GatedModel::new();
    model.gate.add_permits(4);
    let (mut service, _parent) = fixture(dir.path(), model.clone(), Default::default()).await;
    {
        let mut inherited = service.parent_model.write().unwrap();
        inherited.config.base_url = "http://127.0.0.1:9/synthetic-route".into();
        inherited.config.api_key = Some("synthetic-parent-key".into());
    }
    let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
    let capture = captured.clone();
    let backend = model.clone();
    Arc::get_mut(&mut service)
        .unwrap()
        .backend = Arc::new(move |client| {
        capture
            .lock()
            .unwrap()
            .push(client.config().clone());
        Arc::new(backend.clone())
    });
    let inherited = service
        .parent_model
        .read()
        .unwrap()
        .clone();
    TestConfig::new()
        .provider(TestProvider::new("other").models([TestModel::new("other-model")]))
        .write(&service.config_path)
        .unwrap();

    let first = spawn(&service, "inherited", CancellationToken::new()).await;
    assert_eq!(first["model"], inherited.qualified_name());
    service
        .jobs
        .wait(
            &[first["job_id"].as_str().unwrap().into()],
            true,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let next = TestModel::new("next-model").option("next");
    Control(service.clone()).set_parent_model(next.clone());
    let sent = service
        .send(
            ToolCall::function("followup", "SendAgentMessage", json!({})),
            SendArgs {
                agent_id: first["agent_id"]
                    .as_str()
                    .unwrap()
                    .into(),
                message: "Check one more function".into(),
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let SendResult::Launched(LaunchResult::Started { job_id, .. }) = sent else {
        panic!("expected a new child run");
    };
    service
        .jobs
        .wait(&[job_id], true, CancellationToken::new())
        .await
        .unwrap();
    let second = spawn(&service, "new-profile", CancellationToken::new()).await;
    service
        .jobs
        .wait(
            &[second["job_id"]
                .as_str()
                .unwrap()
                .into()],
            true,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    {
        let routes = captured.lock().unwrap();
        assert_eq!(routes.len(), 3);
        for route in &routes[..2] {
            assert_eq!(route.base_url, inherited.config.base_url);
            assert_eq!(route.model, inherited.config.model);
            assert_eq!(route.api_key, inherited.config.api_key);
            assert_eq!(route.reasoning_effort, Some(ReasoningEffort::High));
        }
        assert_eq!(routes[2].model, next.config.model);
    }
    assert_eq!(second["model"], next.qualified_name());
    {
        let writer = service
            .store
            .acquire(first["agent_id"].as_str().unwrap())
            .await
            .unwrap();
        writer
            .set_model_profile("other::other-model".into(), None)
            .await
            .unwrap();
    }
    let args = json!({
        "agent_id": first["agent_id"], "message": "Check the selected profile"
    });
    let sent = service
        .clone()
        .execute(
            Action::Send,
            ToolCall::function("changed-profile", "SendAgentMessage", args.clone()),
            args,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let sent: Value = serde_json::from_str(&sent.content).unwrap();
    service
        .jobs
        .wait(
            &[sent["job_id"].as_str().unwrap().into()],
            true,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(sent["model"], "other::other-model");
    {
        let routes = captured.lock().unwrap();
        assert_eq!(routes.len(), 4);
        assert_eq!(routes[3].model, "other-model");
        assert!(routes[3].api_key.is_none());
    }

    let args = json!({"task": "Inspect", "model": inherited.qualified_name()});
    let error = service
        .clone()
        .execute(
            Action::Spawn,
            ToolCall::function("explicit", "SpawnAgent", args.clone()),
            args,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("not configured")
    );
    assert_eq!(captured.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn final_results_are_direct_unicode_pages_with_stable_completion_cursors() {
    let dir = tempfile::tempdir().unwrap();
    let answer = "Orbit 中文报告🙂\n".repeat(2_000);
    let mut model = GatedModel::new();
    model.answer = answer.clone().into();
    let (service, parent) = fixture(dir.path(), model.clone(), Default::default()).await;
    let receipt = spawn(&service, "orbit-report", CancellationToken::new()).await;
    let id = receipt["agent_id"].as_str().unwrap();
    let pending = read(
        &service,
        json!({
            "agent_id": id
        }),
    )
    .await
    .unwrap();
    assert_eq!(pending["result_available"], false);
    assert_eq!(pending["content"], "");
    model.gate.add_permits(1);
    let finished = service
        .jobs
        .wait(
            &[receipt["job_id"]
                .as_str()
                .unwrap()
                .into()],
            true,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let completion = finished[0].result.as_ref().unwrap();
    assert_eq!(completion["truncated"], true);
    assert!(
        completion["result_message_id"]
            .as_i64()
            .is_some()
    );
    assert_eq!(completion["total_characters"], answer.chars().count());
    let page = read(
        &service,
        json!({
            "agent_id": id
        }),
    )
    .await
    .unwrap();
    assert_eq!(page["view"], "result");
    assert_eq!(page["message_id"], completion["result_message_id"]);
    assert_eq!(
        page["content"].as_str().unwrap(),
        answer
            .chars()
            .take(16_000)
            .collect::<String>()
    );

    let mut reconstructed = completion["summary"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut cursor = completion["read_more"].clone();
    while !cursor.is_null() {
        let page = read(&service, cursor).await.unwrap();
        assert_eq!(page["message_id"], completion["result_message_id"]);
        reconstructed.push_str(page["content"].as_str().unwrap());
        cursor = page["next_read"].clone();
    }

    assert_eq!(reconstructed, answer);
    let transcript = read(
        &service,
        json!({
            "agent_id": id,
            "view": "transcript"
        }),
    )
    .await
    .unwrap();
    assert_eq!(transcript["view"], "transcript");
    assert!(
        transcript["content"]
            .as_str()
            .unwrap()
            .contains("inspect orbit-report")
    );
    let next = read(
        &service,
        json!({
            "agent_id": id,
            "after_message_id": transcript["message_id"]
        }),
    )
    .await
    .unwrap();

    assert_eq!(next["view"], "transcript");
    assert_eq!(next["message_id"], completion["result_message_id"]);
    assert!(
        read(
            &service,
            json!({
                "agent_id": id,
                "view": "result",
                "message_id": transcript["message_id"]
            })
        )
        .await
        .is_err()
    );
    assert!(
        read(
            &service,
            json!({
                "agent_id": id,
                "offset": answer.chars().count() + 1
            })
        )
        .await
        .is_err()
    );
    assert!(
        read(
            &service,
            json!({
                "agent_id": parent.id()
            })
        )
        .await
        .is_err()
    );

    let continued = json!({
        "agent_id": id,
        "message": "再次核对 Orbit"
    });
    service
        .clone()
        .execute(
            Action::Send,
            ToolCall::function("orbit-continue", "SendAgentMessage", continued.clone()),
            continued,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        read(
            &service,
            json!({
                "agent_id": id
            })
        )
        .await
        .unwrap()["result_available"],
        false
    );
    assert_eq!(
        read(
            &service,
            json!({
                "agent_id": id,
                "view": "result",
                "message_id": completion["result_message_id"],
                "offset": 6_000
            })
        )
        .await
        .unwrap()["message_id"],
        completion["result_message_id"]
    );
    service.jobs.cancel(&[]).unwrap();
    service
        .jobs
        .wait(&[], true, CancellationToken::new())
        .await
        .unwrap();
}

#[tokio::test]
async fn eight_children_overlap_while_parent_works_and_waits_without_polling() {
    let dir = tempfile::tempdir().unwrap();
    let child_model = GatedModel::new();
    let (service, mut parent) = fixture(dir.path(), child_model.clone(), Default::default()).await;
    let mut tools = install(&service);
    let did_work = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = did_work.clone();
    let jobs = service.jobs.clone();
    tools.register(ClosureTool::new(
        "parent_work",
        "",
        json!({}),
        move |_, _| {
            let jobs = jobs.clone();
            let flag = flag.clone();
            async move {
                assert_eq!(
                    jobs.snapshots(&[], false)
                        .unwrap()
                        .iter()
                        .filter(|j| j.status == JobStatus::Running)
                        .count(),
                    8
                );
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok::<_, ToolError>("independent work done".into())
            }
        },
    ));
    let mut calls = (0..8)
        .map(|n| {
            ToolCall::function(
                format!("child-{n}"),
                "SpawnAgent",
                json!({
                    "task": format!("inspect Orbit module {n}")
                }),
            )
        })
        .collect::<Vec<_>>();

    calls.push(ToolCall::function("work", "parent_work", json!({})));
    let model = MockModel::new(
        [
            response(ChatMessage::assistant_tool_calls(calls)),
            response(ChatMessage::assistant("waiting")),
        ]
        .into_iter()
        .chain((0..8).map(|_| response(ChatMessage::assistant("review finished")))),
    );
    let agent = Agent::new(model.clone(), tools, AgentConfig::default())
        .with_execution_journal(parent.execution_journal().unwrap());
    let (sender, mut events) = tokio::sync::mpsc::channel(256);
    let run = tokio::spawn(async move {
        agent
            .run_with_agent_events(
                ChatRequest::new(vec![ChatMessage::user("parent-only-secret")]),
                CancellationToken::new(),
                sender,
            )
            .await
    });

    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(event) = events.recv().await {
            if matches!(event, AgentEvent::WaitingForTasks { .. }) {
                return;
            }
        }

        panic!("parent never waited");
    })
    .await
    .unwrap();
    child_model.received(8).await;
    assert!(did_work.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(model.requests().len(), 2);
    assert!(!run.is_finished());
    for request in child_model
        .requests
        .lock()
        .unwrap()
        .iter()
    {
        assert!(
            !serde_json::to_string(&request.messages)
                .unwrap()
                .contains("parent-only-secret")
        );
        assert_eq!(request.reasoning_effort, Some(ReasoningEffort::High));
        let names = request
            .tools
            .iter()
            .map(|t| t.function.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(names, ["ReadFile", "ReadHistory"]);
    }

    child_model.gate.add_permits(8);
    let result = tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    assert!(model.requests().len() >= 3 && model.requests().len() <= 10);
    assert_eq!(
        service
            .store
            .subagents(parent.id())
            .await
            .unwrap()
            .len(),
        8
    );
    assert!(
        service
            .store
            .pending_agent_results(parent.id())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        result
            .history
            .iter()
            .filter(|m| matches!(
                m,
                ChatMessage::Tool { tool_call_id, .. } if tool_call_id.starts_with("child-")
            ))
            .count(),
        8
    );
    parent
        .finish(
            &orbit_store::types::RunCompletion::Completed {
                history: result.history,
            },
            Vec::new(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn running_input_and_finished_continuation_keep_identity_and_canonical_history() {
    let dir = tempfile::tempdir().unwrap();
    let model = GatedModel::new();
    let (service, mut parent) = fixture(dir.path(), model.clone(), Default::default()).await;
    let receipt = spawn(&service, "orbit-explore", CancellationToken::new()).await;
    let id = receipt["agent_id"].as_str().unwrap();
    let job = receipt["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    model.received(1).await;
    let args = json!({
        "agent_id": id,
        "message": "补充检查 Unicode"
    });
    let call = ToolCall::function("input", "SendAgentMessage", args.clone());
    assert_eq!(
        serde_json::from_str::<Value>(
            &service
                .clone()
                .execute(Action::Send, call, args, CancellationToken::new())
                .await
                .unwrap()
                .content
        )
        .unwrap()["status"],
        "queued"
    );
    model.gate.add_permits(2);
    service
        .jobs
        .wait(&[job], true, CancellationToken::new())
        .await
        .unwrap();
    model.received(2).await;
    assert!(
        serde_json::to_string(&model.requests.lock().unwrap()[1].messages)
            .unwrap()
            .contains("补充检查 Unicode")
    );
    assert!(
        service
            .store
            .pending_agent_messages(id)
            .await
            .unwrap()
            .is_empty()
    );
    parent
        .finish(
            &orbit_store::types::RunCompletion::Completed {
                history: vec![ChatMessage::user("private parent context")],
            },
            Vec::new(),
        )
        .await
        .unwrap();
    let parent_before = service
        .store
        .load(parent.id())
        .await
        .unwrap();
    let args = json!({
        "agent_id": id,
        "message": "再次核对 Orbit"
    });
    let call = ToolCall::function("continue", "SendAgentMessage", args.clone());
    let receipt: Value = serde_json::from_str(
        &service
            .clone()
            .execute(Action::Send, call, args, CancellationToken::new())
            .await
            .unwrap()
            .content,
    )
    .unwrap();

    assert_eq!(receipt["agent_id"], id);
    model.gate.add_permits(1);
    service
        .jobs
        .wait(
            &[receipt["job_id"]
                .as_str()
                .unwrap()
                .into()],
            true,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let parent_after = service
        .store
        .load(parent.id())
        .await
        .unwrap();
    assert_eq!(parent_after.runs.len(), parent_before.runs.len());
    assert_eq!(parent_after.history, parent_before.history);
    assert_eq!(parent_after.runs.last().unwrap().status, "completed");
    let (_, messages) = service
        .store
        .canonical_messages(id, None, 100)
        .await
        .unwrap();

    assert_eq!(
        messages
            .iter()
            .filter(|m| matches!(
                &m.message,
                ChatMessage::User { content, .. } if content == "再次核对 Orbit"
            ))
            .count(),
        1
    );
    assert_eq!(
        service
            .store
            .load(id)
            .await
            .unwrap()
            .runs
            .len(),
        2
    );
    let args = json!({
        "agent_id": id,
        "length": 1
    });
    let call = ToolCall::function("read", "ReadAgent", args.clone());
    let read: Value = serde_json::from_str(
        &service
            .clone()
            .execute(Action::Read, call, args, CancellationToken::new())
            .await
            .unwrap()
            .content,
    )
    .unwrap();

    assert_eq!(
        read["content"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        1
    );
    assert_eq!(read["next_offset"], 1);
}

#[tokio::test]
async fn configurable_limit_and_cancellation_isolate_siblings() {
    let dir = tempfile::tempdir().unwrap();
    let model = GatedModel::new();
    let (service, _parent) = fixture(
        dir.path(),
        model.clone(),
        SubagentConfig {
            max_concurrent: 12,
            ..Default::default()
        },
    )
    .await;
    let token = CancellationToken::new();
    let mut receipts = Vec::new();
    for n in 0..12 {
        receipts.push(spawn(&service, &format!("orbit-{n}"), token.clone()).await);
    }
    model.received(12).await;
    let call = ToolCall::function(
        "overflow",
        "SpawnAgent",
        json!({
            "task": "overflow"
        }),
    );
    assert!(
        service
            .clone()
            .execute(
                Action::Spawn,
                call.clone(),
                call.arguments_json().unwrap(),
                token.clone()
            )
            .await
            .is_err()
    );
    assert_eq!(
        service
            .store
            .subagents(&service.parent)
            .await
            .unwrap()
            .len(),
        12
    );
    let job = receipts[0]["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    service
        .jobs
        .cancel(std::slice::from_ref(&job))
        .unwrap();
    service
        .jobs
        .wait(&[job], true, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        service
            .jobs
            .snapshots(&[], false)
            .unwrap()
            .iter()
            .filter(|j| j.status == JobStatus::Running)
            .count(),
        11
    );
    token.cancel();
    service
        .jobs
        .wait(&[], true, CancellationToken::new())
        .await
        .unwrap();
    assert!(
        service
            .jobs
            .snapshots(&[], false)
            .unwrap()
            .iter()
            .all(|j| j.status == JobStatus::Cancelled)
    );
    for agent in service
        .store
        .subagents(&service.parent)
        .await
        .unwrap()
    {
        assert_eq!(
            service
                .store
                .load(&agent.id)
                .await
                .unwrap()
                .runs
                .last()
                .unwrap()
                .status,
            "cancelled"
        );
    }
}

#[tokio::test]
async fn recovery_delivers_finished_results_once_and_never_replays_interrupted_work() {
    let dir = tempfile::tempdir().unwrap();
    let model = GatedModel::new();
    let (service, mut parent) = fixture(dir.path(), model.clone(), Default::default()).await;
    let finished = spawn(&service, "orbit-finished", CancellationToken::new()).await;
    model.gate.add_permits(1);
    service
        .jobs
        .wait(
            &[finished["job_id"]
                .as_str()
                .unwrap()
                .into()],
            true,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let mut interrupted = service
        .store
        .create_subagent(
            &service.parent,
            "orbit::orbit-model".into(),
            "review".into(),
            "interrupted review".into(),
        )
        .await
        .unwrap();

    interrupted
        .begin("review".into(), "orbit::orbit-model".into())
        .await
        .unwrap();
    service
        .store
        .begin_agent_invocation(
            &service.parent,
            interrupted.id(),
            ToolCall::function(
                "interrupted",
                "SpawnAgent",
                json!({
                    "task": "review"
                }),
            ),
        )
        .await
        .unwrap();
    let interrupted_id = interrupted.id().to_owned();
    let project = service
        .store
        .session(&interrupted_id)
        .await
        .unwrap()
        .project_id;
    let task = service
        .store
        .create_task(
            &project,
            orbit_store::tasks::NewTask::new("interrupted work"),
        )
        .await
        .unwrap();
    service
        .store
        .claim_task(&project, task.id, &interrupted_id)
        .await
        .unwrap();
    let recovered = Service {
        tool_policy: None,
        approvals: None,
        user_input: None,
        jobs: BackgroundJobs::default(),
        active: Mutex::new(HashMap::new()),
        store: service.store.clone(),
        parent: service.parent.clone(),
        config_path: service.config_path.clone(),
        auth_directory: service.auth_directory.clone(),
        instructions: Default::default(),
        limits: Default::default(),
        allowed_tools: service.allowed_tools.clone(),
        slots: Arc::new(Semaphore::new(8)),
        backend: service.backend.clone(),
        parent_model: std::sync::RwLock::new(
            service
                .parent_model
                .read()
                .unwrap()
                .clone(),
        ),
    };

    drop(interrupted);
    recovered.recover().await.unwrap();
    let released = service
        .store
        .tasks(&project, false)
        .await
        .unwrap()
        .remove(0);
    assert_eq!(released.owner_session_id, None);
    assert_eq!(released.status, orbit_store::types::TaskStatus::Pending);
    service
        .store
        .claim_task(&project, task.id, parent.id())
        .await
        .unwrap();
    assert_eq!(model.requests.lock().unwrap().len(), 1);
    assert_eq!(
        recovered
            .store
            .load(&interrupted_id)
            .await
            .unwrap()
            .runs[0]
            .status,
        "interrupted"
    );
    let restored = recovered
        .jobs
        .snapshots(&[], true)
        .unwrap();
    // Restored results belong to the next Agent generation.
    assert!(restored.is_empty());
    let recovered = Arc::new(recovered);
    let root = MockModel::new([response(ChatMessage::assistant("collected"))]);
    let result = Agent::new(root.clone(), install(&recovered), AgentConfig::default())
        .with_execution_journal(parent.execution_journal().unwrap())
        .run(
            ChatRequest::new(vec![ChatMessage::user("continue")]),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let context = serde_json::to_string(&root.requests()[0].messages).unwrap();
    assert!(context.contains("Orbit findings"));
    assert!(context.contains("interrupted"));
    assert!(
        service
            .store
            .pending_agent_results(&service.parent)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(model.requests.lock().unwrap().len(), 1);
    parent
        .finish(
            &orbit_store::types::RunCompletion::Completed {
                history: result.history,
            },
            Vec::new(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn child_views_remain_read_only_when_delegation_is_disabled() {
    let dir = tempfile::tempdir().unwrap();
    let model = GatedModel::new();
    let (service, _parent) = fixture(dir.path(), model, Default::default()).await;
    let child = service
        .store
        .create_subagent(
            &service.parent,
            "orbit::orbit-model".into(),
            "review".into(),
            "review".into(),
        )
        .await
        .unwrap();
    let mut tools = ToolRegistry::new();
    for (name, capabilities) in [
        ("ReadFile", orbit_agent::tool::ToolCapabilities::READ),
        ("WriteFile", orbit_agent::tool::ToolCapabilities::WRITE),
        ("Exec", orbit_agent::tool::ToolCapabilities::COMMAND),
        (
            "SpawnAgent",
            orbit_agent::tool::ToolCapabilities::AGENT_CONTROL,
        ),
        ("ListMemories", orbit_agent::tool::ToolCapabilities::READ),
        (
            "MCP_Orbit_Write",
            orbit_agent::tool::ToolCapabilities::UNKNOWN,
        ),
    ] {
        tools.register(
            ClosureTool::new(name, "", json!({}), |_, _| async {
                Ok::<_, ToolError>("unused".into())
            })
            .with_capabilities(capabilities),
        );
    }

    let parent_model = service
        .parent_model
        .read()
        .unwrap()
        .clone();
    register(
        &mut tools,
        service.store.clone(),
        child.id().into(),
        service.config_path.clone(),
        service.auth_directory.clone(),
        Default::default(),
        SubagentConfig {
            enabled: false,
            ..Default::default()
        },
        None,
        None,
        parent_model,
    )
    .await
    .unwrap();
    assert_eq!(
        tools
            .definitions()
            .iter()
            .map(|t| t.function.name.as_str())
            .collect::<Vec<_>>(),
        ["ListMemories", "ReadFile"]
    );
}

#[test]
fn validates_subagent_limits_and_defaults_to_eight() {
    assert_eq!(SubagentConfig::default().max_concurrent, 8);
    assert_eq!(SubagentConfig::default().max_turns, 0);
    for limit in [0, 60, 300, 1_000] {
        assert!(
            SubagentConfig {
                max_turns: limit,
                max_tool_calls: limit,
                timeout_seconds: 0,
                ..Default::default()
            }
            .validate()
            .is_ok()
        );
    }

    for limit in [1, 8, 32, 128] {
        assert!(
            SubagentConfig {
                max_concurrent: limit,
                ..Default::default()
            }
            .validate()
            .is_ok()
        );
    }

    for limit in [0, 129] {
        assert!(
            SubagentConfig {
                max_concurrent: limit,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }

    assert!(
        SubagentConfig {
            timeout_seconds: 86_401,
            ..Default::default()
        }
        .validate()
        .is_err()
    );
}

#[tokio::test]
async fn delegation_effects_follow_role_and_unknown_roles_cannot_bypass_plan() {
    use orbit_agent::tool::ToolCapabilities;
    let directory = tempfile::tempdir().unwrap();
    let (service, _parent) = fixture(directory.path(), GatedModel::new(), Default::default()).await;
    let tool = AgentTool {
        service,
        action: Action::Spawn,
    };
    for args in [
        json!({}),
        json!({
            "role": "explore"
        }),
        json!({
            "role": "review"
        }),
    ] {
        assert!(
            tool.call_capabilities(&ToolCall::function("spawn", "SpawnAgent", args))
                .in_plan()
        );
    }
    for args in [
        json!({
            "role": "develop"
        }),
        json!({
            "role": "unknown"
        }),
    ] {
        assert_eq!(
            tool.call_capabilities(&ToolCall::function("spawn", "SpawnAgent", args)),
            ToolCapabilities::WRITE
        );
    }
}

#[tokio::test]
async fn progress_deadline_wakes_parent_without_terminating_the_child() {
    let dir = tempfile::tempdir().unwrap();
    let child_model = GatedModel::new();
    let (service, _parent) = fixture(
        dir.path(),
        child_model.clone(),
        SubagentConfig {
            timeout_seconds: 1,
            ..Default::default()
        },
    )
    .await;
    let receipt = spawn(&service, "orbit-progress", CancellationToken::new()).await;
    child_model.received(1).await;
    let agent = service
        .store
        .owned_subagent(&service.parent, receipt["agent_id"].as_str().unwrap())
        .await
        .unwrap();
    let reminder = service
        .progress_reminder(
            &agent,
            receipt["run_id"].as_i64().unwrap(),
            Duration::from_secs(1),
        )
        .await;

    assert_eq!(reminder["status"], "running");
    assert!(
        reminder["latest_activity"]["message_preview"]
            .as_str()
            .unwrap()
            .contains("inspect orbit-progress")
    );
    let activity = read(&service, reminder["inspect"].clone())
        .await
        .unwrap();
    assert_eq!(
        activity["message_id"],
        reminder["latest_activity"]["message_id"]
    );
    assert!(
        activity["content"]
            .as_str()
            .unwrap()
            .contains("inspect orbit-progress")
    );
    let waiting = tokio::time::timeout(
        Duration::from_secs(5),
        service
            .jobs
            .wait(&[], true, CancellationToken::new()),
    )
    .await
    .unwrap()
    .unwrap();

    assert_eq!(waiting[0].status, JobStatus::Running);
    assert_eq!(
        service
            .store
            .session_revision(receipt["agent_id"].as_str().unwrap())
            .await
            .unwrap()
            .unwrap()
            .status,
        "running"
    );
    child_model.gate.add_permits(1);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if service
                .jobs
                .snapshots(&[], true)
                .unwrap()[0]
                .status
                == JobStatus::Completed
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn development_writes_use_parent_approvals_and_capability_ceiling() {
    let directory = tempfile::tempdir().unwrap();
    let (mut service, _parent) =
        fixture(directory.path(), GatedModel::new(), Default::default()).await;
    let args = json!({
        "task": "write a test file",
        "role": "develop",
        "workspace_strategy": "shared"
    });
    let call = ToolCall::function("develop", "SpawnAgent", args.clone());
    assert!(
        service
            .clone()
            .execute(
                Action::Spawn,
                call.clone(),
                args.clone(),
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    let model = MockModel::new([
        response(ChatMessage::assistant_tool_calls(vec![ToolCall::function(
            "write",
            "WriteFile",
            json!({
                "path": "child.txt",
                "content": "verified child"
            }),
        )])),
        response(ChatMessage::assistant("finished")),
    ]);
    let (approvals, mut receiver) = orbit_tools::approval::channel();
    let inner = Arc::get_mut(&mut service).unwrap();
    inner.approvals = Some(approvals);
    inner
        .allowed_tools
        .insert("WriteFile".into());
    let backend = model.clone();
    inner.backend = Arc::new(move |_| Arc::new(backend.clone()));
    let output = service
        .clone()
        .execute(Action::Spawn, call, args, CancellationToken::new())
        .await
        .unwrap();
    let receipt: Value = serde_json::from_str(&output.content).unwrap();
    let approval = tokio::time::timeout(Duration::from_secs(5), receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        !directory
            .path()
            .join("child.txt")
            .exists()
    );
    approval.decision.send(true).unwrap();
    let results = service
        .jobs
        .wait(
            &[receipt["job_id"]
                .as_str()
                .unwrap()
                .into()],
            true,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(results[0].status, JobStatus::Completed);
    assert_eq!(
        std::fs::read_to_string(directory.path().join("child.txt")).unwrap(),
        "verified child"
    );
    for request in model.requests().iter() {
        let names: Vec<_> = request
            .tools
            .iter()
            .map(|tool| tool.function.name.as_str())
            .collect();
        assert!(names.contains(&"WriteFile"));
        assert!(
            !names
                .iter()
                .any(|name| ["Exec", "RequestAccess", "SpawnAgent"].contains(name))
        );
    }
}

#[tokio::test]
async fn failed_development_preparation_rolls_back_and_unstarted_children_cannot_continue() {
    let directory = tempfile::tempdir().unwrap();
    let model = GatedModel::new();
    let (mut service, _parent) = fixture(directory.path(), model.clone(), Default::default()).await;
    let (approvals, _receiver) = orbit_tools::approval::channel();
    let inner = Arc::get_mut(&mut service).unwrap();
    inner.approvals = Some(approvals);
    inner
        .allowed_tools
        .insert("WriteFile".into());
    let args = json!({"task": "prepare development workspace", "role": "develop"});
    let error = service
        .clone()
        .execute(
            Action::Spawn,
            ToolCall::function("startup", "SpawnAgent", args.clone()),
            args,
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("startup failed")
    );
    assert!(
        service
            .store
            .subagents(&service.parent)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        model
            .requests
            .lock()
            .unwrap()
            .is_empty()
    );
    assert!(service.active.lock().await.is_empty());
    let orphan = service
        .store
        .create_subagent(
            &service.parent,
            "orbit::orbit-model".into(),
            "develop".into(),
            "legacy unstarted child".into(),
        )
        .await
        .unwrap();
    let id = orphan.id().to_owned();
    drop(orphan);
    assert!(
        service
            .send(
                ToolCall::function("continue", "SendAgentMessage", json!({})),
                SendArgs {
                    agent_id: id.clone(),
                    message: "continue work".into()
                },
                CancellationToken::new()
            )
            .await
            .is_err()
    );
    assert!(
        service
            .store
            .load(&id)
            .await
            .unwrap()
            .runs
            .is_empty()
    );
    assert!(
        service
            .store
            .pending_agent_messages(&id)
            .await
            .unwrap()
            .is_empty()
    );
    let mut writer = service
        .store
        .acquire(&id)
        .await
        .unwrap();
    writer
        .begin("recorded startup".into(), "orbit::orbit-model".into())
        .await
        .unwrap();
    drop(writer);
    let _error = service
        .rollback_startup(
            directory.path(),
            None,
            &id,
            anyhow::anyhow!("synthetic failure after run creation"),
        )
        .await;
    let snapshot = service.store.load(&id).await.unwrap();
    assert_eq!(snapshot.runs.len(), 1);
    assert_eq!(snapshot.runs[0].status, "interrupted");
}
