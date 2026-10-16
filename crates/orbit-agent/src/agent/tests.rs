use orbit_model::types::{Attachment, FunctionCall, ToolCall, Usage};
use serde_json::json;

use super::*;
use crate::{
    model::MockModel,
    tool::{ClosureTool, Tool, ToolError, ToolFuture, ToolOutput},
};

mod tool_execution;

fn assistant_text(text: &str) -> ChatCompletion {
    ChatCompletion {
        id: None,
        model: Some("mock".to_owned()),
        message: ChatMessage::assistant(text),
        finish_reason: Some("stop".to_owned()),
        usage: None,
    }
}

#[tokio::test]
async fn calibrated_budget_stops_request_and_reports_real_configured_limit() {
    let backend = MockModel::new(vec![]);
    let mut calibration = crate::calibration::TokenCalibration::default();
    calibration.observe(1, Some(10_000));
    let agent = Agent::new(
        backend.clone(),
        ToolRegistry::new(),
        AgentConfig {
            context_tokens: 8_192,
            auto_compact: false,
            ..Default::default()
        },
    )
    .with_calibration("provider::model".into(), calibration);
    let result = agent
        .run(request(), CancellationToken::new())
        .await;

    assert!(matches!(
        result,
        Err(AgentError::ContextBudget {
            estimated,
            limit: 8_192,
        }) if estimated > 8_192
    ));
    assert!(backend.requests().is_empty());
}

struct FailingJournal {
    checkpoints: std::sync::Mutex<Vec<crate::checkpoint::ExecutionCheckpoint>>,
    fail_start: bool,
    fail_receipt: bool,
}

#[tokio::test]
async fn compaction_without_an_archive_keeps_the_original_history() {
    let agent = Agent::new(
        MockModel::new(vec![]),
        ToolRegistry::new(),
        AgentConfig {
            context_tokens: 8_192,
            ..Default::default()
        },
    );
    let original = vec![
        ChatMessage::system("rules"),
        ChatMessage::user("preserve this request"),
        ChatMessage::assistant("previous work"),
    ];
    let mut automatic = original.clone();
    let mut thresholds = ContextThresholds {
        prune_after: 0,
        compact_after: 0,
    };

    agent
        .prepare_context(
            &mut automatic,
            (128, 8_192, false),
            &mut thresholds,
            &Default::default(),
            CancellationToken::new(),
            &mut |_| std::future::ready(Ok(())),
        )
        .await
        .unwrap();

    let mut manual = original.clone();
    let error = agent
        .prepare_context(
            &mut manual,
            (128, 8_192, true),
            &mut thresholds,
            &Default::default(),
            CancellationToken::new(),
            &mut |_| std::future::ready(Ok(())),
        )
        .await
        .unwrap_err();

    assert_eq!(automatic, original);
    assert_eq!(manual, original);
    assert!(matches!(
        error,
        AgentError::CompactionFailed {
            kind: crate::compaction::CompactionFailureKind::Archive,
            ..
        }
    ));
}

impl crate::checkpoint::ExecutionJournal for FailingJournal {
    fn record(&self, _: AgentEvent) {}

    fn flush(&self) -> crate::checkpoint::CheckpointFuture<'_> {
        Box::pin(async { Ok(()) })
    }

    fn save<'a>(
        &'a self,
        checkpoint: crate::checkpoint::ExecutionCheckpoint,
        event: Option<AgentEvent>,
    ) -> crate::checkpoint::CheckpointFuture<'a> {
        Box::pin(async move {
            if (self.fail_start && matches!(event, Some(AgentEvent::ToolStarted { .. })))
                || (self.fail_receipt && matches!(event, Some(AgentEvent::ToolFinished { .. })))
            {
                Err(AgentError::Checkpoint("disk unavailable".into()))
            } else {
                self.checkpoints
                    .lock()
                    .unwrap()
                    .push(checkpoint);
                Ok(())
            }
        })
    }
}

#[tokio::test]
async fn cancellation_after_compaction_saves_the_published_context() {
    use std::sync::Arc;

    struct TestArchive;

    impl crate::archive::ContextArchive for TestArchive {
        fn save<'a>(&'a self, _messages: &'a [ChatMessage]) -> crate::archive::ArchiveFuture<'a> {
            Box::pin(async { Ok("archive://test".into()) })
        }
    }

    let response = assistant_text(
        &serde_json::json!({
            "goal": "fix parser",
            "constraints": ["preserve API"],
            "decisions": [],
            "completed": [],
            "remaining": ["test empty input"],
            "verification": [],
            "uncertainty": []
        })
        .to_string(),
    );
    let history = vec![
        ChatMessage::system("coding rules"),
        ChatMessage::user("fix parser, preserve API"),
        ChatMessage::assistant("investigation ".repeat(4_000)),
        ChatMessage::user("continue"),
    ];
    let journal = Arc::new(FailingJournal {
        fail_start: false,
        fail_receipt: false,
        checkpoints: Default::default(),
    });
    let agent = Agent::new(
        MockModel::new(vec![response; 16]),
        ToolRegistry::new(),
        AgentConfig {
            context_tokens: 16_000,
            ..Default::default()
        },
    )
    .with_execution_journal(journal.clone())
    .with_context_archive(Arc::new(TestArchive));
    let token = CancellationToken::new();
    let result = agent
        .run_core(ChatRequest::new(history.clone()), token.clone(), |event| {
            if matches!(event, AgentEvent::ContextCompacted { .. }) {
                token.cancel();
            }

            std::future::ready(Ok(()))
        })
        .await;

    assert!(matches!(result, Err(AgentError::Cancelled)));

    let checkpoints = journal.checkpoints.lock().unwrap();
    let restored = &checkpoints.last().unwrap().history;

    assert!(
        crate::context::estimate_tokens(restored, &[])
            < crate::context::estimate_tokens(&history, &[]) / 2
    );
    assert!(
        serde_json::to_string(restored)
            .unwrap()
            .contains("preserve API")
    );
}

#[tokio::test]
async fn durability_failure_stops_before_next_side_effect_on_all_adapters() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    for fail_start in [true, false] {
        for adapter in 0..3 {
            let count = Arc::new(AtomicUsize::new(0));
            let effects = count.clone();
            let mut tools = ToolRegistry::new();
            tools.register(ClosureTool::new("write", "", json!({}), move |_, _| {
                effects.fetch_add(1, Ordering::SeqCst);
                async { Ok("saved".into()) }
            }));
            let mut completion = assistant_tool_call("a", "write", "{}");
            completion.message = ChatMessage::assistant_tool_calls(vec![
                ToolCall::function("a", "write", json!({})),
                ToolCall::function("b", "write", json!({})),
            ]);
            let agent = Agent::new(
                MockModel::new(vec![completion]),
                tools,
                AgentConfig::default(),
            )
            .with_execution_journal(Arc::new(FailingJournal {
                fail_start,
                fail_receipt: !fail_start,
                checkpoints: Default::default(),
            }));
            let result = match adapter {
                0 => {
                    agent
                        .run(request(), CancellationToken::new())
                        .await
                }
                1 => {
                    agent
                        .run_with_events(request(), CancellationToken::new(), |_| {})
                        .await
                }

                _ => {
                    let (tx, _rx) = mpsc::channel(128);
                    agent
                        .run_with_agent_events(request(), CancellationToken::new(), tx)
                        .await
                }
            };

            assert!(matches!(result, Err(AgentError::Checkpoint(_))));
            assert_eq!(count.load(Ordering::SeqCst), usize::from(!fail_start));
        }
    }
}

#[test]
fn provider_payload_keeps_only_the_four_most_recent_images() {
    let history = (0..6)
        .map(|index| {
            ChatMessage::user_with_attachments(
                format!("prompt {index}"),
                vec![Attachment::Image {
                    lease: None,
                    name: format!("{index}.png"),
                    mime_type: "image/png".into(),
                    path: format!("/stored/{index}.png").into(),
                }],
            )
        })
        .collect::<Vec<_>>();
    let payload = model_messages(&history, true);
    let image_count = payload
        .iter()
        .map(|message| match message {
            ChatMessage::User { attachments, .. } => attachments.len(),
            _ => 0,
        })
        .sum::<usize>();

    assert_eq!(image_count, 4);
    assert!(matches!(
        &payload[0],
        ChatMessage::User { content, attachments }
            if attachments.is_empty() && content.contains("0.png")
    ));
    assert_eq!(
        history[0],
        ChatMessage::user_with_attachments(
            "prompt 0",
            vec![Attachment::Image {
                lease: None,
                name: "0.png".into(),
                mime_type: "image/png".into(),
                path: "/stored/0.png".into(),
            }]
        )
    );
}

#[test]
fn provider_payload_omits_all_images_for_text_only_model() {
    let history = vec![ChatMessage::user_with_attachments(
        "check this scene",
        vec![Attachment::Image {
            lease: None,
            name: "scene.png".into(),
            mime_type: "image/png".into(),
            path: "/stored/scene.png".into(),
        }],
    )];
    let payload = model_messages(&history, false);

    assert_eq!(payload.len(), 1);

    let ChatMessage::User {
        content,
        attachments,
    } = &payload[0]
    else {
        panic!("expected user message");
    };

    assert!(attachments.is_empty());
    assert!(content.contains("check this scene"));
    assert!(
        content.contains("[Image omitted: current model does not support image input: scene.png]")
    );
}

fn assistant_tool_call(id: &str, name: &str, arguments: &str) -> ChatCompletion {
    ChatCompletion {
        id: None,
        model: Some("mock".to_owned()),
        message: ChatMessage::Assistant {
            content: None,
            tool_calls: vec![ToolCall {
                purpose: None,
                metadata: None,
                id: id.to_owned(),
                kind: "function".to_owned(),
                function: FunctionCall {
                    name: name.to_owned(),
                    arguments: arguments.to_owned(),
                },
            }],
        },
        finish_reason: Some("tool_calls".to_owned()),
        usage: None,
    }
}

fn request() -> ChatRequest {
    ChatRequest::new(vec![ChatMessage::user("hello")])
}

struct ImageTool;

struct DocumentImageTool;

impl Tool for DocumentImageTool {
    fn definition(&self) -> orbit_model::types::ToolDefinition {
        orbit_model::types::ToolDefinition::function("ReadDocument", "", json!({}))
    }

    fn execute<'a>(&'a self, _call: ToolCall, _cancellation: CancellationToken) -> ToolFuture<'a> {
        panic!("agent must pass model capabilities to document tools")
    }

    fn execute_with_context<'a>(
        &'a self,
        _call: ToolCall,
        _cancellation: CancellationToken,
        context: crate::tool::ToolContext,
    ) -> ToolFuture<'a> {
        Box::pin(async move {
            if !context.supports_vision {
                return Ok(ToolOutput::text("text only; images unavailable"));
            }
            let directory = std::sync::Arc::new(tempfile::tempdir().unwrap());
            let path = directory.path().join("page-3.png");
            std::fs::write(&path, b"rendered page").unwrap();
            Ok(ToolOutput::with_attachments(
                "PDF page 3 attached",
                vec![Attachment::temporary_image(
                    "book.pdf — page 3.png".into(),
                    "image/png".into(),
                    path,
                    directory,
                )],
            ))
        })
    }
}

#[tokio::test]
async fn document_vision_respects_model_and_image_capability_and_reaches_next_request() {
    for (supports_vision, image_enabled) in [(true, true), (false, true), (true, false)] {
        let backend = MockModel::new(vec![
            assistant_tool_call("doc", "ReadDocument", "{}"),
            assistant_text("done"),
        ])
        .with_vision(supports_vision);
        let mut tools = ToolRegistry::new();
        tools.register(DocumentImageTool);
        if image_enabled {
            tools.register(ImageTool);
        }
        let result = Agent::new(backend.clone(), tools, AgentConfig::default())
            .run(request(), CancellationToken::new())
            .await
            .unwrap();
        let requests = backend.requests();
        let attachments = requests[1]
            .messages
            .iter()
            .filter_map(|message| match message {
                ChatMessage::User { attachments, .. } => Some(attachments.len()),
                _ => None,
            })
            .sum::<usize>();
        assert_eq!(attachments, usize::from(supports_vision && image_enabled));
        assert_eq!(result.turns, 2);
        if attachments > 0 {
            assert!(matches!(
                requests[1].messages.last(),
                Some(ChatMessage::User { content, .. })
                    if content.contains("page 3.png")
                        && !content.contains("ReadImage tool")
            ));
        }
    }
}

#[tokio::test]
async fn temporary_document_images_are_consumed_once_and_never_saved_as_dead_paths() {
    let backend = MockModel::new(vec![
        assistant_tool_call("doc", "ReadDocument", "{}"),
        assistant_tool_call("later", "echo", "{}"),
        assistant_text("done"),
    ])
    .with_vision(true);
    let mut tools = ToolRegistry::new();
    tools.register(DocumentImageTool);
    tools.register(ImageTool);
    tools.register(ClosureTool::new("echo", "", json!({}), |_, _| async {
        Ok("ok".into())
    }));
    let result = Agent::new(backend.clone(), tools, AgentConfig::default())
        .run(request(), CancellationToken::new())
        .await
        .unwrap();
    let requests = backend.requests();
    let images = |request: &ChatRequest| {
        request
            .messages
            .iter()
            .flat_map(|message| match message {
                ChatMessage::User { attachments, .. } => attachments.clone(),
                _ => vec![],
            })
            .collect::<Vec<_>>()
    };
    let pages = images(&requests[1]);
    assert_eq!(pages.len(), 1);
    assert!(
        pages[0].image().is_ok(),
        "the image is readable when the model consumes it"
    );
    let path = pages[0].path().to_owned();
    assert!(
        images(&requests[2]).is_empty(),
        "a later turn must not reattach consumed pages"
    );
    let checkpoint = crate::checkpoint::ExecutionCheckpoint {
        history: requests[1].messages.clone(),
        turns: 1,
        tool_calls: 1,
        in_flight: vec![],
        attachments: pages.clone(),
    };
    let serialized = serde_json::to_string(&checkpoint).unwrap();
    assert!(!serialized.contains(path.to_str().unwrap()));
    let recovered: crate::checkpoint::ExecutionCheckpoint =
        serde_json::from_str(&serialized).unwrap();
    assert!(recovered.attachments.is_empty());
    assert!(
        !serde_json::to_string(&result.history)
            .unwrap()
            .contains(path.to_str().unwrap())
    );
    drop(checkpoint);
    drop(pages);
    drop(requests);
    drop(backend);
    assert!(!path.exists(), "last consumer releases rendered files");
}

impl Tool for ImageTool {
    fn definition(&self) -> orbit_model::types::ToolDefinition {
        orbit_model::types::ToolDefinition::function("ReadImage", "", json!({}))
    }

    fn execute<'a>(&'a self, _call: ToolCall, _cancellation: CancellationToken) -> ToolFuture<'a> {
        Box::pin(async {
            Ok(ToolOutput::with_attachments(
                r#"{"loaded":true}"#,
                vec![Attachment::Image {
                    lease: None,
                    name: "scene.png".into(),
                    mime_type: "image/png".into(),
                    path: "/global/scene.png".into(),
                }],
            ))
        })
    }
}

#[tokio::test]
async fn image_tool_output_is_added_to_the_next_model_request() {
    let backend = MockModel::new(vec![
        assistant_tool_call("image-call", "ReadImage", "{}"),
        assistant_text("I can see it"),
    ])
    .with_vision(true);
    let mut tools = ToolRegistry::new();
    tools.register(ImageTool);
    let agent = Agent::new(backend.clone(), tools, AgentConfig::default());
    let result = agent
        .run(request(), CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(result.turns, 2);

    let requests = backend.requests();

    assert_eq!(requests.len(), 2);
    assert!(matches!(
        requests[1].messages.last(),
        Some(ChatMessage::User { content, attachments })
            if content.contains("scene.png") && attachments.len() == 1
    ));
    assert!(matches!(
        &requests[1].messages[2],
        ChatMessage::Tool { tool_call_id, .. } if tool_call_id == "image-call"
    ));
}

#[tokio::test]
async fn scenario_1_text_only_model_removes_read_image_and_fails_fast_on_invocation() {
    let backend = MockModel::new(vec![
        assistant_tool_call("image-call", "ReadImage", "{}"),
        assistant_text("I see it failed"),
    ])
    .with_vision(false);
    let mut tools = ToolRegistry::new();
    tools.register(ImageTool);
    let agent = Agent::new(backend.clone(), tools, AgentConfig::default());

    assert!(!agent.tools.contains("ReadImage"));
    assert!(
        !agent
            .tools
            .definitions()
            .iter()
            .any(|d| d.function.name == "ReadImage")
    );

    let result = agent
        .run(request(), CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(result.turns, 2);

    let requests = backend.requests();

    assert_eq!(requests.len(), 2);
    assert!(matches!(
        &requests[1].messages[2],
        ChatMessage::Tool { content, .. }
            if content.contains("current model does not support image input")
    ));
    assert!(
        !requests[1]
            .messages
            .iter()
            .any(|m| match m {
                ChatMessage::User { attachments, .. } => !attachments.is_empty(),
                _ => false,
            })
    );
}

#[tokio::test]
async fn scenario_2_model_failure_preserves_attachments_in_checkpoint_losslessly() {
    use std::sync::Arc;

    let journal = Arc::new(FailingJournal {
        checkpoints: std::sync::Mutex::new(Vec::new()),
        fail_start: false,
        fail_receipt: false,
    });
    let backend = MockModel::from_results(vec![
        Ok(assistant_tool_call("image-call", "ReadImage", "{}")),
        Err(orbit_model::error::ModelError::Protocol(
            "HTTP 500: rate limit or transient failure".into(),
        )),
    ])
    .with_vision(true);
    let mut tools = ToolRegistry::new();
    tools.register(ImageTool);
    let agent = Agent::new(backend.clone(), tools, AgentConfig::default())
        .with_execution_journal(journal.clone());
    let result = agent
        .run(request(), CancellationToken::new())
        .await;

    assert!(result.is_err());

    let checkpoints = journal.checkpoints.lock().unwrap();
    let last_checkpoint = checkpoints
        .last()
        .expect("saved checkpoint");
    let has_raw_attachments = last_checkpoint
        .history
        .iter()
        .any(|m| match m {
            ChatMessage::User { attachments, .. } => !attachments.is_empty(),
            _ => false,
        });

    assert!(
        has_raw_attachments,
        "checkpoint must preserve raw image attachments losslessly on model failure"
    );
}

#[tokio::test]
async fn scenario_3_retry_after_model_failure_retains_image_attachment() {
    use std::sync::Arc;

    let journal = Arc::new(FailingJournal {
        checkpoints: std::sync::Mutex::new(Vec::new()),
        fail_start: false,
        fail_receipt: false,
    });
    let backend = MockModel::from_results(vec![
        Ok(assistant_tool_call("image-call", "ReadImage", "{}")),
        Err(orbit_model::error::ModelError::Protocol(
            "HTTP 500: transient error".into(),
        )),
    ])
    .with_vision(true);
    let mut tools = ToolRegistry::new();
    tools.register(ImageTool);
    let agent = Agent::new(backend.clone(), tools.clone(), AgentConfig::default())
        .with_execution_journal(journal.clone());
    let _ = agent
        .run(request(), CancellationToken::new())
        .await;

    let restored_history = {
        let checkpoints = journal.checkpoints.lock().unwrap();
        checkpoints
            .last()
            .unwrap()
            .history
            .clone()
    };

    assert!(
        restored_history
            .iter()
            .any(|m| match m {
                ChatMessage::User { attachments, .. } => !attachments.is_empty(),
                _ => false,
            })
    );

    let follow_up_backend =
        MockModel::new(vec![assistant_text("I see the image and help you")]).with_vision(true);
    let follow_up_agent = Agent::new(follow_up_backend.clone(), tools, AgentConfig::default());
    let result = follow_up_agent
        .run(ChatRequest::new(restored_history), CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(result.turns, 1);

    let requests = follow_up_backend.requests();

    assert_eq!(requests.len(), 1);
    assert!(
        requests[0]
            .messages
            .iter()
            .any(|m| match m {
                ChatMessage::User { attachments, .. } => !attachments.is_empty(),
                _ => false,
            })
    );
}

#[tokio::test]
async fn scenario_4_resuming_failed_conversation_with_text_only_model() {
    let historical_poison = vec![
        ChatMessage::system("system"),
        ChatMessage::user("look at 1.jpg"),
        ChatMessage::user_with_attachments(
            "The ReadImage tool loaded the following local image into this model request: 1.jpg. \
                Analyze the actual image content directly.",
            vec![Attachment::Image {
                lease: None,
                name: "1.jpg".into(),
                mime_type: "image/jpeg".into(),
                path: "/stored/1.jpg".into(),
            }],
        ),
        ChatMessage::user("你看，我就说你不能识图吧"),
    ];

    let text_only_backend =
        MockModel::new(vec![assistant_text("确实，当前模型不支持直接识图。")]).with_vision(false);
    let agent = Agent::new(
        text_only_backend.clone(),
        ToolRegistry::new(),
        AgentConfig::default(),
    );
    let result = agent
        .run(
            ChatRequest::new(historical_poison),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(result.turns, 1);

    let requests = text_only_backend.requests();

    assert_eq!(requests.len(), 1);

    for msg in &requests[0].messages {
        if let ChatMessage::User { attachments, .. } = msg {
            assert!(
                attachments.is_empty(),
                "text-only model must receive 0 attachments"
            );
        }
    }

    let recovered_image_msg = requests[0]
        .messages
        .iter()
        .find(|m| match m {
            ChatMessage::User { content, .. } => content
                .contains("[Image omitted: current model does not support image input: 1.jpg]"),
            _ => false,
        });

    assert!(
        recovered_image_msg.is_some(),
        "must contain degradation text notice"
    );
}

#[tokio::test]
async fn scenario_5_switching_conversation_with_historical_images_to_text_only_and_back_to_vision()
{
    let vision_backend = MockModel::new(vec![
        assistant_tool_call("image-call", "ReadImage", "{}"),
        assistant_text("I see a blue square in scene.png."),
    ])
    .with_vision(true);
    let mut tools = ToolRegistry::new();
    tools.register(ImageTool);
    let agent = Agent::new(vision_backend, tools.clone(), AgentConfig::default());
    let result = agent
        .run(request(), CancellationToken::new())
        .await
        .unwrap();
    let history_after_vision = result.history;

    let mut switched_history = history_after_vision;
    switched_history.push(ChatMessage::user("What color was the square?"));
    let text_only_backend =
        MockModel::new(vec![assistant_text("The square was blue.")]).with_vision(false);
    let text_agent = Agent::new(
        text_only_backend.clone(),
        tools.clone(),
        AgentConfig::default(),
    );
    let result2 = text_agent
        .run(ChatRequest::new(switched_history), CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(result2.turns, 1);

    let requests = text_only_backend.requests();

    assert_eq!(requests.len(), 1);

    for msg in &requests[0].messages {
        if let ChatMessage::User { attachments, .. } = msg {
            assert!(attachments.is_empty());
        }
    }

    assert!(requests[0].messages.iter().any(|m| {
        match m {
            ChatMessage::User { content, .. } => content
                .contains("[Image omitted: current model does not support image input: scene.png]"),
            _ => false,
        }
    }));
    assert!(result2.history.iter().any(|m| match m {
        ChatMessage::User { attachments, .. } => !attachments.is_empty(),
        _ => false,
    }));

    let mut back_to_vision_history = result2.history;
    back_to_vision_history.push(ChatMessage::user(
        "Look at the image again, what is in the corner?",
    ));
    let vision_backend_2 =
        MockModel::new(vec![assistant_text("In the corner is a circle.")]).with_vision(true);
    let vision_agent_2 = Agent::new(vision_backend_2.clone(), tools, AgentConfig::default());
    let result3 = vision_agent_2
        .run(
            ChatRequest::new(back_to_vision_history),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert_eq!(result3.turns, 1);

    let requests2 = vision_backend_2.requests();

    assert_eq!(requests2.len(), 1);
    assert!(
        requests2[0]
            .messages
            .iter()
            .any(|m| match m {
                ChatMessage::User { attachments, .. } => !attachments.is_empty(),
                _ => false,
            }),
        "vision model must receive restored raw image attachment"
    );
}

#[tokio::test]
async fn token_exhaustion_does_not_retry_or_execute_partial_tools() {
    for message in [
        ChatMessage::assistant(""),
        ChatMessage::assistant_tool_calls(vec![ToolCall::function(
            "partial",
            "forbidden",
            json!({}),
        )]),
    ] {
        for streaming in [false, true] {
            let backend = MockModel::new(vec![ChatCompletion {
                id: None,
                model: None,
                message: message.clone(),
                finish_reason: Some("max_tokens".into()),
                usage: None,
            }]);
            let mut tools = ToolRegistry::new();
            tools.register(ClosureTool::new("forbidden", "", json!({}), |_, _| async {
                panic!("truncated response must not run tools")
            }));
            let agent = Agent::new(backend.clone(), tools, AgentConfig::default());
            let mut req = request();
            req.max_tokens = Some(16_384);
            let result = if streaming {
                let (tx, _rx) = mpsc::channel(64);
                agent
                    .run_with_agent_events(req, CancellationToken::new(), tx)
                    .await
            } else {
                agent
                    .run(req, CancellationToken::new())
                    .await
            };

            let error = result.unwrap_err().to_string();

            assert!(
                error.contains("max_tokens") && error.contains("max_output_tokens"),
                "{error}"
            );
            assert_eq!(backend.requests().len(), 1);
            assert_eq!(backend.requests()[0].max_tokens, Some(16_384));
        }
    }
}

struct UsageTailModel;

impl ModelBackend for UsageTailModel {
    fn complete<'a>(
        &'a self,
        _: ChatRequest,
        _: CancellationToken,
    ) -> crate::model::CompletionFuture<'a> {
        Box::pin(async { panic!("a completed stream must not fall back to complete") })
    }

    fn stream<'a>(
        &'a self,
        _: ChatRequest,
        _: CancellationToken,
    ) -> crate::model::StreamFuture<'a> {
        Box::pin(async {
            Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(StreamEvent::TextDelta("partial".into())),
                Ok(StreamEvent::Completed {
                    finish_reason: Some("length".into()),
                    usage: None,
                }),
                Ok(StreamEvent::Completed {
                    finish_reason: None,
                    usage: Some(Usage {
                        prompt_tokens: Some(10),
                        completion_tokens: Some(5),
                        total_tokens: Some(15),
                        ..Default::default()
                    }),
                }),
            ])) as orbit_model::types::ModelStream)
        })
    }
}

#[tokio::test]
async fn usage_only_tail_does_not_hide_a_truncated_stream_reason() {
    for with_agent_events in [false, true] {
        let agent = Agent::new(UsageTailModel, ToolRegistry::new(), AgentConfig::default());
        let error = if with_agent_events {
            let (sender, _receiver) = mpsc::channel(16);
            agent
                .run_with_agent_events(request(), CancellationToken::new(), sender)
                .await
                .unwrap_err()
        } else {
            agent
                .run_with_events(request(), CancellationToken::new(), |_| {})
                .await
                .unwrap_err()
        };

        assert!(
            error
                .to_string()
                .contains("输出达到上限")
        );
    }
}

struct SparseToolModel(MockModel);

impl ModelBackend for SparseToolModel {
    fn complete<'a>(
        &'a self,
        request: ChatRequest,
        token: CancellationToken,
    ) -> crate::model::CompletionFuture<'a> {
        self.0.complete(request, token)
    }

    fn stream<'a>(
        &'a self,
        request: ChatRequest,
        token: CancellationToken,
    ) -> crate::model::StreamFuture<'a> {
        Box::pin(async move {
            let stream = self.0.stream(request, token).await?;

            Ok(Box::pin(stream.map(|event| {
                event.map(|event| match event {
                    StreamEvent::ToolCallDelta(mut delta) => {
                        delta.index = delta.index * 3 + 2;
                        StreamEvent::ToolCallDelta(delta)
                    }

                    other => other,
                })
            })) as orbit_model::types::ModelStream)
        })
    }
}

#[tokio::test]
async fn sparse_content_indices_execute_only_real_tools_in_both_stream_paths() {
    for channel in [false, true] {
        let calls = vec![
            ToolCall::function(
                "first",
                "echo",
                json!({
                    "text": "one"
                }),
            ),
            ToolCall::function(
                "second",
                "echo",
                json!({
                    "text": "two"
                }),
            ),
        ];
        let mock = MockModel::new(vec![
            ChatCompletion {
                id: None,
                model: None,
                message: ChatMessage::Assistant {
                    content: Some("Reading files".into()),
                    tool_calls: calls.clone(),
                },
                finish_reason: Some("tool_use".into()),
                usage: None,
            },
            assistant_text("done"),
        ]);
        let mut tools = ToolRegistry::new();
        tools.register(ClosureTool::new(
            "echo",
            "echo",
            json!({
                "type": "object"
            }),
            |args, _| async move { Ok(args.to_string()) },
        ));
        let agent = Agent::new(SparseToolModel(mock.clone()), tools, AgentConfig::default());
        let result = if channel {
            let (tx, _rx) = mpsc::channel(64);
            agent
                .run_with_agent_events(request(), CancellationToken::new(), tx)
                .await
        } else {
            agent
                .run_with_events(request(), CancellationToken::new(), |_| {})
                .await
        }
        .unwrap();

        assert_eq!(result.tool_calls, 2);

        let sent = mock.requests();

        assert_eq!(sent.len(), 2);
        assert_eq!(sent[1].messages.len(), 4);

        let ChatMessage::Assistant { tool_calls, .. } = &sent[1].messages[1] else {
            panic!("assistant")
        };

        assert_eq!(tool_calls, &calls);
        assert!(matches!(
            &sent[1].messages[2],
            ChatMessage::Tool { tool_call_id, .. } if tool_call_id == "first"
        ));
        assert!(matches!(
            &sent[1].messages[3],
            ChatMessage::Tool { tool_call_id, .. } if tool_call_id == "second"
        ));
    }
}

#[tokio::test]
async fn malformed_ids_stop_before_tools_or_a_followup_request() {
    for ids in [["", ""], ["same", "same"]] {
        for streaming in [false, true] {
            let mock = MockModel::new(vec![ChatCompletion {
                id: None,
                model: None,
                message: ChatMessage::assistant_tool_calls(
                    ids.into_iter()
                        .map(|id| ToolCall::function(id, "fail_if_executed", json!({})))
                        .collect(),
                ),
                finish_reason: Some("tool_calls".into()),
                usage: None,
            }]);
            let mut tools = ToolRegistry::new();
            tools.register(ClosureTool::new(
                "fail_if_executed",
                "",
                json!({}),
                |_, _| async { panic!("invalid response must not execute any tools") },
            ));
            let agent = Agent::new(mock.clone(), tools, AgentConfig::default());
            let error = if streaming {
                let (tx, _rx) = mpsc::channel(64);
                agent
                    .run_with_agent_events(request(), CancellationToken::new(), tx)
                    .await
            } else {
                agent
                    .run(request(), CancellationToken::new())
                    .await
            }
            .unwrap_err();

            assert!(
                error
                    .to_string()
                    .contains("empty or duplicate")
            );
            assert_eq!(mock.requests().len(), 1);
        }
    }
}

struct ReasoningModel;

impl ModelBackend for ReasoningModel {
    fn complete<'a>(
        &'a self,
        _: ChatRequest,
        _: CancellationToken,
    ) -> crate::model::CompletionFuture<'a> {
        Box::pin(async { Ok(assistant_text("answer")) })
    }

    fn stream<'a>(
        &'a self,
        _: ChatRequest,
        _: CancellationToken,
    ) -> crate::model::StreamFuture<'a> {
        Box::pin(async {
            Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(StreamEvent::ReasoningDelta("thinking".into())),
                Ok(StreamEvent::TextDelta("answer".into())),
                Ok(StreamEvent::Completed {
                    finish_reason: Some("stop".into()),
                    usage: None,
                }),
            ])) as orbit_model::types::ModelStream)
        })
    }
}

#[tokio::test]
async fn reasoning_is_displayed_but_not_added_to_assistant_history() {
    let agent = Agent::new(ReasoningModel, ToolRegistry::new(), AgentConfig::default());
    let (sender, mut receiver) = mpsc::channel(16);
    let result = agent
        .run_with_agent_events(request(), CancellationToken::new(), sender)
        .await
        .unwrap();

    assert_eq!(result.final_message, ChatMessage::assistant("answer"));

    let mut saw_reasoning = false;

    while let Some(event) = receiver.recv().await {
        saw_reasoning |= matches!(event, AgentEvent::Model(StreamEvent::ReasoningDelta(_)));
    }

    assert!(saw_reasoning);

    let result = agent
        .run_with_events(request(), CancellationToken::new(), |_| {})
        .await
        .unwrap();

    assert_eq!(result.final_message, ChatMessage::assistant("answer"));
}

struct StreamOnlyModel;

struct PartialStreamModel(StreamEvent);

impl ModelBackend for PartialStreamModel {
    fn complete<'a>(
        &'a self,
        _: ChatRequest,
        _: CancellationToken,
    ) -> crate::model::CompletionFuture<'a> {
        Box::pin(async { panic!("an incomplete stream must not replay the request") })
    }

    fn stream<'a>(
        &'a self,
        _: ChatRequest,
        _: CancellationToken,
    ) -> crate::model::StreamFuture<'a> {
        Box::pin(async {
            Ok(Box::pin(futures_util::stream::iter([Ok(self.0.clone())]))
                as orbit_model::types::ModelStream)
        })
    }
}

#[tokio::test]
async fn incomplete_streams_neither_execute_partial_tools_nor_replay_the_request() {
    for event in [
        StreamEvent::TextDelta("unfinished response".into()),
        StreamEvent::ToolCallDelta(orbit_model::types::ToolCallDelta {
            metadata: None,
            index: 1,
            id: Some("partial".into()),
            name: Some("never_execute".into()),
            arguments: "{}".into(),
        }),
        StreamEvent::Completed {
            finish_reason: None,
            usage: Some(Usage::default()),
        },
    ] {
        let mut tools = ToolRegistry::new();
        tools.register(ClosureTool::new(
            "never_execute",
            "",
            json!({}),
            |_, _| async { panic!("unconfirmed tool calls must not run") },
        ));
        let agent = Agent::new(PartialStreamModel(event), tools, AgentConfig::default());
        let error = agent
            .run(request(), CancellationToken::new())
            .await
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("terminal finish reason")
        );
    }
}

impl ModelBackend for StreamOnlyModel {
    fn complete<'a>(
        &'a self,
        _: ChatRequest,
        _: CancellationToken,
    ) -> crate::model::CompletionFuture<'a> {
        Box::pin(async { panic!("the canonical execution loop must use stream") })
    }

    fn stream<'a>(
        &'a self,
        _: ChatRequest,
        _: CancellationToken,
    ) -> crate::model::StreamFuture<'a> {
        Box::pin(async {
            Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(StreamEvent::TextDelta("streamed answer".into())),
                Ok(StreamEvent::Completed {
                    finish_reason: Some("stop".into()),
                    usage: None,
                }),
            ])) as orbit_model::types::ModelStream)
        })
    }
}

#[tokio::test]
async fn non_event_run_uses_the_same_streaming_core() {
    let agent = Agent::new(StreamOnlyModel, ToolRegistry::new(), AgentConfig::default());
    let result = agent
        .run(request(), CancellationToken::new())
        .await
        .unwrap();

    assert_eq!(
        result.final_message,
        ChatMessage::assistant("streamed answer")
    );
}

#[tokio::test]
async fn completes_when_model_returns_text() {
    let model = MockModel::new([assistant_text("hello back")]);
    let agent = Agent::new(model.clone(), ToolRegistry::new(), AgentConfig::default());

    let result = agent
        .run(request(), CancellationToken::new())
        .await
        .expect("agent completes");

    assert_eq!(result.final_message, ChatMessage::assistant("hello back"));
    assert_eq!(result.turns, 1);
    assert_eq!(result.tool_calls, 0);
    assert_eq!(result.history.len(), 2);
    assert_eq!(model.requests().len(), 1);
}

#[tokio::test]
async fn executes_tool_and_sends_result_back_to_model() {
    let model = MockModel::new([
        assistant_tool_call("call-1", "echo", r#"{"value":"hello"}"#),
        assistant_text("tool finished"),
    ]);
    let mut tools = ToolRegistry::new();
    tools.register(ClosureTool::new(
        "echo",
        "Echo a value",
        json!({
            "type": "object",
            "properties": {
                "value": {
                    "type": "string"
                }
            }
        }),
        |arguments, _cancellation| async move {
            Ok(arguments["value"]
                .as_str()
                .unwrap_or_default()
                .to_owned())
        },
    ));
    let agent = Agent::new(model.clone(), tools, AgentConfig::default());

    let result = agent
        .run(request(), CancellationToken::new())
        .await
        .expect("agent completes");

    assert_eq!(
        result.final_message,
        ChatMessage::assistant("tool finished")
    );
    assert_eq!(result.turns, 2);
    assert_eq!(result.tool_calls, 1);
    assert_eq!(result.history.len(), 4);
    assert!(matches!(
        &result.history[2],
        ChatMessage::Tool {
            tool_call_id,
            content
        } if tool_call_id == "call-1" && content == "hello"
    ));

    let requests = model.requests();

    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].messages.len(), 3);
    assert_eq!(requests[1].tools.len(), 1);
}

#[tokio::test]
async fn tool_failure_is_returned_to_model_as_tool_message() {
    let model = MockModel::new([
        assistant_tool_call("call-1", "missing", "{}"),
        assistant_text("I recovered"),
    ]);
    let agent = Agent::new(model, ToolRegistry::new(), AgentConfig::default());

    let result = agent
        .run(request(), CancellationToken::new())
        .await
        .expect("tool failure is recoverable");

    assert_eq!(result.final_message, ChatMessage::assistant("I recovered"));
    assert!(matches!(
        &result.history[2],
        ChatMessage::Tool { content, .. } if content.contains("unknown tool: missing")
    ));
}

#[tokio::test]
async fn invalid_tool_arguments_are_returned_to_model() {
    let model = MockModel::new([
        assistant_tool_call("call-1", "echo", "not-json"),
        assistant_text("bad arguments handled"),
    ]);
    let mut tools = ToolRegistry::new();
    tools.register(ClosureTool::new(
        "echo",
        "Echo a value",
        json!({
            "type": "object"
        }),
        |_arguments, _cancellation| async {
            Err(ToolError::ExecutionFailed("should not execute".to_owned()))
        },
    ));
    let agent = Agent::new(model, tools, AgentConfig::default());

    let result = agent
        .run(request(), CancellationToken::new())
        .await
        .expect("invalid arguments are recoverable");

    assert!(matches!(
        &result.history[2],
        ChatMessage::Tool { content, .. }
            if content.contains("invalid arguments for tool echo")
    ));
}

#[tokio::test]
async fn default_budget_allows_long_tasks() {
    for streaming in [false, true] {
        let responses = (0..40)
            .map(|i| assistant_tool_call(&format!("call-{i}"), "missing", "{}"))
            .chain(std::iter::once(assistant_text("finished")));
        let agent = Agent::new(
            MockModel::new(responses),
            ToolRegistry::new(),
            AgentConfig::default(),
        );
        let result = if streaming {
            agent
                .run_with_events(request(), CancellationToken::new(), |_| {})
                .await
        } else {
            agent
                .run(request(), CancellationToken::new())
                .await
        }
        .unwrap();

        assert_eq!(result.turns, 41);
        assert_eq!(result.tool_calls, 40);
    }
}

#[tokio::test]
async fn max_turns_stops_an_unfinished_loop() {
    let model = MockModel::new([
        assistant_tool_call("call-1", "missing", "{}"),
        assistant_tool_call("call-2", "missing", "{}"),
    ]);
    let agent = Agent::new(
        model,
        ToolRegistry::new(),
        AgentConfig {
            max_turns: 1,
            max_tool_calls: 10,
            ..AgentConfig::default()
        },
    );

    let error = agent
        .run(request(), CancellationToken::new())
        .await
        .expect_err("turn budget must stop the loop");

    assert!(matches!(error, AgentError::MaxTurns));
}

#[tokio::test]
async fn max_tool_calls_stops_tool_execution() {
    let first = ToolCall {
        purpose: None,
        metadata: None,
        id: "call-1".to_owned(),
        kind: "function".to_owned(),
        function: FunctionCall {
            name: "missing".to_owned(),
            arguments: "{}".to_owned(),
        },
    };
    let second = ToolCall {
        metadata: None,
        id: "call-2".to_owned(),
        ..first.clone()
    };
    let model = MockModel::new([ChatCompletion {
        id: None,
        model: Some("mock".to_owned()),
        message: ChatMessage::Assistant {
            content: None,
            tool_calls: vec![first, second],
        },
        finish_reason: Some("tool_calls".to_owned()),
        usage: None,
    }]);
    let journal = std::sync::Arc::new(FailingJournal {
        checkpoints: Default::default(),
        fail_start: false,
        fail_receipt: false,
    });
    let agent = Agent::new(
        model,
        ToolRegistry::new(),
        AgentConfig {
            max_turns: 4,
            max_tool_calls: 1,
            ..AgentConfig::default()
        },
    )
    .with_execution_journal(journal.clone());

    let error = agent
        .run(request(), CancellationToken::new())
        .await
        .expect_err("tool budget must stop the loop");

    assert!(matches!(error, AgentError::MaxToolCalls));
    assert!(
        journal
            .checkpoints
            .lock()
            .unwrap()
            .iter()
            .all(|checkpoint| {
                !checkpoint
                    .history
                    .iter()
                    .any(|message| {
                        matches!(
                            message,
                            ChatMessage::Assistant { tool_calls, .. } if !tool_calls.is_empty()
                        )
                    })
            })
    );
}

#[tokio::test]
async fn agent_event_channel_reports_stream_and_terminal_state() {
    let model = MockModel::new([assistant_text("hello back")]);
    let agent = Agent::new(model, ToolRegistry::new(), AgentConfig::default());
    let (sender, mut receiver) = mpsc::channel(16);

    agent
        .run_with_agent_events(request(), CancellationToken::new(), sender)
        .await
        .expect("agent completes");

    let mut events = Vec::new();

    while let Some(event) = receiver.recv().await {
        events.push(event);
    }

    assert!(matches!(
        events.first(),
        Some(AgentEvent::TurnStarted { turn: 1 })
    ));
    assert!(events.iter().any(|event| {
        matches!(
            event,
            AgentEvent::Model(StreamEvent::TextDelta(text)) if text == "hello back"
        )
    }));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AgentEvent::Completed { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn cancellation_is_checked_before_first_model_call() {
    let model = MockModel::new([assistant_text("never returned")]);
    let agent = Agent::new(model.clone(), ToolRegistry::new(), AgentConfig::default());
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    let error = agent
        .run(request(), cancellation)
        .await
        .expect_err("cancelled agent must stop");

    assert!(matches!(error, AgentError::Cancelled));
    assert!(model.requests().is_empty());
}

#[test]
fn file_material_survives_text_only_projection_without_requesting_vision() {
    use orbit_model::types::Attachment;
    let file = Attachment::File {
        name: "sample.txt".into(),
        path: "/workspace/sample.txt".into(),
        text: Some("  generated material\nsecond line  ".into()),
        lease: None,
    };
    let history = vec![ChatMessage::user_with_attachments(
        "inspect",
        vec![file.clone()],
    )];
    for vision in [false, true] {
        let projected = super::model_messages(&history, vision);
        let ChatMessage::User {
            content,
            attachments,
        } = &projected[0]
        else {
            panic!("user expected")
        };
        assert!(content.contains("  generated material\nsecond line  "));
        assert!(content.contains("sample.txt"));
        assert!(!content.contains("Image omitted"));
        assert!(attachments.is_empty());
    }
    assert_eq!(
        history[0],
        ChatMessage::user_with_attachments("inspect", vec![file])
    );
}
