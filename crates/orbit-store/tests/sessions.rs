use orbit_agent::events::AgentEvent;
use orbit_agent::tool::FileChange;
use orbit_model::types::{ChatMessage, StreamEvent, ToolCall};
use orbit_store::store::Store;
use orbit_store::types::{MemoryCategory, ProjectPlugin, TaskStatus};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

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
        call: ToolCall::function("write-1", "write_file", serde_json::json!({})),
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
        call: ToolCall::function(
            "command-1",
            "run_command",
            serde_json::json!({ "command": command }),
        ),
        content: serde_json::json!({ "exit_code": exit_code, "timed_out": false, "cancelled": false, "elapsed_ms": 42 })
        .to_string(),
        changes: Vec::new(),
    }
}

#[tokio::test]
async fn restart_preserves_event_order_and_successful_context() {
    let db = Database::new();
    let store = Store::open(db.path()).await.unwrap();
    let mut writer = store
        .create(db.0.to_string_lossy().into_owned(), "mock".into(), true)
        .await
        .unwrap();
    let id = writer.id().to_owned();
    writer.begin("你好".into(), "mock".into()).await.unwrap();
    writer.append(vec![text("一"), text("二")]).await.unwrap();
    let history = vec![ChatMessage::user("你好"), ChatMessage::assistant("一二三")];
    writer
        .finish("completed", vec![text("三")], Some(history.clone()))
        .await
        .unwrap();
    writer
        .begin("cancel me".into(), "mock".into())
        .await
        .unwrap();
    writer
        .finish("cancelled", vec![text("partial")], None)
        .await
        .unwrap();
    drop(writer);
    drop(store);
    let store = Store::open(db.path()).await.unwrap();
    let snapshot = store.load(&id).await.unwrap();

    assert_eq!(snapshot.history, history);
    assert_eq!(snapshot.runs[0].events.len(), 3);

    for (event, expected) in snapshot.runs[0].events.iter().zip(["一", "二", "三"]) {
        assert!(matches!(event, AgentEvent::Model(StreamEvent::TextDelta(t)) if t == expected));
    }

    assert_eq!(snapshot.runs[1].status, "cancelled");
    assert!(snapshot.runs.iter().all(|run| run.elapsed_ms.is_some()));
    assert_eq!(store.list().await.unwrap()[0].title, "你好");
}

#[tokio::test]
async fn workspace_change_ledger_keeps_only_successful_write_receipts() {
    let db = Database::new();
    let store = Store::open(db.path()).await.unwrap();
    let mut writer = store
        .create(db.0.to_string_lossy().into_owned(), "mock".into(), true)
        .await
        .unwrap();
    let id = writer.id().to_owned();
    writer.begin("change".into(), "mock".into()).await.unwrap();
    writer
        .finish(
            "completed",
            vec![
                write_receipt("src/lib.rs", Some("old"), "new"),
                command_receipt("cargo test", 0),
            ],
            Some(vec![ChatMessage::user("change")]),
        )
        .await
        .unwrap();
    let changes = store.workspace_changes(&id).await.unwrap();

    assert_eq!(changes.len(), 1);
    assert_eq!(changes[0].tool, "write_file");
    assert_eq!(changes[0].change.before.as_deref(), Some("old"));
    assert_eq!(changes[0].change.after, "new");
    assert!(!changes[0].undone);

    store.record_workspace_undo(&id, &changes[0]).await.unwrap();

    assert!(store.workspace_changes(&id).await.unwrap()[0].undone);

    let baseline = serde_json::json!({
        "branch": { "oid": "abc" },
        "entries": []
    });
    store.save_git_baseline(&id, &baseline).await.unwrap();

    assert_eq!(store.git_baseline(&id).await.unwrap(), Some(baseline));

    let verification = store.verification_records(&id).await.unwrap();

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
        .create(db.0.to_string_lossy().into_owned(), "mock".into(), true)
        .await
        .unwrap();
    let id = writer.id().to_owned();
    writer
        .begin("unfinished".into(), "mock".into())
        .await
        .unwrap();
    writer.append(vec![text("partial")]).await.unwrap();

    assert!(other.acquire(&id).await.is_err());
    assert_eq!(other.load(&id).await.unwrap().runs[0].status, "running");

    drop(writer);
    let _writer = other.acquire(&id).await.unwrap();
    let snapshot = other.load(&id).await.unwrap();

    assert_eq!(snapshot.runs[0].status, "interrupted");
    assert_eq!(snapshot.runs[0].events.len(), 1);
    assert!(snapshot.history.is_empty());
    assert!(other.acquire("../escape").await.is_err());
}

#[tokio::test]
async fn invalid_finish_keeps_run_recoverable_and_migration_can_be_replayed() {
    let db = Database::new();
    let store = Store::open(db.path()).await.unwrap();
    let mut writer = store
        .create(db.0.to_string_lossy().into_owned(), "mock".into(), true)
        .await
        .unwrap();
    writer.begin("prompt".into(), "mock".into()).await.unwrap();

    assert!(writer.finish("completed", vec![], None).await.is_err());
    assert!(writer.finish("failed", vec![], Some(vec![])).await.is_err());
    assert_eq!(
        store.load(writer.id()).await.unwrap().runs[0].status,
        "running"
    );

    writer.finish("failed", vec![], None).await.unwrap();
    drop(writer);
    drop(store);
    let connection = rusqlite::Connection::open(db.path()).unwrap();
    connection.execute_batch("PRAGMA user_version=14").unwrap();
    drop(connection);

    assert!(Store::open(db.path()).await.is_ok());
}

#[tokio::test]
async fn rejects_a_database_from_a_newer_schema() {
    let db = Database::new();
    let store = Store::open(db.path()).await.unwrap();
    drop(store);
    let connection = rusqlite::Connection::open(db.path()).unwrap();
    connection.execute_batch("PRAGMA user_version=17").unwrap();
    drop(connection);

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
        .create(db.0.to_string_lossy().into_owned(), "mock".into(), true)
        .await
        .unwrap();
    let id = writer.id().to_owned();
    let usage = store.session_storage_usage().await.unwrap();

    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0].id, id);
    assert!(usage[0].payload_bytes > 0);

    writer.begin("prompt".into(), "mock".into()).await.unwrap();
    writer.append(vec![text("stored event")]).await.unwrap();
    writer.finish("failed", Vec::new(), None).await.unwrap();
    let with_event = store.session_storage_usage().await.unwrap();

    assert!(with_event[0].payload_bytes > usage[0].payload_bytes);

    for extension in ["workspaces", "command-output", "artifacts"] {
        let sidecar = db.path().with_extension(extension).join(&id);
        std::fs::create_dir_all(&sidecar).unwrap();
        std::fs::write(sidecar.join("retained"), b"data").unwrap();
    }

    assert!(store.delete_session(&id).await.is_err());
    assert!(store.load(&id).await.is_ok());

    drop(writer);

    assert!(store.delete_session(&id).await.unwrap());
    assert!(store.load(&id).await.is_err());
    assert!(store.session_storage_usage().await.unwrap().is_empty());

    for extension in ["workspaces", "command-output", "artifacts"] {
        assert!(!db.path().with_extension(extension).join(&id).exists());
    }

    assert!(!store.delete_session(&id).await.unwrap());

    store.reclaim_unused_space().await.unwrap();
}

#[tokio::test]
async fn finalization_rolls_back_events_status_and_context_together() {
    let db = Database::new();
    let store = Store::open(db.path()).await.unwrap();
    let mut writer = store
        .create(db.0.to_string_lossy().into_owned(), "mock".into(), true)
        .await
        .unwrap();
    writer.begin("prompt".into(), "mock".into()).await.unwrap();
    let connection = rusqlite::Connection::open(db.path()).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_history BEFORE UPDATE OF history ON sessions BEGIN SELECT RAISE(ABORT, 'injected write failure'); END;").unwrap();

    assert!(
        writer
            .finish(
                "completed",
                vec![text("final")],
                Some(vec![ChatMessage::user("prompt")])
            )
            .await
            .is_err()
    );

    let snapshot = store.load(writer.id()).await.unwrap();

    assert!(snapshot.history.is_empty());
    assert!(snapshot.runs[0].events.is_empty());
    assert_eq!(snapshot.runs[0].status, "running");

    connection
        .execute_batch("DROP TRIGGER reject_history")
        .unwrap();
    writer
        .finish(
            "completed",
            vec![text("final")],
            Some(vec![ChatMessage::user("prompt")]),
        )
        .await
        .unwrap();

    assert_eq!(
        store.load(writer.id()).await.unwrap().runs[0].events.len(),
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
    let project = store.resolve_project(&old).await.unwrap();
    let mut writer = store
        .create(old.to_str().unwrap().into(), "mock".into(), true)
        .await
        .unwrap();
    let session = writer.id().to_owned();
    writer
        .begin("remember".into(), "mock".into())
        .await
        .unwrap();
    writer
        .finish(
            "completed",
            vec![text("answer")],
            Some(vec![ChatMessage::user("remember")]),
        )
        .await
        .unwrap();
    drop(writer);

    assert_eq!(std::fs::read_dir(&old).unwrap().count(), 1);

    std::fs::rename(&old, &moved).unwrap();
    let renamed = store.resolve_project(&moved).await.unwrap();

    if std::fs::metadata(&moved).unwrap().created().is_ok() && cfg!(any(unix, windows)) {
        assert_eq!(renamed.id, project.id);
        assert_eq!(
            store.load(&session).await.unwrap().session.workspace,
            moved.to_str().unwrap()
        );
    } else {
        store.link_project(&project.id, &moved).await.unwrap();
    }

    std::fs::create_dir(&copy).unwrap();
    std::fs::copy(moved.join("user.txt"), copy.join("user.txt")).unwrap();

    assert_ne!(store.resolve_project(&copy).await.unwrap().id, project.id);

    std::fs::create_dir(&old).unwrap();

    assert_ne!(store.resolve_project(&old).await.unwrap().id, project.id);

    std::fs::write(moved.join("user.txt"), "changed").unwrap();

    assert_eq!(store.resolve_project(&moved).await.unwrap().id, project.id);
    assert_eq!(store.load(&session).await.unwrap().runs[0].events.len(), 1);
    assert_eq!(std::fs::read_dir(&moved).unwrap().count(), 1);
}

#[tokio::test]
async fn project_plugins_are_central_stable_and_independent_from_market_cache() {
    let db = Database::new();
    let store = Store::open(db.path()).await.unwrap();
    let original = db.0.join("plugin-project");
    let renamed = db.0.join("renamed-plugin-project");
    std::fs::create_dir(&original).unwrap();
    let project = store.resolve_project(&original).await.unwrap();
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
    assert_eq!(store.project_plugins(&project.id).await.unwrap().len(), 1);
    assert_eq!(std::fs::read_dir(&original).unwrap().count(), 0);

    std::fs::rename(&original, &renamed).unwrap();
    let same_project = store.resolve_project(&renamed).await.unwrap();

    assert_eq!(same_project.id, project.id);

    let plugins = store.project_plugins(&same_project.id).await.unwrap();

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
    let project = store.resolve_project(&old).await.unwrap();
    let writer = store
        .create(old.to_str().unwrap().into(), "mock".into(), true)
        .await
        .unwrap();
    let session = writer.id().to_owned();

    assert!(store.link_project(&project.id, &new).await.is_err());

    std::fs::remove_dir(&old).unwrap();

    assert!(store.link_project(&project.id, &new).await.is_err());
    assert_eq!(
        store.load(&session).await.unwrap().session.workspace,
        old.to_str().unwrap()
    );

    drop(writer);
    let empty = store.resolve_project(&new).await.unwrap();

    assert_ne!(empty.id, project.id);

    store.link_project(&project.id, &new).await.unwrap();

    assert_eq!(store.resolve_project(&new).await.unwrap().id, project.id);
    assert_eq!(
        store.load(&session).await.unwrap().session.workspace,
        new.to_str().unwrap()
    );

    let destination = db.0.join("populated");
    std::fs::create_dir(&destination).unwrap();
    let mut other = store
        .create(destination.to_str().unwrap().into(), "mock".into(), true)
        .await
        .unwrap();
    other
        .begin("keep separate".into(), "mock".into())
        .await
        .unwrap();
    other.finish("failed", vec![], None).await.unwrap();
    drop(other);
    std::fs::remove_dir(&new).unwrap();

    assert!(store.link_project(&project.id, &destination).await.is_err());
}

#[tokio::test]
async fn schema_one_migration_preserves_sessions_runs_and_context() {
    let db = Database::new();
    std::fs::create_dir_all(&db.0).unwrap();
    let connection = rusqlite::Connection::open(db.path()).unwrap();
    connection.execute_batch("CREATE TABLE sessions (id TEXT PRIMARY KEY, workspace TEXT NOT NULL, model TEXT NOT NULL, demo INTEGER NOT NULL, title TEXT NOT NULL DEFAULT '', updated_at INTEGER NOT NULL DEFAULT 0, history TEXT NOT NULL DEFAULT '[]');
    CREATE TABLE runs (id INTEGER PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id), prompt TEXT NOT NULL, status TEXT NOT NULL);
    CREATE TABLE events (id INTEGER PRIMARY KEY, run_id INTEGER NOT NULL REFERENCES runs(id), payload TEXT NOT NULL);
    PRAGMA user_version=1;").unwrap();
    let id = "a".repeat(32);
    let history = vec![ChatMessage::user("old conversation")];
    connection
        .execute(
            "INSERT INTO sessions(id,workspace,model,demo,history) VALUES(?1,?2,'mock',1,?3)",
            rusqlite::params![
                id,
                db.0.to_str().unwrap(),
                serde_json::to_string(&history).unwrap()
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO runs VALUES(1,?1,'old conversation','completed')",
            [&id],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO events VALUES(1,1,?1)",
            [serde_json::to_string(&text("old answer")).unwrap()],
        )
        .unwrap();
    drop(connection);
    let store = Store::open(db.path()).await.unwrap();
    let snapshot = store.load(&id).await.unwrap();

    assert_eq!(snapshot.history, history);
    assert_eq!(snapshot.runs[0].events.len(), 1);
    assert_eq!(
        store.resolve_project(&db.0).await.unwrap().id,
        snapshot.session.project_id
    );

    drop(store);
    let reopened = Store::open(db.path()).await.unwrap();

    assert_eq!(
        reopened.load(&id).await.unwrap().session.project_id,
        snapshot.session.project_id
    );
}

#[tokio::test]
async fn project_memory_migration_keeps_only_unambiguous_session_ownership() {
    let db = Database::new();
    std::fs::create_dir_all(&db.0).unwrap();
    let connection = rusqlite::Connection::open(db.path()).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE projects (id TEXT PRIMARY KEY);
             CREATE TABLE sessions (id TEXT PRIMARY KEY, project_id TEXT NOT NULL);
             CREATE TABLE project_memories (
                id INTEGER PRIMARY KEY,
                project_id TEXT NOT NULL,
                category TEXT NOT NULL,
                title TEXT NOT NULL,
                content TEXT NOT NULL,
                source_session_id TEXT,
                active INTEGER NOT NULL DEFAULT 1,
                created_at INTEGER NOT NULL DEFAULT 1,
                updated_at INTEGER NOT NULL DEFAULT 1
             );
             CREATE TABLE project_plugins (
                project_id TEXT NOT NULL,
                marketplace_source TEXT NOT NULL,
                plugin_name TEXT NOT NULL,
                package_source TEXT NOT NULL,
                version TEXT,
                enabled_at INTEGER NOT NULL DEFAULT 1,
                PRIMARY KEY(project_id,marketplace_source,plugin_name)
             );
             PRAGMA user_version=9;",
        )
        .unwrap();
    connection
        .execute("INSERT INTO projects(id) VALUES('many'),('one')", [])
        .unwrap();
    connection
        .execute(
            "INSERT INTO sessions(id,project_id) VALUES
             ('first','many'),('second','many'),('only','one')",
            [],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO project_memories(project_id,category,title,content,source_session_id)
             VALUES
             ('many','fact','Attributed','belongs to first','first'),
             ('many','fact','Ambiguous','must remain archived',NULL),
             ('one','fact','Single session','may migrate safely',NULL)",
            [],
        )
        .unwrap();
    drop(connection);

    let store = Store::open(db.path()).await.unwrap();
    let first = store.session_memories("first", true).await.unwrap();

    assert_eq!(first.len(), 1);
    assert_eq!(first[0].title, "Attributed");
    assert!(
        store
            .session_memories("second", true)
            .await
            .unwrap()
            .is_empty()
    );

    let only = store.session_memories("only", true).await.unwrap();

    assert_eq!(only.len(), 1);
    assert_eq!(only[0].title, "Single session");
}

#[tokio::test]
async fn compaction_audit_survives_failure_without_publishing_failed_context() {
    use orbit_agent::memory::{CompactionMode, CompactionRecord, FileActivity, TaskSummary};

    let db = Database::new();
    let store = Store::open(db.path()).await.unwrap();
    let mut writer = store
        .create(db.0.to_string_lossy().into_owned(), "mock".into(), true)
        .await
        .unwrap();
    let id = writer.id().to_owned();
    writer.begin("initial".into(), "mock".into()).await.unwrap();
    let original = vec![ChatMessage::user("initial"), ChatMessage::assistant("done")];
    writer
        .finish("completed", vec![], Some(original.clone()))
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
        before_tokens: 9000,
        after_tokens: 3000,
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

    writer.finish("failed", vec![], None).await.unwrap();
    drop(writer);
    drop(store);
    let store = Store::open(db.path()).await.unwrap();
    let records = store.compactions(&id).await.unwrap();

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].record, record);
    assert_eq!(records[0].run_status, "failed");
    assert_eq!(store.load(&id).await.unwrap().history, original);

    let mut writer = store.acquire(&id).await.unwrap();
    writer.begin("retry".into(), "mock".into()).await.unwrap();
    let connection = rusqlite::Connection::open(db.path()).unwrap();
    connection.execute_batch("CREATE TRIGGER reject_history BEFORE UPDATE OF history ON sessions BEGIN SELECT RAISE(ABORT,'injected'); END;").unwrap();

    assert!(
        writer
            .finish(
                "completed",
                vec![AgentEvent::CompactionRecorded { record }],
                Some(original)
            )
            .await
            .is_err()
    );
    assert_eq!(store.compactions(&id).await.unwrap().len(), 1);
    assert!(store.compactions("missing").await.is_err());
}

#[tokio::test]
async fn project_tasks_and_session_memories_stay_isolated_without_workspace_files() {
    let db = Database::new();
    let store = Store::open(db.path()).await.unwrap();
    let old = db.0.join("old");
    let moved = db.0.join("moved");
    std::fs::create_dir(&old).unwrap();
    std::fs::write(old.join("user-file.txt"), "only user content").unwrap();
    let project = store.resolve_project(&old).await.unwrap();
    let first_writer = store
        .create(old.to_string_lossy().into_owned(), "mock".into(), false)
        .await
        .unwrap();
    let first_session = first_writer.id().to_owned();
    let second_writer = store
        .create(old.to_string_lossy().into_owned(), "mock".into(), false)
        .await
        .unwrap();
    let second_session = second_writer.id().to_owned();
    drop(first_writer);
    drop(second_writer);

    let parent = store
        .create_task(
            &project.id,
            None,
            "Ship the project overview".into(),
            "Make the plan visible in every session.".into(),
            10,
        )
        .await
        .unwrap();
    let child = store
        .create_task(
            &project.id,
            Some(parent.id),
            "Add the task command".into(),
            String::new(),
            5,
        )
        .await
        .unwrap();
    let updated = store
        .update_task(
            &project.id,
            child.id,
            None,
            None,
            Some(TaskStatus::InProgress),
            None,
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
    assert_eq!(store.tasks(&project.id, false).await.unwrap().len(), 2);
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
    let renamed = store.resolve_project(&moved).await.unwrap();

    if renamed.id != project.id {
        store.link_project(&project.id, &moved).await.unwrap();
    }

    let tasks = store.tasks(&project.id, false).await.unwrap();

    assert_eq!(
        tasks
            .iter()
            .find(|task| task.id == parent.id)
            .unwrap()
            .parent_id,
        None
    );
    assert_eq!(
        store.session_memories(&first_session, false).await.unwrap()[0].id,
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
        .create(db.0.display().to_string(), "mock".into(), false)
        .await
        .unwrap();
    let id = writer.id().to_owned();
    writer.begin("repair".into(), "mock".into()).await.unwrap();
    let journal = writer.execution_journal().unwrap();
    let calls: Vec<_> = (0..3)
        .map(|i| ToolCall::function(i.to_string(), "write_file", serde_json::json!({})))
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
                in_flight: None,
                attachments: vec![],
            },
            Some(AgentEvent::ToolFinished {
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
                in_flight: Some("1".into()),
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
    assert!(
        matches!(&restored.history[3], ChatMessage::Tool {content,..} if content.contains("UNKNOWN"))
    );
    assert!(
        matches!(&restored.history[4], ChatMessage::Tool {content,..} if content.contains("NOT executed"))
    );
    assert_eq!(restored.recovery.unwrap().unresolved_tools, 2);
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
    writer.finish("cancelled", vec![], None).await.unwrap();

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
        .finish("completed", vec![], Some(complete.clone()))
        .await
        .unwrap();

    assert_eq!(store.load(&id).await.unwrap().history, complete);
    assert!(store.load(&id).await.unwrap().recovery.is_none());
    assert!(
        old_journal
            .save(
                ExecutionCheckpoint {
                    history,
                    turns: 1,
                    tool_calls: 2,
                    in_flight: None,
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
        .create(db.0.display().to_string(), "mock".into(), false)
        .await
        .unwrap();
    let id = writer.id().to_owned();
    let baseline = WorkspaceFingerprint {
        digest: "first".into(),
        ..Default::default()
    };
    store.save_workspace_baseline(&id, &baseline).await.unwrap();
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
        store.workspace_baseline(&id).await.unwrap().unwrap(),
        baseline
    );

    let other = store
        .create(db.0.display().to_string(), "mock".into(), false)
        .await
        .unwrap();

    assert!(
        store
            .workspace_baseline(other.id())
            .await
            .unwrap()
            .is_none()
    );

    writer.begin("verify".into(), "mock".into()).await.unwrap();
    let evidence = VerificationEvidence {
        input: baseline,
        output: None,
        platform: "linux-x86_64".into(),
        isolation: "bubblewrap".into(),
        rust_toolchain: None,
        writable_snapshot: false,
        persistent_scratch: true,
        mounts: Vec::new(),
    };
    writer
        .finish(
            "failed",
            vec![
                AgentEvent::VerificationRecorded {
                    call_id: "command-1".into(),
                    evidence: evidence.clone(),
                },
                command_receipt("cargo test", 1),
            ],
            None,
        )
        .await
        .unwrap();
    let records = store.verification_records(&id).await.unwrap();

    assert_eq!(records[0].evidence, Some(evidence));
    assert_eq!(records[0].exit_code, Some(1));
    assert!(
        store
            .verification_records(other.id())
            .await
            .unwrap()
            .is_empty()
    );
}
