use super::*;

#[test]
fn compaction_outcomes_preserve_diagnostic_causes() {
    for error in [
        AgentError::Compaction("history has no completed assistant turn".into()),
        AgentError::CompactionFailed {
            kind: orbit_agent::compaction::CompactionFailureKind::InputBudget,
            message: "retained working set exceeds the configured input budget".into(),
        },
    ] {
        let cause = match &error {
            AgentError::Compaction(message) | AgentError::CompactionFailed { message, .. } => {
                message.clone()
            }
            _ => unreachable!(),
        };
        let RunOutcome::Failed(message) = outcome(Ok(Err(error)), None) else {
            panic!("compaction failure must remain failed");
        };
        assert!(message.contains(&cause));
        assert!(message.contains("原上下文已保留"));
    }
}

#[tokio::test]
async fn compact_retry_restores_the_original_history_after_a_failed_retry() {
    use orbit_agent::memory::{CompactionMode, CompactionRecord, FileActivity, TaskSummary};

    let dir = tempfile::tempdir().unwrap();
    let mut session = SessionState::open(dir.path().join("orbit.db"), None, "mock".into())
        .await
        .unwrap();
    let original = vec![
        ChatMessage::system("old instructions"),
        ChatMessage::runtime(
            orbit_model::types::RuntimeStateKind::ToolPolicy,
            "obsolete policy",
        ),
        ChatMessage::user("original work"),
        ChatMessage::assistant("original answer"),
    ];
    session
        .begin("original work".into(), "mock".into())
        .await
        .unwrap();
    session
        .finish(orbit_store::types::RunCompletion::Cancelled)
        .await
        .unwrap();
    let id = session
        .store
        .archive_context(session.id(), &original)
        .await
        .unwrap();

    let record = CompactionRecord {
        version: 2,
        mode: CompactionMode::Manual,
        source_messages: 3,
        retained_from: 2,
        pinned_user: None,
        before_tokens: 200_000,
        after_tokens: 40_000,
        strategy: CompactionStrategy::ArchiveProjection,
        summary_requests: 1,
        summary_input_tokens: 4_096,
        archive: Some(format!("ReadHistory archive_id={id}; search with query")),
        fallback: true,
        fallback_reason: None,
        summary: TaskSummary {
            goal: String::new(),
            constraints: vec![],
            decisions: vec![],
            completed: vec![],
            remaining: vec![],
            verification: vec![],
            uncertainty: vec![],
        },
        files: FileActivity::default(),
    };
    let archive_run = Run {
        files: Vec::new(),
        images: Vec::new(),
        prompt: "/compact".into(),
        status: "completed".into(),
        elapsed_ms: None,
        events: vec![AgentEvent::CompactionRecorded { record }],
    };
    let failed_retry = Run {
        files: Vec::new(),
        images: Vec::new(),
        prompt: "/compact".into(),
        status: "failed".into(),
        elapsed_ms: None,
        events: vec![],
    };
    let projected = vec![
        ChatMessage::system("current instructions"),
        ChatMessage::runtime(
            orbit_model::types::RuntimeStateKind::ToolPolicy,
            "current policy",
        ),
        ChatMessage::user(format!(
            "BEGIN_UNTRUSTED_COMPACTION_RECORD\nReadHistory archive_id={id}; search with query"
        )),
    ];

    assert_eq!(
        retry_archive_id(&[archive_run.clone(), failed_retry], &projected),
        Some(id)
    );

    let mut snapshot = (*session.snapshot).clone();
    snapshot.runs = vec![archive_run.clone()];
    session.snapshot = snapshot.into();
    let restored = compaction_source_history(&session, &projected)
        .await
        .unwrap();

    assert_eq!(
        restored,
        vec![
            ChatMessage::system("current instructions"),
            ChatMessage::user("original work"),
            ChatMessage::assistant("original answer"),
            ChatMessage::runtime(
                orbit_model::types::RuntimeStateKind::ToolPolicy,
                "current policy"
            ),
        ]
    );
    assert_eq!(
        retry_archive_id(std::slice::from_ref(&archive_run), &[]),
        None
    );

    let mut legacy_run = archive_run.clone();
    let AgentEvent::CompactionRecorded { record } = &mut legacy_run.events[0] else {
        unreachable!();
    };
    record.archive = Some(format!("read_history archive_id={id}; search with query"));
    let legacy_projection = vec![ChatMessage::user(format!(
        "BEGIN_UNTRUSTED_COMPACTION_RECORD\nread_history archive_id={id}; search with query"
    ))];
    assert_eq!(
        retry_archive_id(std::slice::from_ref(&legacy_run), &legacy_projection),
        Some(id)
    );
    assert_eq!(retry_archive_id(&[legacy_run], &projected), None);

    let later_reply = Run {
        files: Vec::new(),
        images: Vec::new(),
        prompt: "new work".into(),
        status: "completed".into(),
        elapsed_ms: None,
        events: vec![],
    };

    assert_eq!(
        retry_archive_id(&[archive_run, later_reply], &projected),
        None
    );
}

#[tokio::test]
async fn failed_compaction_survives_restart_and_archive_recovery_allows_the_next_reply() {
    use orbit_agent::{
        agent::{Agent, AgentConfig},
        context::compact_with_archive,
        memory::{CompactionStrategy, FileActivity},
        model::MockModel,
    };
    use orbit_model::types::ChatCompletion;
    use orbit_store::{store::Store, types::RunCompletion};
    use tokio_util::sync::CancellationToken;

    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("conversation.db");
    let original = vec![
        ChatMessage::system("Keep the generated public API stable."),
        ChatMessage::user("Finish the scanner migration."),
        ChatMessage::assistant("generated historical investigation ".repeat(2_000)),
        ChatMessage::user("Continue with verification."),
    ];
    let store = Store::open(&database).await.unwrap();
    let mut writer = store
        .create(directory.path().display().to_string(), "mock".into())
        .await
        .unwrap();
    let id = writer.id().to_owned();
    writer
        .begin("Finish the scanner migration.".into(), "mock".into())
        .await
        .unwrap();
    writer
        .finish(
            &RunCompletion::Completed {
                history: original.clone(),
            },
            vec![],
        )
        .await
        .unwrap();
    writer
        .begin("/compact".into(), "mock".into())
        .await
        .unwrap();
    let blank = || ChatCompletion {
        id: None,
        model: None,
        message: ChatMessage::assistant(""),
        finish_reason: Some("stop".into()),
        usage: None,
    };
    // A failure without durable retrieval cannot publish any lossy substitute.
    let failed = compact_with_archive(
        &MockModel::new([blank(), blank(), blank()]),
        original.clone(),
        &[],
        8_192,
        1_024,
        CancellationToken::new(),
        &FileActivity::default(),
        None,
    )
    .await;
    assert!(failed.is_err());
    writer
        .finish(
            &RunCompletion::Failed,
            vec![
                outcome(
                    Ok(Err(failed
                        .err()
                        .expect("summary must fail without an archive"))),
                    None,
                )
                .event(),
            ],
        )
        .await
        .unwrap();
    drop(writer);
    drop(store);

    let reopened = Store::open(&database).await.unwrap();
    assert_eq!(
        reopened
            .load(&id)
            .await
            .unwrap()
            .history,
        original
    );
    let mut writer = reopened.acquire(&id).await.unwrap();
    writer
        .begin("/compact".into(), "mock".into())
        .await
        .unwrap();
    let archive = crate::history::SessionArchive::new(reopened.clone(), id.clone());
    let recovered = compact_with_archive(
        &MockModel::new([blank(), blank(), blank()]),
        original.clone(),
        &[],
        8_192,
        1_024,
        CancellationToken::new(),
        &FileActivity::default(),
        Some(&archive),
    )
    .await
    .unwrap();
    assert_eq!(
        recovered.record.strategy,
        CompactionStrategy::ArchiveProjection
    );
    assert!(recovered.record.fallback);
    let archive_id = recovered.record.archive_id().unwrap();
    writer
        .finish(
            &RunCompletion::Completed {
                history: recovered.history.clone(),
            },
            vec![AgentEvent::CompactionRecorded {
                record: recovered.record,
            }],
        )
        .await
        .unwrap();
    drop(writer);
    drop(archive);
    drop(reopened);

    let reopened = Store::open(&database).await.unwrap();
    let snapshot = reopened.load(&id).await.unwrap();
    assert_eq!(snapshot.history, recovered.history);
    assert_eq!(
        reopened
            .load_context_archive(&id, archive_id)
            .await
            .unwrap(),
        original
    );
    let model = MockModel::new([ChatCompletion {
        message: ChatMessage::assistant("Continuing from the saved scanner requirements."),
        ..blank()
    }]);
    let mut writer = reopened.acquire(&id).await.unwrap();
    writer
        .begin("Continue.".into(), "mock".into())
        .await
        .unwrap();
    let mut history = snapshot.history;
    history.push(ChatMessage::user("Continue."));
    let mut request = ChatRequest::new(history);
    request.max_tokens = Some(1_024);
    let result = Agent::new(
        model.clone(),
        ToolRegistry::new(),
        AgentConfig {
            context_tokens: 8_192,
            ..Default::default()
        },
    )
    .with_execution_journal(writer.execution_journal().unwrap())
    .run(request, CancellationToken::new())
    .await
    .unwrap();
    assert_eq!(model.requests().len(), 1);
    assert_eq!(result.tool_calls, 0);
    writer
        .finish(
            &RunCompletion::Completed {
                history: result.history,
            },
            vec![],
        )
        .await
        .unwrap();
    assert_eq!(
        reopened
            .load(&id)
            .await
            .unwrap()
            .runs
            .last()
            .unwrap()
            .status,
        "completed"
    );
}
