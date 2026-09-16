use orbit_agent::events::AgentEvent;
use orbit_model::types::{ChatMessage, StreamEvent};
use orbit_store::store::Store;
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

#[tokio::test]
async fn restart_preserves_event_order_and_successful_context() {
    let db = Database::new();
    let store = Store::open(db.path()).await.unwrap();
    let mut writer = store
        .create("/work".into(), "mock".into(), true)
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
    assert_eq!(store.list().await.unwrap()[0].title, "你好");
}

#[tokio::test]
async fn lease_blocks_second_writer_and_recovers_only_after_release() {
    let db = Database::new();
    let store = Store::open(db.path()).await.unwrap();
    let other = Store::open(db.path()).await.unwrap();
    let mut writer = store
        .create("/work".into(), "mock".into(), true)
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
async fn invalid_finish_keeps_run_recoverable_and_newer_schema_is_rejected() {
    let db = Database::new();
    let store = Store::open(db.path()).await.unwrap();
    let mut writer = store
        .create("/work".into(), "mock".into(), true)
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
    connection.execute_batch("PRAGMA user_version=2").unwrap();
    drop(connection);
    assert!(Store::open(db.path()).await.is_err());
}

#[tokio::test]
async fn finalization_rolls_back_events_status_and_context_together() {
    let db = Database::new();
    let store = Store::open(db.path()).await.unwrap();
    let mut writer = store
        .create("/work".into(), "mock".into(), true)
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
