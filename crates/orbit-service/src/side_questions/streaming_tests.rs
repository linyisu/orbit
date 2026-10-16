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
        self.requests
            .lock()
            .unwrap()
            .push(request);
        let receiver = self
            .receiver
            .lock()
            .unwrap()
            .take()
            .unwrap();
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
async fn streaming_applies_model_options_and_keeps_answer_until_cleared() {
    let (sender, receiver) = mpsc::channel(16);
    let backend = Arc::new(StreamingModel {
        receiver: Mutex::new(Some(receiver)),
        requests: Mutex::new(vec![]),
    });
    let mut questions = SideQuestions::default();
    questions
        .start(
            &client(),
            &[
                ChatMessage::system("Orbit 系统提示"),
                ChatMessage::user("main context"),
            ],
            "解释一下",
            20_000,
            backend.clone(),
            None,
        )
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
        assert_eq!(sent[0].max_tokens, Some(8_192));
        assert_eq!(sent[0].reasoning_effort, Some(ReasoningEffort::High));
        assert!(
            sent[0]
                .messages
                .contains(&ChatMessage::user("main context"))
        );
    }
    questions.clear();
    assert!(questions.records().is_empty());
    assert!(
        SideQuestions::default()
            .records()
            .is_empty()
    );
}

#[tokio::test]
async fn cancelling_and_tool_attempts_are_isolated_to_the_side_question() {
    let (sender, receiver) = mpsc::channel(16);
    let backend = Arc::new(StreamingModel {
        receiver: Mutex::new(Some(receiver)),
        requests: Mutex::new(vec![]),
    });
    let mut questions = SideQuestions::default();
    questions
        .start(&client(), &[], "取消问题", 4_096, backend, None)
        .unwrap();
    assert!(questions.cancel(0));
    settle(&mut questions).await;
    assert_eq!(questions.records()[0].status, "cancelled");
    drop(sender);
    let calls = MockModel::new([ChatCompletion {
        message: ChatMessage::assistant_tool_calls(vec![ToolCall::function(
            "orbit-call",
            "WriteFile",
            serde_json::json!({
                "path": "orbit.rs",
                "content": "bad"
            }),
        )]),
        ..completion("")
    }]);

    questions
        .start(
            &client(),
            &[],
            "需要工具",
            4_096,
            Arc::new(calls.clone()),
            None,
        )
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
    assert_eq!(calls.requests().len(), 1);
}

#[tokio::test]
async fn independent_questions_do_not_import_other_side_answers() {
    let backend = MockModel::new([completion("第一个独立回答"), completion("第二个独立回答")]);
    let mut questions = SideQuestions::default();
    for text in ["第一个问题", "第二个问题"] {
        questions
            .start(&client(), &[], text, 4_096, Arc::new(backend.clone()), None)
            .unwrap();
        settle(&mut questions).await;
    }

    assert!(
        !serde_json::to_string(&backend.requests()[1])
            .unwrap()
            .contains("第一个独立回答")
    );
}

#[tokio::test]
async fn follow_ups_keep_their_live_thread_and_leave_other_questions_independent() {
    let backend = MockModel::new([
        completion("Orbit 原回答"),
        completion("Orbit 另一回答"),
        completion("Orbit 追问回答"),
    ]);
    let mut questions = SideQuestions::default();
    for prompt in ["Orbit 原问题", "Orbit 另一问题"] {
        questions
            .start(
                &client(),
                &[],
                prompt,
                4_096,
                Arc::new(backend.clone()),
                None,
            )
            .unwrap();
        settle(&mut questions).await;
    }

    questions
        .start(
            &client(),
            &[],
            "再解释 Orbit",
            4_096,
            Arc::new(backend.clone()),
            Some(0),
        )
        .unwrap();
    settle(&mut questions).await;
    assert_eq!(
        questions.records()[0].thread_id,
        questions.records()[2].thread_id
    );
    assert_ne!(
        questions.records()[1].thread_id,
        questions.records()[2].thread_id
    );
    let sent = serde_json::to_string(&backend.requests()[2].messages).unwrap();
    for expected in ["Orbit 原问题", "Orbit 原回答", "再解释 Orbit"] {
        assert!(sent.contains(expected));
    }
    assert!(!sent.contains("Orbit 另一回答"));
    questions.clear();
    assert!(questions.records().is_empty());
    assert!(
        questions
            .start(&client(), &[], "", 4_096, Arc::new(backend), None)
            .is_err()
    );
}

#[tokio::test]
async fn interrupted_or_empty_streams_preserve_partial_answers_and_fail_locally() {
    let (sender, receiver) = mpsc::channel(16);
    let backend = Arc::new(StreamingModel {
        receiver: Mutex::new(Some(receiver)),
        requests: Mutex::new(vec![]),
    });
    let mut questions = SideQuestions::default();
    questions
        .start(&client(), &[], "问题", 4_096, backend, None)
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
        .start(&client(), &[], "空回答", 4_096, Arc::new(backend), None)
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

#[tokio::test]
async fn clearing_a_running_question_closes_the_stream_and_drops_all_records() {
    let (sender, receiver) = mpsc::channel(16);
    let backend = Arc::new(StreamingModel {
        receiver: Mutex::new(Some(receiver)),
        requests: Mutex::new(vec![]),
    });
    let mut questions = SideQuestions::default();
    questions
        .start(&client(), &[], "question", 4_096, backend.clone(), None)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while backend
            .requests
            .lock()
            .unwrap()
            .is_empty()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    sender
        .send(Ok(StreamEvent::TextDelta("partial answer".into())))
        .await
        .unwrap();
    questions.clear();
    tokio::time::timeout(Duration::from_secs(5), sender.closed())
        .await
        .unwrap();
    assert!(!questions.running());
    assert!(!questions.poll());
    assert!(questions.records().is_empty());
}
