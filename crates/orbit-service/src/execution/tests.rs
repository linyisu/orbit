use super::*;

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
    session.finish("cancelled", None).await.unwrap();
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
        archive: Some(format!("read_history archive_id={id}; search with query")),
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
        images: Vec::new(),
        prompt: "/compact".into(),
        status: "completed".into(),
        elapsed_ms: None,
        events: vec![AgentEvent::CompactionRecorded { record }],
    };
    let failed_retry = Run {
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
            "BEGIN_UNTRUSTED_COMPACTION_RECORD\nread_history archive_id={id}; search with query"
        )),
    ];

    assert_eq!(
        retry_archive_id(&[archive_run.clone(), failed_retry], &projected),
        Some(id)
    );

    session.snapshot.runs = vec![archive_run.clone()];
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

    let later_reply = Run {
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
