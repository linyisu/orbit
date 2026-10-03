use super::*;
use orbit_agent::{
    events::AgentEvent,
    model::{CompletionFuture, MockModel, StreamFuture},
    tool::ClosureTool,
};
use orbit_model::{error::ModelError, types::ChatCompletion};
use tokio::sync::Notify;

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
}
impl GatedModel {
    fn new() -> Self {
        Self {
            requests: Default::default(),
            gate: Arc::new(Semaphore::new(0)),
            changed: Arc::new(Notify::new()),
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
            MockModel::new([response(ChatMessage::assistant("Orbit findings"))])
                .complete(request, token)
                .await
        })
    }
    fn stream<'a>(&'a self, request: ChatRequest, token: CancellationToken) -> StreamFuture<'a> {
        Box::pin(async move {
            self.requests.lock().unwrap().push(request.clone());
            self.changed.notify_waiters();
            tokio::select! {
                _=token.cancelled()=>return Err(ModelError::Cancelled),
                permit=self.gate.acquire()=>permit.unwrap().forget(),
            }
            MockModel::new([response(ChatMessage::assistant("Orbit findings"))])
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
    std::fs::write(
        &path,
        r#"default_provider = "orbit"
auto_compact = false
[providers.orbit]
base_url = "https://orbit.example.invalid/v1"
api_key = "orbit-test-key"
max_retries = 0
[[providers.orbit.models]]
id = "orbit-model"
reasoning = { adapter = "openai_chat", efforts = ["low", "high"], default = "low" }
"#,
    )
    .unwrap();
    let store = Store::open(dir.join("orbit.db")).await.unwrap();
    let mut parent = store
        .create(
            dir.display().to_string(),
            "orbit::orbit-model".into(),
            false,
        )
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
        parent: parent.id().into(),
        store,
        config_path: path,
        auth_directory: dir.join("auth"),
        instructions: Default::default(),
        jobs: BackgroundJobs::with_capacity(limits.max_concurrent).unwrap(),
        slots: Arc::new(Semaphore::new(limits.max_concurrent)),
        limits,
        allowed_tools: ["read_file".into(), "read_history".into()]
            .into_iter()
            .collect(),
        active: Mutex::new(HashMap::new()),
        backend: Arc::new(move |_| Arc::new(model.clone())),
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
        "spawn_agent",
        json!({"task":format!("inspect {name}")}),
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
                "spawn_agent",
                json!({"task":format!("inspect Orbit module {n}")}),
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
    for request in child_model.requests.lock().unwrap().iter() {
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
        assert_eq!(names, ["read_file", "read_history"]);
    }
    child_model.gate.add_permits(8);
    let result = tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(model.requests().len() >= 3 && model.requests().len() <= 10);
    assert_eq!(service.store.subagents(parent.id()).await.unwrap().len(), 8);
    assert!(
        service
            .store
            .pending_agent_results(parent.id())
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(result.history.iter().filter(|m|matches!(m,ChatMessage::Tool{tool_call_id,..} if tool_call_id.starts_with("child-"))).count(),8);
    parent
        .finish("completed", Vec::new(), Some(result.history))
        .await
        .unwrap();
}

#[tokio::test]
async fn running_input_and_finished_continuation_keep_identity_and_canonical_history() {
    let dir = tempfile::tempdir().unwrap();
    let model = GatedModel::new();
    let (service, _parent) = fixture(dir.path(), model.clone(), Default::default()).await;
    let receipt = spawn(&service, "orbit-explore", CancellationToken::new()).await;
    let id = receipt["agent_id"].as_str().unwrap();
    let job = receipt["job_id"].as_str().unwrap().to_owned();
    model.received(1).await;
    let args = json!({"agent_id":id,"message":"补充检查 Unicode"});
    let call = ToolCall::function("input", "send_agent_message", args.clone());
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
    let args = json!({"agent_id":id,"message":"再次核对 Orbit"});
    let call = ToolCall::function("continue", "send_agent_message", args.clone());
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
            &[receipt["job_id"].as_str().unwrap().into()],
            true,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let (_, messages) = service
        .store
        .canonical_messages(id, None, 100)
        .await
        .unwrap();
    assert_eq!(
        messages
            .iter()
            .filter(
                |m| matches!(&m.message,ChatMessage::User{content,..} if content=="再次核对 Orbit")
            )
            .count(),
        1
    );
    assert_eq!(service.store.load(id).await.unwrap().runs.len(), 2);
    let args = json!({"agent_id":id,"length":1});
    let call = ToolCall::function("read", "read_agent", args.clone());
    let read: Value = serde_json::from_str(
        &service
            .clone()
            .execute(Action::Read, call, args, CancellationToken::new())
            .await
            .unwrap()
            .content,
    )
    .unwrap();
    assert_eq!(read["content"].as_str().unwrap().chars().count(), 1);
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
    let call = ToolCall::function("overflow", "spawn_agent", json!({"task":"overflow"}));
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
    let job = receipts[0]["job_id"].as_str().unwrap().to_owned();
    service.jobs.cancel(std::slice::from_ref(&job)).unwrap();
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
    for agent in service.store.subagents(&service.parent).await.unwrap() {
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
            &[finished["job_id"].as_str().unwrap().into()],
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
            ToolCall::function("interrupted", "spawn_agent", json!({"task":"review"})),
        )
        .await
        .unwrap();
    let interrupted_id = interrupted.id().to_owned();
    drop(interrupted);
    let recovered = Service {
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
    };
    recovered.recover().await.unwrap();
    assert_eq!(model.requests.lock().unwrap().len(), 1);
    assert_eq!(
        recovered.store.load(&interrupted_id).await.unwrap().runs[0].status,
        "interrupted"
    );
    let restored = recovered.jobs.snapshots(&[], true).unwrap();
    assert!(restored.is_empty()); // Restored results belong to the next Agent generation.
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
        .finish("completed", Vec::new(), Some(result.history))
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
    for name in [
        "read_file",
        "write_file",
        "run_command",
        "spawn_agent",
        "memory_list",
        "mcp__orbit__write",
    ] {
        tools.register(ClosureTool::new(name, "", json!({}), |_, _| async {
            Ok::<_, ToolError>("unused".into())
        }));
    }
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
    )
    .await
    .unwrap();
    assert_eq!(
        tools
            .definitions()
            .iter()
            .map(|t| t.function.name.as_str())
            .collect::<Vec<_>>(),
        ["read_file"]
    );
}

#[test]
fn validates_subagent_limits_and_defaults_to_eight() {
    assert_eq!(SubagentConfig::default().max_concurrent, 8);
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
            timeout_seconds: 0,
            ..Default::default()
        }
        .validate()
        .is_err()
    );
}
