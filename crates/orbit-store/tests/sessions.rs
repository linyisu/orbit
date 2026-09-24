use orbit_model::types::{ChatMessage, ToolCall};
use orbit_store::Store;
use serde_json::json;

#[tokio::test]
async fn records_and_reopens_complete_session_history() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("data/sessions.db");
    let store = Store::open(&database).await.unwrap();
    let mut writer = store.create(root.path(), "model-a").await.unwrap();
    let id = writer.id().to_owned();

    writer
        .begin("find the bug".to_owned(), "model-a".to_owned())
        .await
        .unwrap();
    writer
        .append(ChatMessage::assistant("fixed"))
        .await
        .unwrap();
    writer.finish("completed").await.unwrap();
    drop(writer);
    drop(store);

    let reopened = Store::open(&database).await.unwrap();
    let sessions = reopened.list(root.path()).await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].id, id);
    assert_eq!(sessions[0].title, "find the bug");
    let snapshot = reopened.load(&id).await.unwrap();
    assert_eq!(snapshot.runs[0].status, "completed");
    assert_eq!(
        snapshot.projected_history(),
        vec![
            ChatMessage::user("find the bug"),
            ChatMessage::assistant("fixed")
        ]
    );

    let first = reopened.canonical_messages(&id, None, 1).await.unwrap();
    let second = reopened
        .canonical_messages(&id, Some(first[0].id), 1)
        .await
        .unwrap();
    assert_eq!(first[0].message, ChatMessage::user("find the bug"));
    assert_eq!(second[0].message, ChatMessage::assistant("fixed"));
}

#[tokio::test]
async fn renaming_a_session_survives_reopen_and_preserves_history() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("sessions.db");
    let store = Store::open(&database).await.unwrap();
    let mut writer = store.create(root.path(), "model-a").await.unwrap();
    let id = writer.id().to_owned();

    writer
        .begin("original".into(), "model-a".into())
        .await
        .unwrap();
    writer
        .append(ChatMessage::assistant("answer"))
        .await
        .unwrap();
    writer.finish("completed").await.unwrap();
    store.rename_session(&id, "  custom title  ").await.unwrap();
    drop(writer);
    drop(store);

    let reopened = Store::open(&database).await.unwrap();
    let snapshot = reopened.load(&id).await.unwrap();
    assert_eq!(snapshot.session.title, "custom title");
    assert_eq!(snapshot.messages.len(), 2);
    assert!(reopened.rename_session(&id, " ").await.is_err());
}

#[tokio::test]
async fn changing_session_model_preserves_history_and_survives_reopen() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("sessions.db");
    let store = Store::open(&database).await.unwrap();
    let mut writer = store.create(root.path(), "first::a").await.unwrap();
    let id = writer.id().to_owned();
    writer
        .begin("hello".into(), "first::a".into())
        .await
        .unwrap();
    writer.append(ChatMessage::assistant("hi")).await.unwrap();
    writer.finish("completed").await.unwrap();

    store.set_session_model(&id, "second::b").await.unwrap();
    drop(writer);
    drop(store);

    let reopened = Store::open(&database).await.unwrap();
    let snapshot = reopened.load(&id).await.unwrap();
    assert_eq!(snapshot.session.model, "second::b");
    assert_eq!(snapshot.projected_history().len(), 2);
    assert!(reopened.set_session_model(&id, " ").await.is_err());
}

#[tokio::test]
async fn prevents_parallel_writers_and_repairs_interrupted_tool_projection() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path().join("sessions.db")).await.unwrap();
    let mut writer = store.create(root.path(), "model-a").await.unwrap();
    let id = writer.id().to_owned();
    let call = ToolCall::function("call-1", "read_file", json!({ "path": "main.rs" }));

    writer
        .begin("inspect".to_owned(), "model-a".to_owned())
        .await
        .unwrap();
    writer
        .append(ChatMessage::assistant_tool_calls(vec![call.clone()]))
        .await
        .unwrap();
    let error = match store.acquire(&id).await {
        Ok(_) => panic!("a second writer must not acquire the session"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("already open"), "{error}");
    drop(writer);

    let mut resumed = store.acquire(&id).await.unwrap();
    let before = store.load(&id).await.unwrap();
    assert_eq!(before.runs[0].status, "interrupted");
    assert_eq!(before.messages.len(), 2);
    let projected = before.projected_history();
    assert_eq!(projected.len(), 3);
    assert!(matches!(
        &projected[2],
        ChatMessage::Tool { tool_call_id, content }
            if tool_call_id == "call-1" && content.contains("outcome is unknown")
    ));

    resumed
        .begin("continue".to_owned(), "model-b".to_owned())
        .await
        .unwrap();
    resumed
        .append(ChatMessage::assistant("done"))
        .await
        .unwrap();
    resumed.finish("completed").await.unwrap();

    let after = store.load(&id).await.unwrap();
    assert_eq!(after.runs.len(), 2);
    assert_eq!(after.messages.len(), 4);
    assert_eq!(after.session.model, "model-b");
    assert_eq!(after.session.title, "inspect");
    assert_eq!(
        after.messages[1].message,
        ChatMessage::assistant_tool_calls(vec![call])
    );
}

#[tokio::test]
async fn rejects_incompatible_schema_without_changing_it() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("legacy.db");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch("PRAGMA user_version = 15;")
        .unwrap();
    drop(connection);

    let error = match Store::open(&database).await {
        Ok(_) => panic!("legacy schema must be rejected"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("incompatible"), "{error}");

    let connection = rusqlite::Connection::open(&database).unwrap();
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 15);
}

#[tokio::test]
async fn rejects_unversioned_existing_schema_without_changing_journal_mode() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("unversioned.db");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch("CREATE TABLE legacy_records (value TEXT);")
        .unwrap();
    drop(connection);

    let error = match Store::open(&database).await {
        Ok(_) => panic!("unversioned existing schema must be rejected"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("unrecognized schema"), "{error}");

    let connection = rusqlite::Connection::open(&database).unwrap();
    let mode: String = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "delete");
}

#[tokio::test]
async fn command_output_pages_survive_resume_and_stay_in_their_session() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("sessions.db");
    let store = Store::open(&database).await.unwrap();
    let writer = store.create(root.path(), "model-a").await.unwrap();
    let session_id = writer.id().to_owned();
    let output_id = store
        .save_command_output(
            &session_id,
            b"alpha\nbeta\n".to_vec(),
            b"warning\n".to_vec(),
            25,
            8,
            None,
        )
        .await
        .unwrap();
    drop(writer);
    drop(store);

    let reopened = Store::open(&database).await.unwrap();
    let _resumed = reopened.acquire(&session_id).await.unwrap();
    let first = reopened
        .read_command_output(&session_id, &output_id, "stdout", 0, 6)
        .await
        .unwrap();
    assert_eq!(first.content, b"alpha\n");
    assert_eq!(first.retained_bytes, 11);
    assert_eq!(first.total_bytes, 25);
    let second = reopened
        .read_command_output(&session_id, &output_id, "stdout", 6, 8)
        .await
        .unwrap();
    assert_eq!(second.content, b"beta\n");

    let other = reopened.create(root.path(), "model-b").await.unwrap();
    let denied = reopened
        .read_command_output(other.id(), &output_id, "stderr", 0, 8)
        .await
        .unwrap_err();
    assert!(denied.to_string().contains("not found"), "{denied}");
    let invalid = reopened
        .read_command_output(&session_id, "../escape", "stdout", 0, 8)
        .await
        .unwrap_err();
    assert!(
        invalid.to_string().contains("invalid session ID"),
        "{invalid}"
    );
}

#[tokio::test]
async fn upgrades_existing_session_database_without_losing_messages() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("sessions.db");
    let store = Store::open(&database).await.unwrap();
    let mut writer = store.create(root.path(), "model-a").await.unwrap();
    let id = writer.id().to_owned();
    writer
        .begin("hello".to_owned(), "model-a".to_owned())
        .await
        .unwrap();
    writer.finish("completed").await.unwrap();
    drop(writer);
    drop(store);

    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "DROP TABLE session_memories; DROP TABLE project_tasks; DROP TABLE context_checkpoints; DROP INDEX messages_session_digest; DROP TABLE command_evidence; DROP TABLE command_outputs; PRAGMA user_version = 1;",
        )
        .unwrap();
    drop(connection);

    let upgraded = Store::open(&database).await.unwrap();
    let snapshot = upgraded.load(&id).await.unwrap();
    assert_eq!(
        snapshot.projected_history(),
        vec![ChatMessage::user("hello")]
    );
    let output_id = upgraded
        .save_command_output(&id, b"ok".to_vec(), Vec::new(), 2, 0, None)
        .await
        .unwrap();
    assert_eq!(
        upgraded
            .read_command_output(&id, &output_id, "stdout", 0, 8)
            .await
            .unwrap()
            .content,
        b"ok"
    );
}

#[tokio::test]
async fn upgrades_version_two_command_outputs_to_verification_evidence_schema() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("sessions.db");
    let store = Store::open(&database).await.unwrap();
    let writer = store.create(root.path(), "model-a").await.unwrap();
    let session_id = writer.id().to_owned();
    let output_id = store
        .save_command_output(&session_id, b"ok".to_vec(), Vec::new(), 2, 0, None)
        .await
        .unwrap();
    drop(writer);
    drop(store);

    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch("DROP TABLE session_memories; DROP TABLE project_tasks; DROP TABLE context_checkpoints; DROP INDEX messages_session_digest; DROP TABLE command_evidence; PRAGMA user_version = 2;")
        .unwrap();
    drop(connection);

    let upgraded = Store::open(&database).await.unwrap();
    assert_eq!(
        upgraded
            .read_command_output(&session_id, &output_id, "stdout", 0, 8)
            .await
            .unwrap()
            .content,
        b"ok"
    );
    assert!(
        upgraded
            .verification_records(&session_id, 20)
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn checkpoint_and_history_evidence_survive_resume_without_cross_session_access() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("sessions.db");
    let store = Store::open(&database).await.unwrap();
    let mut writer = store.create(root.path(), "model-a").await.unwrap();
    let session_id = writer.id().to_owned();
    writer
        .begin("review parser".to_owned(), "model-a".to_owned())
        .await
        .unwrap();
    writer
        .append(ChatMessage::assistant("修复 parser 边界条件"))
        .await
        .unwrap();
    writer.finish("completed").await.unwrap();
    let messages = store
        .canonical_messages(&session_id, None, 10)
        .await
        .unwrap();
    let digest = sha256_message(&messages[1].message);
    let checkpoint = r#"{"version":1,"through":2}"#;
    store
        .save_context_checkpoint(&session_id, 2, &"a".repeat(64), checkpoint)
        .await
        .unwrap();
    drop(writer);
    drop(store);

    let reopened = Store::open(&database).await.unwrap();
    assert_eq!(
        reopened
            .load_context_checkpoint(&session_id)
            .await
            .unwrap()
            .as_deref(),
        Some(checkpoint)
    );
    let found = reopened
        .search_history(&session_id, "parser", 10)
        .await
        .unwrap();
    assert_eq!(found.len(), 2);
    assert!(found.iter().any(|hit| hit.sha256 == digest));
    let page = reopened
        .read_history(&session_id, &digest, 0, 8192)
        .await
        .unwrap();
    assert!(page.content.contains("修复 parser"), "{}", page.content);
    assert_eq!(page.total_bytes, page.content.len());

    let other = reopened.create(root.path(), "model-b").await.unwrap();
    let denied = reopened
        .read_history(other.id(), &digest, 0, 100)
        .await
        .unwrap_err();
    assert!(denied.to_string().contains("not found"), "{denied}");
    assert!(
        reopened
            .search_history(other.id(), "parser", 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        reopened
            .load_context_checkpoint(other.id())
            .await
            .unwrap()
            .is_none()
    );
}

fn sha256_message(message: &ChatMessage) -> String {
    use sha2::{Digest, Sha256};
    let payload = serde_json::to_string(message).unwrap();
    Sha256::digest(payload.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[tokio::test]
async fn upgrades_version_three_session_with_checkpoint_schema() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("sessions.db");
    let store = Store::open(&database).await.unwrap();
    let writer = store.create(root.path(), "model-a").await.unwrap();
    let id = writer.id().to_owned();
    drop(writer);
    drop(store);

    let connection = rusqlite::Connection::open(&database).unwrap();
    connection.execute_batch("DROP TABLE session_memories; DROP TABLE project_tasks; DROP TABLE context_checkpoints; DROP INDEX messages_session_digest; PRAGMA user_version = 3;").unwrap();
    drop(connection);

    let upgraded = Store::open(&database).await.unwrap();
    assert_eq!(upgraded.session(&id).await.unwrap().id, id);
    assert!(
        upgraded
            .load_context_checkpoint(&id)
            .await
            .unwrap()
            .is_none()
    );
    upgraded
        .save_context_checkpoint(&id, 1, &"b".repeat(64), "{}")
        .await
        .unwrap();
    assert_eq!(
        upgraded
            .load_context_checkpoint(&id)
            .await
            .unwrap()
            .as_deref(),
        Some("{}")
    );
}
