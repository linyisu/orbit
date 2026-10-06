use orbit_model::types::{ChatRequest, ReasoningEffort, StreamEvent};
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crossterm::event::KeyEvent;
use orbit_agent::model::{CompletionFuture, MockModel, StreamFuture};
use orbit_model::{
    error::ModelError,
    types::{ChatCompletion, ModelConfig, ModelStream, ToolCall},
};
use ratatui::{Terminal, backend::TestBackend};

use super::*;

fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
    Event::Key(KeyEvent::new(code, modifiers))
}

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

async fn settle(questions: &mut Questions) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while questions.running() {
            questions.poll();
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[test]
fn command_is_available_during_a_main_run_and_does_not_steer_it() {
    assert_eq!(question("/btw 为什么？"), Some("为什么？"));
    assert_eq!(question("/btw"), Some(""));
    assert_eq!(question("/btw\nOrbit 问题"), Some("Orbit 问题"));
    assert_eq!(question("/btwhatever"), None);
    let mut state = super::super::app::UiState::new("orbit-fast");
    state.status = RunStatus::Running;
    state.input.set("/btw 为什么？".into());
    assert_eq!(
        super::super::keyboard_action(&mut state, key(KeyCode::Enter, KeyModifiers::NONE), true),
        super::super::Action::Submit("/btw 为什么？".into())
    );
    assert_eq!(state.status, RunStatus::Running);
    state.input.set("/bt".into());
    assert!(
        state
            .menu()
            .iter()
            .any(|candidate| candidate.name == "/btw")
    );
}

#[tokio::test]
async fn questions_stream_after_hiding_keep_main_session_running_and_can_be_reopened() {
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
    let main_token = CancellationToken::new();
    let mut questions = Questions::default();
    questions
        .ask_with_backend(
            &store,
            writer.id(),
            &client(),
            &[ChatMessage::system("Orbit 系统提示")],
            "解释一下",
            20_000,
            backend.clone(),
        )
        .await
        .unwrap();
    assert!(questions.visible());
    assert!(matches!(
        questions
            .handle(&key(KeyCode::Esc, KeyModifiers::NONE))
            .unwrap(),
        Action::Close
    ));
    assert!(!questions.visible());
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
    assert!(!questions.visible());
    assert!(!main_token.is_cancelled());
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
    let mut reopened = Questions::default();
    reopened
        .ask(&store, writer.id(), &client(), &[], "", 4096)
        .await
        .unwrap();
    assert!(reopened.visible());
    assert_eq!(reopened.records()[0].answer, "中文回答");
    for (width, height) in [(100, 30), (30, 12), (10, 3), (1, 1)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                let area = super::super::dialogs::centered_area(frame.area());
                let inner = ratatui::widgets::Block::bordered().inner(area);
                reopened.draw(frame, area, inner, Default::default());
            })
            .unwrap();
    }
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
    let mut questions = Questions::default();
    questions
        .ask_with_backend(
            &store,
            writer.id(),
            &client(),
            &[],
            "取消问题",
            4096,
            backend,
        )
        .await
        .unwrap();
    assert!(matches!(
        questions
            .handle(&key(KeyCode::Char('c'), KeyModifiers::CONTROL))
            .unwrap(),
        Action::Redraw
    ));
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
        .ask_with_backend(
            &store,
            writer.id(),
            &client(),
            &[],
            "需要工具",
            4096,
            Arc::new(calls.clone()),
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
async fn history_navigation_and_new_questions_do_not_import_other_side_answers() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("orbit.db")).await.unwrap();
    let writer = store
        .create(dir.path().display().to_string(), "orbit-fast".into(), false)
        .await
        .unwrap();
    let backend = MockModel::new([completion("第一个独立回答"), completion("第二个独立回答")]);
    let mut questions = Questions::default();
    for text in ["第一个问题", "第二个问题"] {
        questions
            .ask_with_backend(
                &store,
                writer.id(),
                &client(),
                &[],
                text,
                4096,
                Arc::new(backend.clone()),
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
    assert_eq!(questions.dialog.as_ref().unwrap().index, 1);
    questions
        .handle(&key(KeyCode::Left, KeyModifiers::ALT))
        .unwrap();
    assert_eq!(questions.dialog.as_ref().unwrap().index, 0);
    assert!(
        matches!(&questions.dialog.as_ref().unwrap().view.ui.transcript[1], TranscriptItem::Assistant(text) if text == "第一个独立回答")
    );
    questions
        .handle(&key(KeyCode::Right, KeyModifiers::ALT))
        .unwrap();
    assert_eq!(questions.dialog.as_ref().unwrap().index, 1);
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
    let mut questions = Questions::default();
    for prompt in ["Orbit 原问题", "Orbit 另一问题"] {
        questions
            .ask_with_backend(
                &store,
                writer.id(),
                &client(),
                &[],
                prompt,
                4096,
                Arc::new(backend.clone()),
            )
            .await
            .unwrap();
        settle(&mut questions).await;
    }

    let mut restored = Questions::default();
    restored
        .ask_with_backend(
            &store,
            writer.id(),
            &client(),
            &[],
            "",
            4096,
            Arc::new(backend.clone()),
        )
        .await
        .unwrap();
    restored.open(0);
    restored
        .dialog
        .as_mut()
        .unwrap()
        .view
        .ui
        .input
        .set("再解释 Orbit".into());
    assert!(
        matches!(restored.handle(&key(KeyCode::Enter, KeyModifiers::NONE)).unwrap(), Action::Submit(text) if text == "再解释 Orbit")
    );
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
    assert_eq!(restored.thread_heads(), vec![2, 1]);
    let sent = serde_json::to_string(&backend.requests()[2].messages).unwrap();
    for expected in ["Orbit 原问题", "Orbit 原回答", "再解释 Orbit"] {
        assert!(sent.contains(expected));
    }
    assert!(!sent.contains("Orbit 另一回答"));
    assert!(
        restored
            .dialog
            .as_ref()
            .unwrap()
            .view
            .ui
            .input
            .text
            .is_empty()
    );
    assert_eq!(
        restored.dialog.as_ref().unwrap().view.ui.transcript.len(),
        4
    );
    let after = store.load(writer.id()).await.unwrap();
    assert_eq!(before.history, after.history);
    assert!(after.runs.is_empty());
    let mut again = Questions::default();
    again
        .ask_with_backend(
            &store,
            writer.id(),
            &client(),
            &[],
            "",
            4096,
            Arc::new(backend),
        )
        .await
        .unwrap();
    assert_eq!(again.thread_heads(), vec![2, 1]);
    assert_eq!(again.dialog.as_ref().unwrap().view.ui.transcript.len(), 4);
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
    let mut questions = Questions::default();
    questions
        .ask_with_backend(&store, writer.id(), &client(), &[], "问题", 4096, backend)
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
        .ask_with_backend(
            &store,
            writer.id(),
            &client(),
            &[],
            "空回答",
            4096,
            Arc::new(backend),
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

#[tokio::test]
async fn closing_a_tab_keeps_its_side_request_running_until_the_saved_result_arrives() {
    use super::super::{app::UiState, background::Runs, tabs::Tabs};
    use crate::session::SessionState;

    let dir = tempfile::tempdir().unwrap();
    let mut session = SessionState::open_shared_in(
        dir.path().join("orbit.db"),
        None,
        "orbit-fast".into(),
        dir.path(),
    )
    .await
    .unwrap();
    let id = session.id().to_owned();
    let store = session.store.clone();
    let mut tabs = Tabs::new(&session);
    let mut state = UiState::new("orbit-fast");
    let mut model = client();
    let mut history = vec![];
    let (sender, receiver) = mpsc::channel(16);
    let backend = Arc::new(StreamingModel {
        receiver: Mutex::new(Some(receiver)),
        requests: Mutex::new(vec![]),
    });
    state
        .btw
        .ask_with_backend(&store, &id, &model, &[], "后台问题", 4096, backend)
        .await
        .unwrap();
    assert!(tabs.empty_tab(&session, &state).is_none());
    state
        .btw
        .handle(&key(KeyCode::Esc, KeyModifiers::NONE))
        .unwrap();
    let next = crate::tui::session_actions::prepare_test_session(&session, "orbit-fast".into())
        .await
        .unwrap();
    tabs.activate(
        &mut session,
        &mut state,
        &mut model,
        &mut history,
        (next, UiState::new("orbit-fast"), client(), vec![]),
    );
    assert!(tabs.close_parked(&id));
    assert!(tabs.prune_closed(&Runs::new()).is_empty());
    sender
        .send(Ok(StreamEvent::TextDelta("后台回答".into())))
        .await
        .unwrap();
    sender
        .send(Ok(StreamEvent::Completed {
            finish_reason: Some("stop".into()),
            usage: None,
        }))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while tabs.parked_mut(&id).unwrap().state.btw.running() {
            tabs.poll_side_questions();
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(tabs.prune_closed(&Runs::new()), vec![id.clone()]);
    assert_eq!(
        store.side_questions(&id).await.unwrap()[0].answer,
        "后台回答"
    );
    assert!(state.btw.records().is_empty());
}

#[tokio::test]
async fn a_side_dialog_stays_above_pending_approval_and_freezes_text_while_selected() {
    use super::super::app::UiState;
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use orbit_tools::approval::{ApprovalPreview, WritePreview};

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
    let mut state = UiState::new("orbit-fast");
    state.status = RunStatus::Running;
    state
        .transcript
        .push(TranscriptItem::User("Orbit 主任务".into()));
    state
        .btw
        .ask_with_backend(
            &store,
            writer.id(),
            &client(),
            &[],
            "解释 Orbit",
            4096,
            backend,
        )
        .await
        .unwrap();
    sender
        .send(Ok(StreamEvent::TextDelta("中文回答".into())))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while state.btw.records()[0].answer.is_empty() {
            state.btw.poll();
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    state.approval = Some(ApprovalPreview::Write(WritePreview {
        path: "orbit.rs".into(),
        before: None,
        after: "approval-body-marker".into(),
    }));
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|frame| super::super::render::draw(frame, &mut state))
        .unwrap();
    let screen: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect();

    assert!(screen.replace(' ', "").contains("旁路提问"));
    assert!(!screen.contains("approval-body-marker"));
    let area = state
        .btw
        .dialog
        .as_ref()
        .unwrap()
        .view
        .ui
        .selection
        .viewport;

    for (kind, x) in [
        (MouseEventKind::Down(MouseButton::Left), area.x + 2),
        (MouseEventKind::Drag(MouseButton::Left), area.x + 8),
        (MouseEventKind::Up(MouseButton::Left), area.x + 8),
    ] {
        state
            .btw
            .handle(&Event::Mouse(MouseEvent {
                kind,
                column: x,
                row: area.y + 2,
                modifiers: KeyModifiers::NONE,
            }))
            .unwrap();
    }

    assert!(
        state
            .btw
            .dialog
            .as_ref()
            .unwrap()
            .view
            .ui
            .selection
            .active()
    );
    sender
        .send(Ok(StreamEvent::TextDelta("追加内容".into())))
        .await
        .unwrap();
    sender
        .send(Ok(StreamEvent::Completed {
            finish_reason: Some("stop".into()),
            usage: None,
        }))
        .await
        .unwrap();
    settle(&mut state.btw).await;
    assert!(
        matches!(&state.btw.dialog.as_ref().unwrap().view.ui.transcript[1],TranscriptItem::Assistant(text) if text=="中文回答")
    );
    state.btw.dialog.as_mut().unwrap().view.ui.selection.clear();
    state.btw.poll();
    assert!(
        matches!(&state.btw.dialog.as_ref().unwrap().view.ui.transcript[1],TranscriptItem::Assistant(text) if text=="中文回答追加内容")
    );
    assert_eq!(state.status, RunStatus::Running);
    assert!(state.approval.is_some());
}
