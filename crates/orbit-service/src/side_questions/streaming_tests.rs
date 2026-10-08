use super::*;
use orbit_agent::model::{CompletionFuture, MockModel, StreamFuture};
use orbit_model::{
    error::ModelError,
    types::{ChatCompletion, ModelConfig, ModelStream, ToolCall},
};
use std::sync::Mutex;

fn client() -> ModelClient {
    ModelClient::new(ModelConfig {
        model: "orbit-fast".into(),
        base_url: "https://orbit.test/v1".into(),
        reasoning_effort: Some(ReasoningEffort::High),
        ..Default::default()
    })
    .unwrap()
}

fn completion(text: &str) -> ChatCompletion {
    ChatCompletion {
        id: Some("orbit-reply".into()),
        model: Some("orbit-fast".into()),
        message: ChatMessage::assistant(text),
        finish_reason: Some("stop".into()),
        usage: None,
    }
}

struct StreamingModel {
    receiver: Mutex<Option<mpsc::Receiver<Result<StreamEvent, ModelError>>>>,
    requests: Mutex<Vec<ChatRequest>>,
}

impl ModelBackend for StreamingModel {
    fn complete<'a>(&'a self, _: ChatRequest, _: CancellationToken) -> CompletionFuture<'a> {
        Box::pin(async { panic!("side questions must stream") })
    }

    fn stream<'a>(&'a self, request: ChatRequest, _: CancellationToken) -> StreamFuture<'a> {
        self.requests.lock().unwrap().push(request);
        let receiver = self.receiver.lock().unwrap().take().unwrap();
        Box::pin(async move {
            Ok(
                Box::pin(futures_util::stream::unfold(receiver, |mut rx| async {
                    rx.recv().await.map(|event| (event, rx))
                })) as ModelStream,
            )
        })
    }
}

async fn settle(questions: &mut SideQuestions) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while questions.running() {
            questions.poll();
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn streaming_preserves_main_run_and_persists_for_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("orbit.db")).await.unwrap();
    let mut writer = store
        .create(dir.path().display().to_string(), "orbit-fast".into(), false)
        .await
        .unwrap();
    writer
        .begin("Orbit 主任务".into(), "orbit-fast".into())
        .await
        .unwrap();
    let before = store.load(writer.id()).await.unwrap();
    let (sender, receiver) = mpsc::channel(16);
    let backend = Arc::new(StreamingModel {
        receiver: Mutex::new(Some(receiver)),
        requests: Mutex::new(vec![]),
    });
    let mut questions = SideQuestions::default();
    questions
        .start(
            &store,
            writer.id(),
            &client(),
            &[ChatMessage::system("Orbit 系统提示")],
            "解释一下",
            20_000,
            backend.clone(),
            None,
        )
        .await
        .unwrap();
    sender
        .send(Ok(StreamEvent::TextDelta("中文回答".into())))
        .await
        .unwrap();
    sender
        .send(Ok(StreamEvent::Completed {
            finish_reason: Some("stop".into()),
            usage: None,
        }))
        .await
        .unwrap();
    settle(&mut questions).await;
    assert_eq!(questions.records()[0].answer, "中文回答");
    assert_eq!(questions.records()[0].status, "completed");
    {
        let sent = backend.requests.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert!(sent[0].tools.is_empty());
        assert_eq!(sent[0].max_tokens, Some(8192));
        assert_eq!(sent[0].reasoning_effort, Some(ReasoningEffort::High));
    }
    let after = store.load(writer.id()).await.unwrap();
    assert_eq!(before.history, after.history);
    assert_eq!(after.runs[0].status, "running");
    let mut reopened = SideQuestions::default();
    reopened.load(&store, writer.id()).await.unwrap();
    assert_eq!(reopened.records()[0].answer, "中文回答");
    assert!(!reopened.running());
}

#[tokio::test]
async fn cancelling_and_tool_attempts_are_isolated_to_the_side_question() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("orbit.db")).await.unwrap();
    let mut writer = store
        .create(dir.path().display().to_string(), "orbit-fast".into(), false)
        .await
        .unwrap();
    writer
        .begin("Orbit 主任务".into(), "orbit-fast".into())
        .await
        .unwrap();
    let (sender, receiver) = mpsc::channel(16);
    let backend = Arc::new(StreamingModel {
        receiver: Mutex::new(Some(receiver)),
        requests: Mutex::new(vec![]),
    });
    let mut questions = SideQuestions::default();
    questions
        .start(
            &store,
            writer.id(),
            &client(),
            &[],
            "取消问题",
            4096,
            backend,
            None,
        )
        .await
        .unwrap();
    assert!(questions.cancel(0));
    settle(&mut questions).await;
    assert_eq!(questions.records()[0].status, "cancelled");
    drop(sender);
    let calls = MockModel::new([ChatCompletion {
        message: ChatMessage::assistant_tool_calls(vec![ToolCall::function(
            "orbit-call",
            "write_file",
            serde_json::json!({"path":"orbit.rs","content":"bad"}),
        )]),
        ..completion("")
    }]);

    questions
        .start(
            &store,
            writer.id(),
            &client(),
            &[],
            "需要工具",
            4096,
            Arc::new(calls.clone()),
            None,
        )
        .await
        .unwrap();
    settle(&mut questions).await;
    assert_eq!(questions.records()[1].status, "failed");
    assert!(
        questions.records()[1]
            .error
            .as_deref()
            .unwrap()
            .contains("尝试调用工具")
    );
    assert!(!dir.path().join("orbit.rs").exists());
    assert_eq!(calls.requests().len(), 1);
    assert_eq!(
        store.load(writer.id()).await.unwrap().runs[0].status,
        "running"
    );
    assert_eq!(store.side_questions(writer.id()).await.unwrap().len(), 2);
}

#[tokio::test]
async fn independent_questions_do_not_import_other_side_answers() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("orbit.db")).await.unwrap();
    let writer = store
        .create(dir.path().display().to_string(), "orbit-fast".into(), false)
        .await
        .unwrap();
    let backend = MockModel::new([completion("第一个独立回答"), completion("第二个独立回答")]);
    let mut questions = SideQuestions::default();
    for text in ["第一个问题", "第二个问题"] {
        questions
            .start(
                &store,
                writer.id(),
                &client(),
                &[],
                text,
                4096,
                Arc::new(backend.clone()),
                None,
            )
            .await
            .unwrap();
        settle(&mut questions).await;
    }

    assert!(
        !serde_json::to_string(&backend.requests()[1])
            .unwrap()
            .contains("第一个独立回答")
    );
    assert!(store.load(writer.id()).await.unwrap().runs.is_empty());
}

#[tokio::test]
async fn follow_ups_keep_their_thread_after_restart_and_leave_other_questions_independent() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("orbit.db")).await.unwrap();
    let writer = store
        .create(dir.path().display().to_string(), "orbit-fast".into(), false)
        .await
        .unwrap();
    let before = store.load(writer.id()).await.unwrap();
    let backend = MockModel::new([
        completion("Orbit 原回答"),
        completion("Orbit 另一回答"),
        completion("Orbit 追问回答"),
    ]);
    let mut questions = SideQuestions::default();
    for prompt in ["Orbit 原问题", "Orbit 另一问题"] {
        questions
            .start(
                &store,
                writer.id(),
                &client(),
                &[],
                prompt,
                4096,
                Arc::new(backend.clone()),
                None,
            )
            .await
            .unwrap();
        settle(&mut questions).await;
    }

    let mut restored = SideQuestions::default();
    restored
        .start(
            &store,
            writer.id(),
            &client(),
            &[],
            "",
            4096,
            Arc::new(backend.clone()),
            None,
        )
        .await
        .unwrap();
    restored
        .start(
            &store,
            writer.id(),
            &client(),
            &[],
            "再解释 Orbit",
            4096,
            Arc::new(backend.clone()),
            Some(0),
        )
        .await
        .unwrap();
    settle(&mut restored).await;
    assert_eq!(
        restored.records()[0].thread_id,
        restored.records()[2].thread_id
    );
    assert_ne!(
        restored.records()[1].thread_id,
        restored.records()[2].thread_id
    );
    let sent = serde_json::to_string(&backend.requests()[2].messages).unwrap();
    for expected in ["Orbit 原问题", "Orbit 原回答", "再解释 Orbit"] {
        assert!(sent.contains(expected));
    }
    assert!(!sent.contains("Orbit 另一回答"));
    let after = store.load(writer.id()).await.unwrap();
    assert_eq!(before.history, after.history);
    assert!(after.runs.is_empty());
    let mut again = SideQuestions::default();
    again
        .start(
            &store,
            writer.id(),
            &client(),
            &[],
            "",
            4096,
            Arc::new(backend),
            None,
        )
        .await
        .unwrap();
    assert_eq!(again.records().len(), 3);
    assert_eq!(again.records()[0].thread_id, again.records()[2].thread_id);
}

#[tokio::test]
async fn interrupted_or_empty_streams_preserve_partial_answers_and_fail_locally() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("orbit.db")).await.unwrap();
    let writer = store
        .create(dir.path().display().to_string(), "orbit-fast".into(), false)
        .await
        .unwrap();
    let (sender, receiver) = mpsc::channel(16);
    let backend = Arc::new(StreamingModel {
        receiver: Mutex::new(Some(receiver)),
        requests: Mutex::new(vec![]),
    });
    let mut questions = SideQuestions::default();
    questions
        .start(
            &store,
            writer.id(),
            &client(),
            &[],
            "问题",
            4096,
            backend,
            None,
        )
        .await
        .unwrap();
    sender
        .send(Ok(StreamEvent::TextDelta("已生成部分".into())))
        .await
        .unwrap();
    drop(sender);
    settle(&mut questions).await;
    assert_eq!(questions.records()[0].answer, "已生成部分");
    assert_eq!(questions.records()[0].status, "failed");
    let backend = MockModel::new([completion("")]);
    questions
        .start(
            &store,
            writer.id(),
            &client(),
            &[],
            "空回答",
            4096,
            Arc::new(backend),
            None,
        )
        .await
        .unwrap();
    settle(&mut questions).await;
    assert!(
        questions.records()[1]
            .error
            .as_deref()
            .unwrap()
            .contains("未返回旁路回答")
    );
}
