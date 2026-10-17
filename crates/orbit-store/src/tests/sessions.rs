use super::support;
use super::support::insert;
use crate::database::sqlite;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use sea_orm_migration::prelude::*;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::store::Store;
use crate::types::{MemoryCategory, ProjectPlugin, TaskStatus};
use orbit_agent::events::AgentEvent;
use orbit_agent::tool::FileChange;
use orbit_model::types::{ChatMessage, StreamEvent, ToolCall};

static NEXT: AtomicU64 = AtomicU64::new(0);

#[tokio::test]
async fn incremental_refresh_preserves_receipts_and_detects_stale_model_context() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("sessions.db"))
        .await
        .unwrap();
    let mut writer = store
        .create(
            dir.path()
                .to_string_lossy()
                .into_owned(),
            "model".into(),
        )
        .await
        .unwrap();
    let id = writer.id().to_owned();
    let mut snapshot = store.load(&id).await.unwrap();
    for turn in 0..24 {
        assert!(
            store
                .matches_snapshot(&snapshot)
                .await
                .unwrap()
        );
        writer
            .begin(format!("question {turn}"), "model".into())
            .await
            .unwrap();
        let history = vec![ChatMessage::assistant(format!("answer {turn}"))];
        writer
            .finish(
                &crate::types::RunCompletion::Completed { history },
                vec![
                    text(&format!("answer {turn}")),
                    command_receipt("cargo test", 0),
                ],
            )
            .await
            .unwrap();
        assert!(
            !store
                .matches_snapshot(&snapshot)
                .await
                .unwrap()
        );
        store
            .refresh(&mut snapshot)
            .await
            .unwrap();
        assert_eq!(snapshot.runs.len(), turn + 1);
        assert_eq!(snapshot.runs[0].prompt, "question 0");
        assert!(
            store
                .matches_snapshot(&snapshot)
                .await
                .unwrap()
        );
    }
    assert_eq!(
        serde_json::to_value(&snapshot).unwrap(),
        serde_json::to_value(store.load(&id).await.unwrap()).unwrap()
    );
    // Metadata and an idle context append must be visible even without a new run.
    writer
        .set_model_profile("other-model".into(), None)
        .await
        .unwrap();
    writer
        .append_context(ChatMessage::user("follow-up constraint"))
        .await
        .unwrap();
    assert!(
        !store
            .matches_snapshot(&snapshot)
            .await
            .unwrap()
    );
    store
        .refresh(&mut snapshot)
        .await
        .unwrap();
    assert_eq!(snapshot.session.model, "other-model");
    assert_eq!(
        serde_json::to_value(&snapshot).unwrap(),
        serde_json::to_value(store.load(&id).await.unwrap()).unwrap()
    );
}

struct Database(std::path::PathBuf);

impl Database {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "orbit-store-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        )))
    }

    fn path(&self) -> std::path::PathBuf {
        self.0.join("test.db")
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn text(value: &str) -> AgentEvent {
    AgentEvent::Model(StreamEvent::TextDelta(value.into()))
}

fn write_receipt(path: &str, before: Option<&str>, after: &str) -> AgentEvent {
    AgentEvent::ToolFinished {
        outcome: None,
        files: None,
        call: ToolCall::function("write-1", "WriteFile", serde_json::json!({})),
        content: format!(r#"{{"path":"{path}"}}"#),
        changes: vec![FileChange {
            path: path.into(),
            before: before.map(str::to_owned),
            after: after.into(),
        }],
    }
}

fn command_receipt(command: &str, exit_code: i32) -> AgentEvent {
    AgentEvent::ToolFinished {
        outcome: None,
        files: None,
        call: ToolCall::function(
            "command-1",
            "Exec",
            serde_json::json!({
                "command": command
            }),
        ),
        content: serde_json::json!({
            "exit_code": exit_code,
            "timed_out": false,
            "cancelled": false,
            "elapsed_ms": 42
        })
        .to_string(),
        changes: Vec::new(),
    }
}

#[tokio::test]
async fn restart_preserves_event_order_and_successful_context() {
    let db = Database::new();
    let store = Store::open(db.path()).await.unwrap();
    let mut writer = store
        .create(db.0.to_string_lossy().into_owned(), "mock".into())
        .await
        .unwrap();
    let id = writer.id().to_owned();
    writer
        .begin("你好".into(), "mock".into())
        .await
        .unwrap();
    writer
        .append(vec![text("一"), text("二")])
        .await
        .unwrap();
    let history = vec![ChatMessage::user("你好"), ChatMessage::assistant("一二三")];
    writer
        .finish(
            &crate::types::RunCompletion::Completed {
                history: history.clone(),
            },
            vec![text("三")],
        )
        .await
        .unwrap();
    writer
        .begin("cancel me".into(), "mock".into())
        .await
        .unwrap();
    writer
        .finish(
            &crate::types::RunCompletion::Cancelled,
            vec![text("partial")],
        )
        .await
        .unwrap();
    drop(writer);
    drop(store);
    let store = Store::open(db.path()).await.unwrap();
    let snapshot = store.load(&id).await.unwrap();

    assert_eq!(snapshot.history, history);
    let replay: String = snapshot.runs[0]
        .events
        .iter()
        .filter_map(|event| {
            if let AgentEvent::Model(StreamEvent::TextDelta(text)) = event {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(replay, "一二三");

    assert_eq!(snapshot.runs[1].status, "cancelled");
    assert!(
        snapshot
            .runs
            .iter()
            .all(|run| run.elapsed_ms.is_some())
    );
    assert_eq!(store.list().await.unwrap()[0].title, "你好");
}

#[tokio::test]
async fn workspace_change_ledger_keeps_only_successful_write_receipts() {
    let db = Database::new();
    let store = Store::open(db.path()).await.unwrap();
    let mut writer = store
        .create(db.0.to_string_lossy().into_owned(), "mock".into())
        .await
        .unwrap();
    let id = writer.id().to_owned();
    writer
        .begin("change".into(), "mock".into())
        .await
        .unwrap();
    writer
        .finish(
            &crate::types::RunCompletion::Completed {
                history: vec![ChatMessage::user("change")],
            },
            vec![
                write_receipt("src/lib.rs", Some("old"), "new"),
                command_receipt("cargo test", 0),
            ],
        )
        .await
        .unwrap();
    let changes = store
        .workspace_changes(&id)
        .await
        .unwrap();

    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].tool, "WriteFile");
    assert_eq!(changes[0].change.before.as_deref(), Some("old"));
    assert_eq!(changes[0].change.after, "new");
    assert!(!changes[0].undone);

    store
        .record_workspace_undo(&id, &changes[0])
        .await
        .unwrap();

    assert!(
        store
            .workspace_changes(&id)
            .await
            .unwrap()[0]
            .undone
    );

    let baseline = serde_json::json!({
        "branch": {
            "oid": "abc"
        },
        "entries": []
    });
    store
        .save_git_baseline(&id, &baseline)
        .await
        .unwrap();

    assert_eq!(store.git_baseline(&id).await.unwrap(), Some(baseline));

    let verification = store
        .verification_records(&id)
        .await
        .unwrap();

    assert_eq!(verification.len(), 1);
    assert_eq!(verification[0].command, "cargo test");
    assert_eq!(verification[0].exit_code, Some(0));
    assert_eq!(verification[0].elapsed_ms, 42);
}

#[tokio::test]
async fn lease_blocks_second_writer_and_recovers_only_after_release() {
    let db = Database::new();
    let store = Store::open(db.path()).await.unwrap();
    let other = Store::open(db.path()).await.unwrap();
    let mut writer = store
        .create(db.0.to_string_lossy().into_owned(), "mock".into())
        .await
        .unwrap();
    let id = writer.id().to_owned();
    writer
        .begin("unfinished".into(), "mock".into())
        .await
        .unwrap();
    writer
        .append(vec![text("partial")])
        .await
        .unwrap();

    assert!(other.acquire(&id).await.is_err());
    assert_eq!(other.load(&id).await.unwrap().runs[0].status, "running");

    let lock_file = store
        .path()
        .with_extension("session-locks")
        .join(&id);
    assert!(lock_file.is_file());
    drop(writer);
    // A leftover lock file is not a live lease and must not prevent reopening.
    assert!(lock_file.is_file());
    let _writer = other.acquire(&id).await.unwrap();
    let snapshot = other.load(&id).await.unwrap();

    assert_eq!(snapshot.runs[0].status, "interrupted");
    assert_eq!(snapshot.runs[0].events.len(), 1);
    assert!(snapshot.history.is_empty());
    assert!(
        other
            .acquire("../escape")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn rejects_a_database_from_a_newer_schema() {
    let db = Database::new();
    let store = Store::open(db.path()).await.unwrap();
    drop(store);
    let connection = support::connect(db.path()).await;
    let current = sqlite::schema_version(&connection)
        .await
        .unwrap();
    sqlite::set_schema_version(&connection, (current + 1).try_into().unwrap())
        .await
        .unwrap();
    connection.close().await.unwrap();

    let error = Store::open(db.path())
        .await
        .err()
        .expect("newer schema rejected");

    assert!(error.to_string().contains("newer than"), "{error}");
}

#[tokio::test]
async fn deleting_a_closed_session_reclaims_its_private_sidecars() {
    let db = Database::new();
    let store = Store::open(db.path()).await.unwrap();
    let mut writer = store
        .create(db.0.to_string_lossy().into_owned(), "mock".into())
        .await
        .unwrap();
    let id = writer.id().to_owned();
    let usage = store
        .session_storage_usage()
        .await
        .unwrap();

    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0].id, id);
    assert!(usage[0].payload_bytes > 0);

    writer
        .begin("prompt".into(), "mock".into())
        .await
        .unwrap();
    writer
        .append(vec![text("stored event")])
        .await
        .unwrap();
    writer
        .finish(&crate::types::RunCompletion::Failed, Vec::new())
        .await
        .unwrap();
    let with_event = store
        .session_storage_usage()
        .await
        .unwrap();

    assert!(with_event[0].payload_bytes > usage[0].payload_bytes);

    for extension in ["workspaces", "command-output", "artifacts"] {
        let sidecar = db
            .path()
            .with_extension(extension)
            .join(&id);
        std::fs::create_dir_all(&sidecar).unwrap();
        std::fs::write(sidecar.join("retained"), b"data").unwrap();
    }

    assert!(store.delete_session(&id).await.is_err());
    assert!(store.load(&id).await.is_ok());

    drop(writer);

    assert!(store.delete_session(&id).await.unwrap());
    assert!(store.load(&id).await.is_err());
    assert!(
        store
            .session_storage_usage()
            .await
            .unwrap()
            .is_empty()
    );

    assert!(
        !db.path()
            .with_extension("command-output")
            .join(&id)
            .exists()
    );
    // Files produced by commands belong to the user, including old export directories.
    for extension in ["workspaces", "artifacts"] {
        assert_eq!(
            std::fs::read(
                db.path()
                    .with_extension(extension)
                    .join(&id)
                    .join("retained")
            )
            .unwrap(),
            b"data"
        );
    }

    assert!(!store.delete_session(&id).await.unwrap());

    store
        .reclaim_unused_space()
        .await
        .unwrap();
}

#[tokio::test]
async fn finalization_rolls_back_events_status_and_context_together() {
    let db = Database::new();
    let store = Store::open(db.path()).await.unwrap();
    let mut writer = store
        .create(db.0.to_string_lossy().into_owned(), "mock".into())
        .await
        .unwrap();
    writer
        .begin("prompt".into(), "mock".into())
        .await
        .unwrap();
    let connection = support::connect(db.path()).await;
    support::restrict_history(&connection, true).await;

    let error = writer
        .finish(
            &crate::types::RunCompletion::Completed {
                history: vec![ChatMessage::user("prompt")],
            },
            vec![text("final")],
        )
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("CHECK constraint failed"),
        "{error:#}"
    );

    let snapshot = store.load(writer.id()).await.unwrap();

    assert!(snapshot.history.is_empty());
    assert!(snapshot.runs[0].events.is_empty());
    assert_eq!(snapshot.runs[0].status, "running");

    support::restrict_history(&connection, false).await;
    connection.close().await.unwrap();
    writer
        .finish(
            &crate::types::RunCompletion::Completed {
                history: vec![ChatMessage::user("prompt")],
            },
            vec![text("final")],
        )
        .await
        .unwrap();

    assert_eq!(
        store
            .load(writer.id())
            .await
            .unwrap()
            .runs[0]
            .events
            .len(),
        1
    );
}

#[tokio::test]
async fn project_rename_preserves_history_without_markers_and_copy_is_separate() {
    let db = Database::new();
    let store = Store::open(db.path()).await.unwrap();
    let old = db.0.join("original");
    let moved = db.0.join("renamed");
    let copy = db.0.join("copy");
    std::fs::create_dir(&old).unwrap();
    std::fs::write(old.join("user.txt"), "user content").unwrap();
    let project = store
        .resolve_project(&old)
        .await
        .unwrap();
    let mut writer = store
        .create(old.to_str().unwrap().into(), "mock".into())
        .await
        .unwrap();
    let session = writer.id().to_owned();
    writer
        .begin("remember".into(), "mock".into())
        .await
        .unwrap();
    writer
        .finish(
            &crate::types::RunCompletion::Completed {
                history: vec![ChatMessage::user("remember")],
            },
            vec![text("answer")],
        )
        .await
        .unwrap();
    drop(writer);

    assert_eq!(std::fs::read_dir(&old).unwrap().count(), 1);

    std::fs::rename(&old, &moved).unwrap();
    let renamed = store
        .resolve_project(&moved)
        .await
        .unwrap();

    if std::fs::metadata(&moved)
        .unwrap()
        .created()
        .is_ok()
        && cfg!(any(unix, windows))
    {
        assert_eq!(renamed.id, project.id);
        assert_eq!(
            store
                .load(&session)
                .await
                .unwrap()
                .session
                .workspace,
            moved.to_str().unwrap()
        );
    } else {
        store
            .link_project(&project.id, &moved)
            .await
            .unwrap();
    }

    std::fs::create_dir(&copy).unwrap();
    std::fs::copy(moved.join("user.txt"), copy.join("user.txt")).unwrap();

    assert_ne!(
        store
            .resolve_project(&copy)
            .await
            .unwrap()
            .id,
        project.id
    );

    std::fs::create_dir(&old).unwrap();

    assert_ne!(
        store
            .resolve_project(&old)
            .await
            .unwrap()
            .id,
        project.id
    );

    std::fs::write(moved.join("user.txt"), "changed").unwrap();

    assert_eq!(
        store
            .resolve_project(&moved)
            .await
            .unwrap()
            .id,
        project.id
    );
    assert_eq!(
        store.load(&session).await.unwrap().runs[0]
            .events
            .len(),
        1
    );
    assert_eq!(
        std::fs::read_dir(&moved)
            .unwrap()
            .count(),
        1
    );
}

#[tokio::test]
async fn legacy_project_plugin_records_survive_rename_and_can_be_removed() {
    let db = Database::new();
    let store = Store::open(db.path()).await.unwrap();
    let original = db.0.join("plugin-project");
    let renamed = db.0.join("renamed-plugin-project");
    std::fs::create_dir(&original).unwrap();
    let project = store
        .resolve_project(&original)
        .await
        .unwrap();
    let enabled = store
        .enable_project_plugin(ProjectPlugin {
            project_id: project.id.clone(),
            marketplace_source: "https://github.com/example/marketplace.git".into(),
            plugin_name: "reviewer".into(),
            package_source: "plugins/reviewer".into(),
            version: Some("1.2.3".into()),
            marketplace_revision: Some("a".repeat(40)),
            enabled_at: 0,
        })
        .await
        .unwrap();

    assert!(enabled.enabled_at > 0);
    assert_eq!(
        store
            .project_plugins(&project.id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        std::fs::read_dir(&original)
            .unwrap()
            .count(),
        0
    );

    std::fs::rename(&original, &renamed).unwrap();
    let same_project = store
        .resolve_project(&renamed)
        .await
        .unwrap();

    assert_eq!(same_project.id, project.id);

    let plugins = store
        .project_plugins(&same_project.id)
        .await
        .unwrap();

    assert_eq!(plugins[0].plugin_name, "reviewer");
    assert_eq!(plugins[0].package_source, "plugins/reviewer");
    assert!(
        store
            .disable_project_plugin(
                &same_project.id,
                "https://github.com/example/marketplace.git",
                "reviewer",
            )
            .await
            .unwrap()
    );
    assert!(
        store
            .project_plugins(&same_project.id)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        !store
            .disable_project_plugin(
                &same_project.id,
                "https://github.com/example/marketplace.git",
                "reviewer",
            )
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn explicit_link_rejects_copies_active_writers_and_populated_destinations() {
    let db = Database::new();
    let store = Store::open(db.path()).await.unwrap();
    let old = db.0.join("old");
    let new = db.0.join("new");
    std::fs::create_dir(&old).unwrap();
    std::fs::create_dir(&new).unwrap();
    let project = store
        .resolve_project(&old)
        .await
        .unwrap();
    let writer = store
        .create(old.to_str().unwrap().into(), "mock".into())
        .await
        .unwrap();
    let session = writer.id().to_owned();

    assert!(
        store
            .link_project(&project.id, &new)
            .await
            .is_err()
    );

    std::fs::remove_dir(&old).unwrap();

    assert!(
        store
            .link_project(&project.id, &new)
            .await
            .is_err()
    );
    assert_eq!(
        store
            .load(&session)
            .await
            .unwrap()
            .session
            .workspace,
        old.to_str().unwrap()
    );

    drop(writer);
    let empty = store
        .resolve_project(&new)
        .await
        .unwrap();

    assert_ne!(empty.id, project.id);

    store
        .link_project(&project.id, &new)
        .await
        .unwrap();

    assert_eq!(
        store
            .resolve_project(&new)
            .await
            .unwrap()
            .id,
        project.id
    );
    assert_eq!(
        store
            .load(&session)
            .await
            .unwrap()
            .session
            .workspace,
        new.to_str().unwrap()
    );

    let destination = db.0.join("populated");
    std::fs::create_dir(&destination).unwrap();
    let mut other = store
        .create(destination.to_str().unwrap().into(), "mock".into())
        .await
        .unwrap();
    other
        .begin("keep separate".into(), "mock".into())
        .await
        .unwrap();
    other
        .finish(&crate::types::RunCompletion::Failed, vec![])
        .await
        .unwrap();
    drop(other);
    std::fs::remove_dir(&new).unwrap();

    assert!(
        store
            .link_project(&project.id, &destination)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn schema_one_migration_preserves_sessions_runs_and_context() {
    let db = Database::new();
    std::fs::create_dir_all(&db.0).unwrap();
    let connection = support::connect(db.path()).await;
    support::legacy_schema(&connection, 1).await;
    let id = "a".repeat(32);
    let history = vec![ChatMessage::user("old conversation")];
    insert(
        &connection,
        "sessions",
        &[
            ("id", id.clone().into()),
            ("workspace", db.0.to_str().unwrap().into()),
            ("model", "mock".into()),
            ("demo", true.into()),
            (
                "history",
                serde_json::to_string(&history)
                    .unwrap()
                    .into(),
            ),
        ],
    )
    .await
    .unwrap();
    insert(
        &connection,
        "runs",
        &[
            ("id", 1.into()),
            ("session_id", id.clone().into()),
            ("prompt", "old conversation".into()),
            ("status", "completed".into()),
        ],
    )
    .await
    .unwrap();
    insert(
        &connection,
        "events",
        &[
            ("id", 1.into()),
            ("run_id", 1.into()),
            (
                "payload",
                serde_json::to_string(&text("old answer"))
                    .unwrap()
                    .into(),
            ),
        ],
    )
    .await
    .unwrap();
    connection.close().await.unwrap();
    let store = Store::open(db.path()).await.unwrap();
    let snapshot = store.load(&id).await.unwrap();

    assert_eq!(snapshot.history, history);
    assert_eq!(snapshot.runs[0].events.len(), 1);
    assert_eq!(
        store
            .resolve_project(&db.0)
            .await
            .unwrap()
            .id,
        snapshot.session.project_id
    );

    drop(store);
    let reopened = Store::open(db.path()).await.unwrap();

    assert_eq!(
        reopened
            .load(&id)
            .await
            .unwrap()
            .session
            .project_id,
        snapshot.session.project_id
    );
}

#[tokio::test]
async fn project_memory_migration_keeps_only_unambiguous_session_ownership() {
    let db = Database::new();
    std::fs::create_dir_all(&db.0).unwrap();
    let connection = support::connect(db.path()).await;
    support::legacy_schema(&connection, 9).await;
    for project in ["many", "one"] {
        insert(&connection, "projects", &[("id", project.into())])
            .await
            .unwrap();
    }
    for (session, project) in [("first", "many"), ("second", "many"), ("only", "one")] {
        insert(
            &connection,
            "sessions",
            &[
                ("id", session.into()),
                ("project_id", project.into()),
                ("workspace", format!("/{project}").into()),
                ("model", "model".into()),
                ("demo", false.into()),
            ],
        )
        .await
        .unwrap();
    }
    for (project, title, content, source) in [
        ("many", "Attributed", "belongs to first", Some("first")),
        ("many", "Ambiguous", "must remain archived", None),
        ("one", "Single session", "may migrate safely", None),
    ] {
        insert(
            &connection,
            "project_memories",
            &[
                ("project_id", project.into()),
                ("category", "fact".into()),
                ("title", title.into()),
                ("content", content.into()),
                ("source_session_id", source.into()),
            ],
        )
        .await
        .unwrap();
    }
    connection.close().await.unwrap();

    let store = Store::open(db.path()).await.unwrap();
    let first = store
        .session_memories("first", true)
        .await
        .unwrap();

    assert_eq!(first.len(), 1);
    assert_eq!(first[0].title, "Attributed");
    assert!(
        store
            .session_memories("second", true)
            .await
            .unwrap()
            .is_empty()
    );

    let only = store
        .session_memories("only", true)
        .await
        .unwrap();

    assert_eq!(only.len(), 1);
    assert_eq!(only[0].title, "Single session");
}

#[tokio::test]
async fn compaction_audit_survives_failure_without_publishing_failed_context() {
    use orbit_agent::memory::{CompactionMode, CompactionRecord, FileActivity, TaskSummary};

    let db = Database::new();
    let store = Store::open(db.path()).await.unwrap();
    let mut writer = store
        .create(db.0.to_string_lossy().into_owned(), "mock".into())
        .await
        .unwrap();
    let id = writer.id().to_owned();
    writer
        .begin("initial".into(), "mock".into())
        .await
        .unwrap();
    let original = vec![ChatMessage::user("initial"), ChatMessage::assistant("done")];
    writer
        .finish(
            &crate::types::RunCompletion::Completed {
                history: original.clone(),
            },
            vec![],
        )
        .await
        .unwrap();
    let record = CompactionRecord {
        strategy: Default::default(),
        summary_requests: 0,
        summary_input_tokens: 0,
        archive: None,
        version: 1,
        mode: CompactionMode::Automatic,
        source_messages: 7,
        retained_from: 4,
        pinned_user: Some(1),
        before_tokens: 9_000,
        after_tokens: 3_000,
        fallback: false,
        fallback_reason: None,
        summary: TaskSummary {
            goal: "keep the goal".into(),
            constraints: vec![],
            decisions: vec![],
            completed: vec![],
            remaining: vec!["verify".into()],
            verification: vec![],
            uncertainty: vec![],
        },
        files: FileActivity::default(),
    };

    writer
        .begin("long work".into(), "mock".into())
        .await
        .unwrap();
    writer
        .append(vec![AgentEvent::CompactionRecorded {
            record: record.clone(),
        }])
        .await
        .unwrap();

    assert_eq!(
        store.compactions(&id).await.unwrap()[0].run_status,
        "running"
    );

    writer
        .finish(&crate::types::RunCompletion::Failed, vec![])
        .await
        .unwrap();
    drop(writer);
    drop(store);
    let store = Store::open(db.path()).await.unwrap();
    let records = store.compactions(&id).await.unwrap();

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].record, record);
    assert_eq!(records[0].run_status, "failed");
    assert_eq!(store.load(&id).await.unwrap().history, original);

    let mut writer = store.acquire(&id).await.unwrap();
    writer
        .begin("retry".into(), "mock".into())
        .await
        .unwrap();
    let connection = support::connect(db.path()).await;
    support::restrict_history(&connection, true).await;
    connection.close().await.unwrap();

    let error = writer
        .finish(
            &crate::types::RunCompletion::Completed {
                history: vec![ChatMessage::user("replacement context")],
            },
            vec![AgentEvent::CompactionRecorded { record }],
        )
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("CHECK constraint failed"),
        "{error:#}"
    );
    assert_eq!(
        store
            .compactions(&id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(store.load(&id).await.unwrap().history, original);
    assert!(
        store
            .compactions("missing")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn project_tasks_and_session_memories_stay_isolated_without_workspace_files() {
    let db = Database::new();
    let store = Store::open(db.path()).await.unwrap();
    let old = db.0.join("old");
    let moved = db.0.join("moved");
    std::fs::create_dir(&old).unwrap();
    std::fs::write(old.join("user-file.txt"), "only user content").unwrap();
    let project = store
        .resolve_project(&old)
        .await
        .unwrap();
    let first_writer = store
        .create(old.to_string_lossy().into_owned(), "mock".into())
        .await
        .unwrap();
    let first_session = first_writer.id().to_owned();
    let second_writer = store
        .create(old.to_string_lossy().into_owned(), "mock".into())
        .await
        .unwrap();
    let second_session = second_writer.id().to_owned();
    drop(first_writer);
    drop(second_writer);

    let parent = store
        .create_task(
            &project.id,
            crate::tasks::NewTask {
                title: "Ship the project overview".into(),
                details: "Make the plan visible in every session.".into(),
                priority: 10,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let child = store
        .create_task(
            &project.id,
            crate::tasks::NewTask {
                parent_id: Some(parent.id),
                title: "Add the task command".into(),
                priority: 5,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let updated = store
        .update_task(
            &project.id,
            child.id,
            crate::tasks::TaskChanges {
                status: Some(TaskStatus::InProgress),
                ..Default::default()
            },
            crate::tasks::TaskActor::User,
        )
        .await
        .unwrap();

    assert_eq!(updated.status, TaskStatus::InProgress);

    let memory = store
        .remember_in_session(
            &first_session,
            MemoryCategory::Constraint,
            "No project metadata".into(),
            "Orbit must keep its own state outside the user workspace.".into(),
        )
        .await
        .unwrap();

    assert!(memory.active);
    assert_eq!(
        store
            .tasks(&project.id, false)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        store
            .session_memories(&first_session, false)
            .await
            .unwrap()
            .len(),
        1
    );

    let preference = store
        .remember_in_session(
            &first_session,
            MemoryCategory::Preference,
            "Use concise Chinese progress updates".into(),
            "Keep project progress messages short and in Chinese.".into(),
        )
        .await
        .unwrap();

    store
        .remember_in_session(
            &second_session,
            MemoryCategory::Fact,
            "Other conversation".into(),
            "This memory must not be visible to the first conversation.".into(),
        )
        .await
        .unwrap();
    let search = store
        .search_session_memories(&first_session, "Chinese progress", 8)
        .await
        .unwrap();

    assert_eq!(search.first().map(|memory| memory.id), Some(preference.id));
    assert!(
        store
            .search_session_memories(&first_session, "missing phrase", 8)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .session_memories(&second_session, false)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        store
            .set_session_memory_active(&second_session, memory.id, false)
            .await
            .is_err()
    );
    assert_eq!(std::fs::read_dir(&old).unwrap().count(), 1);

    std::fs::rename(&old, &moved).unwrap();
    let renamed = store
        .resolve_project(&moved)
        .await
        .unwrap();

    if renamed.id != project.id {
        store
            .link_project(&project.id, &moved)
            .await
            .unwrap();
    }

    let tasks = store
        .tasks(&project.id, false)
        .await
        .unwrap();

    assert_eq!(
        tasks
            .iter()
            .find(|task| task.id == parent.id)
            .unwrap()
            .parent_id,
        None
    );
    assert_eq!(
        store
            .session_memories(&first_session, false)
            .await
            .unwrap()[0]
            .id,
        preference.id
    );

    let inactive = store
        .set_session_memory_active(&first_session, memory.id, false)
        .await
        .unwrap();

    assert!(!inactive.active);

    store
        .set_session_memory_active(&first_session, preference.id, false)
        .await
        .unwrap();

    assert!(
        store
            .session_memories(&first_session, false)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .session_memories(&first_session, true)
            .await
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        store
            .session_memories(&second_session, false)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn checkpoint_receipts_survive_restart_without_replaying_tools() {
    use orbit_agent::checkpoint::ExecutionCheckpoint;

    let db = Database::new();
    let store = Store::open(db.path()).await.unwrap();
    let mut writer = store
        .create(db.0.display().to_string(), "mock".into())
        .await
        .unwrap();
    let id = writer.id().to_owned();
    writer
        .begin("repair".into(), "mock".into())
        .await
        .unwrap();
    let journal = writer.execution_journal().unwrap();
    let calls: Vec<_> = (0..3)
        .map(|i| ToolCall::function(i.to_string(), "WriteFile", serde_json::json!({})))
        .collect();
    let history = vec![
        ChatMessage::user("repair"),
        ChatMessage::Assistant {
            content: None,
            tool_calls: calls.clone(),
        },
        ChatMessage::tool("0", "saved first file"),
    ];

    journal.record(text("plan"));
    journal
        .save(
            ExecutionCheckpoint {
                history: history.clone(),
                turns: 1,
                tool_calls: 1,
                in_flight: Vec::new(),
                attachments: vec![],
            },
            Some(AgentEvent::ToolFinished {
                outcome: None,
                files: None,
                call: calls[0].clone(),
                content: "saved first file".into(),
                changes: vec![],
            }),
        )
        .await
        .unwrap();
    journal
        .save(
            ExecutionCheckpoint {
                history: history.clone(),
                turns: 1,
                tool_calls: 2,
                in_flight: vec!["1".into()],
                attachments: vec![],
            },
            Some(AgentEvent::ToolStarted {
                call: calls[1].clone(),
            }),
        )
        .await
        .unwrap();
    drop(journal);
    drop(writer);
    drop(store);
    let store = Store::open(db.path()).await.unwrap();
    let mut writer = store.acquire(&id).await.unwrap();
    let restored = store.load(&id).await.unwrap();

    assert_eq!(&restored.history[..3], &history);
    assert!(matches!(
        &restored.history[3],
        ChatMessage::Tool { content, .. } if content.contains("UNKNOWN")
    ));
    assert!(matches!(
        &restored.history[4],
        ChatMessage::Tool { content, .. } if content.contains("NOT executed")
    ));
    assert_eq!(
        restored
            .recovery
            .unwrap()
            .unresolved_tools,
        2
    );
    assert_eq!(restored.runs[0].events.len(), 3);
    assert!(matches!(restored.runs[0].events[0], AgentEvent::Model(_)));
    assert!(matches!(
        restored.runs[0].events[1],
        AgentEvent::ToolFinished { .. }
    ));

    // A restart before the next initial checkpoint must not discard recovery.
    writer
        .begin("continue".into(), "mock".into())
        .await
        .unwrap();
    writer
        .finish(&crate::types::RunCompletion::Cancelled, vec![])
        .await
        .unwrap();

    assert_eq!(store.load(&id).await.unwrap().history, restored.history);

    writer
        .begin("continue".into(), "mock".into())
        .await
        .unwrap();
    let old_journal = writer.execution_journal().unwrap();
    let complete = vec![
        ChatMessage::user("done"),
        ChatMessage::assistant("verified"),
    ];
    writer
        .finish(
            &crate::types::RunCompletion::Completed {
                history: complete.clone(),
            },
            vec![],
        )
        .await
        .unwrap();

    assert_eq!(store.load(&id).await.unwrap().history, complete);
    assert!(
        store
            .load(&id)
            .await
            .unwrap()
            .recovery
            .is_none()
    );
    assert!(
        old_journal
            .save(
                ExecutionCheckpoint {
                    history,
                    turns: 1,
                    tool_calls: 2,
                    in_flight: Vec::new(),
                    attachments: vec![]
                },
                None
            )
            .await
            .is_err()
    );
    assert_eq!(store.load(&id).await.unwrap().history, complete);
}

#[tokio::test]
async fn verification_evidence_and_start_baseline_are_durable_and_session_scoped() {
    use orbit_runtime::verification::{VerificationEvidence, WorkspaceFingerprint};

    let db = Database::new();
    let store = Store::open(db.path()).await.unwrap();
    let mut writer = store
        .create(db.0.display().to_string(), "mock".into())
        .await
        .unwrap();
    let id = writer.id().to_owned();
    let baseline = WorkspaceFingerprint {
        digest: "first".into(),
        ..Default::default()
    };
    store
        .save_workspace_baseline(&id, &baseline)
        .await
        .unwrap();
    store
        .save_workspace_baseline(
            &id,
            &WorkspaceFingerprint {
                digest: "do not replace".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    assert_eq!(
        store
            .workspace_baseline(&id)
            .await
            .unwrap()
            .unwrap(),
        baseline
    );

    let other = store
        .create(db.0.display().to_string(), "mock".into())
        .await
        .unwrap();

    assert!(
        store
            .workspace_baseline(other.id())
            .await
            .unwrap()
            .is_none()
    );

    writer
        .begin("verify".into(), "mock".into())
        .await
        .unwrap();
    let evidence = VerificationEvidence {
        input: baseline,
        output: None,
        platform: "linux-x86_64".into(),
        isolation: "native".into(),
    };

    let mut receipt = command_receipt("synthetic verification", 1);
    if let AgentEvent::ToolFinished { content, .. } = &mut receipt {
        *content = serde_json::json!({
            "exit_code": null, "signal": 15, "core_dumped": false,
            "timed_out": false, "cancelled": false,
            "elapsed": { "secs": 1, "nanos": 250_000_000 }
        })
        .to_string();
    }

    writer
        .finish(
            &crate::types::RunCompletion::Failed,
            vec![
                AgentEvent::VerificationRecorded {
                    call_id: "command-1".into(),
                    evidence: evidence.clone(),
                },
                receipt,
            ],
        )
        .await
        .unwrap();
    let records = store
        .verification_records(&id)
        .await
        .unwrap();

    assert_eq!(records[0].evidence, Some(evidence));
    assert_eq!(records[0].exit_code, None);
    assert_eq!(records[0].signal, Some(15));
    assert!(!records[0].core_dumped);
    assert_eq!(records[0].elapsed_ms, 1_250);
    assert!(
        store
            .verification_records(other.id())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn moving_session_preserves_history_and_scopes_undo_to_the_new_workspace() {
    let directory = tempfile::tempdir().unwrap();
    let old = directory.path().join("old");
    let new = directory.path().join("new");
    std::fs::create_dir(&old).unwrap();
    std::fs::create_dir(&new).unwrap();
    let store = Store::open(directory.path().join("store.db"))
        .await
        .unwrap();
    let mut writer = store
        .create(old.to_string_lossy().into_owned(), "mock".into())
        .await
        .unwrap();
    let id = writer.id().to_owned();
    let history = vec![ChatMessage::user("keep my conversation")];
    writer
        .begin("change old project".into(), "mock".into())
        .await
        .unwrap();
    assert!(
        writer
            .move_to(&new, None)
            .await
            .is_err()
    );
    writer
        .finish(
            &crate::types::RunCompletion::Completed {
                history: history.clone(),
            },
            vec![write_receipt("same.rs", None, "old content")],
        )
        .await
        .unwrap();
    let original = store.load(&id).await.unwrap();
    assert_eq!(
        store
            .workspace_changes(&id)
            .await
            .unwrap()
            .len(),
        1
    );

    let moved = writer
        .move_to(&new, None)
        .await
        .unwrap();

    assert_eq!(moved.id, id);
    assert_ne!(moved.project_id, original.session.project_id);
    assert_eq!(moved.workspace, new.to_string_lossy());
    let restored = store.load(&id).await.unwrap();
    assert_eq!(restored.history, history);
    assert_eq!(restored.runs.len(), original.runs.len());
    assert_eq!(restored.runs[0].events.len(), original.runs[0].events.len());
    assert!(
        store
            .workspace_changes(&id)
            .await
            .unwrap()
            .is_empty()
    );

    writer
        .begin("change new project".into(), "mock".into())
        .await
        .unwrap();
    writer
        .finish(
            &crate::types::RunCompletion::Completed { history },
            vec![write_receipt("same.rs", None, "new content")],
        )
        .await
        .unwrap();

    let changes = store
        .workspace_changes(&id)
        .await
        .unwrap();
    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].change.after, "new content");
}

#[tokio::test]
async fn schema_19_migrates_with_or_without_file_grants_without_losing_sessions() {
    for had_grants in [false, true] {
        let db = Database::new();
        std::fs::create_dir_all(&db.0).unwrap();
        let connection = support::connect(db.path()).await;
        support::legacy_schema(&connection, 19).await;
        if had_grants {
            support::released_migration(&connection, 20).await;
        }
        let id = "a".repeat(32);
        insert(&connection, "projects", &[("id", "project".into())])
            .await
            .unwrap();
        insert(
            &connection,
            "sessions",
            &[
                ("id", id.clone().into()),
                ("project_id", "project".into()),
                ("workspace", db.0.to_str().unwrap().into()),
                ("model", "mock".into()),
                ("demo", false.into()),
                ("title", "saved".into()),
                (
                    "history",
                    serde_json::to_string(&vec![ChatMessage::user("saved prompt")])
                        .unwrap()
                        .into(),
                ),
            ],
        )
        .await
        .unwrap();
        insert(
            &connection,
            "runs",
            &[
                ("id", 1.into()),
                ("session_id", id.clone().into()),
                ("prompt", "preserve my conversation".into()),
                ("status", "completed".into()),
            ],
        )
        .await
        .unwrap();
        insert(
            &connection,
            "events",
            &[
                ("run_id", 1.into()),
                (
                    "payload",
                    serde_json::to_string(&text("saved answer"))
                        .unwrap()
                        .into(),
                ),
            ],
        )
        .await
        .unwrap();
        insert(
            &connection,
            "tui_session_state",
            &[
                ("session_id", id.clone().into()),
                ("permission_mode", "ask".into()),
            ],
        )
        .await
        .unwrap();
        insert(
            &connection,
            "tui_tabs",
            &[("session_id", id.clone().into())],
        )
        .await
        .unwrap();
        if had_grants {
            insert(
                &connection,
                "tui_file_grants",
                &[
                    ("session_id", id.clone().into()),
                    ("path", "existing.txt".into()),
                ],
            )
            .await
            .unwrap();
        }
        connection.close().await.unwrap();

        let store = Store::open(db.path()).await.unwrap();
        let saved = store.load(&id).await.unwrap();
        assert_eq!(saved.runs.len(), 1);
        assert_eq!(saved.runs[0].prompt, "preserve my conversation");
        assert_eq!(saved.runs[0].events.len(), 1);
        assert_eq!(saved.history, vec![ChatMessage::user("saved prompt")]);
        assert_eq!(store.tui_mode(&id).await.unwrap(), "ask");
        assert_eq!(
            store
                .tui_tabs(&saved.session.project_id)
                .await
                .unwrap(),
            vec![id.clone()]
        );
        assert_eq!(
            store
                .tui_file_grants(&id)
                .await
                .unwrap()
                .contains("existing.txt"),
            had_grants
        );
        store
            .grant_tui_file(&id, "new.txt")
            .await
            .unwrap();
        drop(store);
        let reopened = Store::open(db.path()).await.unwrap();
        assert!(
            reopened
                .tui_file_grants(&id)
                .await
                .unwrap()
                .contains("new.txt")
        );
        let connection = support::connect(db.path()).await;
        assert_eq!(
            sqlite::schema_version(&connection)
                .await
                .unwrap(),
            crate::migrations::Migrator::migrations().len() as i64
        );
        connection.close().await.unwrap();
    }
}

#[tokio::test]
async fn write_transactions_serialize_across_independent_store_connections() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("orbit.db");
    let store = Store::open(&path).await.unwrap();
    let project = store
        .resolve_project(dir.path())
        .await
        .unwrap();
    let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(8));
    let mut writers = tokio::task::JoinSet::new();
    for worker in 0..8 {
        let connection = Store::open(&path).await.unwrap();
        let project = project.id.clone();
        let barrier = barrier.clone();
        writers.spawn(async move {
            barrier.wait().await;
            for item in 0..20 {
                connection
                    .create_task(
                        &project,
                        crate::tasks::NewTask::new(format!("task {worker}/{item}")),
                    )
                    .await
                    .unwrap();
            }
        });
    }

    while let Some(result) = writers.join_next().await {
        result.unwrap();
    }
    assert_eq!(
        store
            .tasks(&project.id, true)
            .await
            .unwrap()
            .len(),
        160
    );
}

#[tokio::test]
async fn developer_role_migration_preserves_children_and_queued_messages() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("sessions.db");
    let connection = support::connect(&path).await;
    support::legacy_schema(&connection, 23).await;
    let parent_id = "a".repeat(32);
    let child_id = "b".repeat(32);
    insert(&connection, "projects", &[("id", "project".into())])
        .await
        .unwrap();
    for id in [&parent_id, &child_id] {
        insert(
            &connection,
            "sessions",
            &[
                ("id", id.clone().into()),
                ("project_id", "project".into()),
                ("workspace", root.path().to_str().unwrap().into()),
                ("model", "mock".into()),
                ("demo", false.into()),
            ],
        )
        .await
        .unwrap();
    }
    insert(
        &connection,
        "runs",
        &[
            ("session_id", parent_id.clone().into()),
            ("prompt", "parent".into()),
            ("status", "completed".into()),
        ],
    )
    .await
    .unwrap();
    insert(
        &connection,
        "agent_sessions",
        &[
            ("session_id", child_id.clone().into()),
            ("parent_session_id", parent_id.clone().into()),
            ("role", "explore".into()),
            ("task", "inspect".into()),
        ],
    )
    .await
    .unwrap();
    insert(
        &connection,
        "agent_messages",
        &[
            ("session_id", child_id.clone().into()),
            ("content", "preserve queued input".into()),
        ],
    )
    .await
    .unwrap();
    connection.close().await.unwrap();
    let store = Store::open(path).await.unwrap();
    assert_eq!(
        store
            .subagent_parent(&child_id)
            .await
            .unwrap(),
        Some(parent_id.clone())
    );
    assert_eq!(
        store
            .pending_agent_messages(&child_id)
            .await
            .unwrap()[0]
            .content,
        "preserve queued input"
    );
    let developer = store
        .create_subagent(
            &parent_id,
            "mock".into(),
            "develop".into(),
            "implement".into(),
        )
        .await
        .unwrap();
    assert_eq!(
        store
            .owned_subagent(&parent_id, developer.id())
            .await
            .unwrap()
            .role,
        "develop"
    );
}

#[tokio::test]
async fn resume_history_excludes_drafts_but_keeps_failed_and_untitled_runs() {
    let db = Database::new();
    let store = Store::open(db.path()).await.unwrap();
    let draft = store
        .create(db.0.to_string_lossy().into_owned(), "mock".into())
        .await
        .unwrap();
    draft
        .set_title("named draft".into())
        .await
        .unwrap();
    assert!(
        store
            .list_resumable()
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.list().await.unwrap().len(), 1);
    assert!(
        store
            .load(draft.id())
            .await
            .unwrap()
            .runs
            .is_empty()
    );
    let mut conversation = store
        .create(db.0.to_string_lossy().into_owned(), "mock".into())
        .await
        .unwrap();
    conversation
        .begin("first prompt".into(), "mock".into())
        .await
        .unwrap();
    assert_eq!(
        store.list_resumable().await.unwrap()[0].id,
        conversation.id()
    );
    conversation
        .finish(&crate::types::RunCompletion::Failed, Vec::new())
        .await
        .unwrap();
    let connection = support::connect(db.path()).await;
    crate::entities::sessions::Entity::update_many()
        .col_expr(crate::entities::sessions::Column::Title, Expr::value(""))
        .filter(crate::entities::sessions::Column::Id.eq(conversation.id()))
        .exec(&connection)
        .await
        .unwrap();
    connection.close().await.unwrap();
    let history = store.list_resumable().await.unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].id, conversation.id());
    assert!(history[0].title.is_empty());
}

#[tokio::test]
async fn runtime_recovery_prefers_typed_file_evidence_to_tool_text() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("sessions.db"))
        .await
        .unwrap();
    let mut writer = store
        .create(directory.path().display().to_string(), "model".into())
        .await
        .unwrap();
    writer
        .begin("inspect generated sources".into(), "model".into())
        .await
        .unwrap();
    let receipt = |id: &str, path: &str, files| AgentEvent::ToolFinished {
        outcome: None,
        call: ToolCall::function(id, "ReadFile", serde_json::json!({ "path": path })),
        content: serde_json::json!({ "path": path, "content": "source", "total_lines": 1 })
            .to_string(),
        changes: vec![],
        files,
    };
    writer
        .finish(
            &crate::types::RunCompletion::Completed { history: vec![] },
            vec![
                receipt("old", "legacy.rs", None),
                receipt("empty", "unconfirmed.rs", Some(Default::default())),
                receipt(
                    "typed",
                    "display-only.rs",
                    Some(orbit_agent::memory::FileActivity {
                        read: ["observed.rs".into()].into(),
                        written: Default::default(),
                    }),
                ),
            ],
        )
        .await
        .unwrap();
    let runtime = store
        .load_runtime(writer.id())
        .await
        .unwrap();
    assert_eq!(
        runtime.files.read,
        ["legacy.rs".into(), "observed.rs".into()].into()
    );
    assert!(runtime.files.written.is_empty());
}
