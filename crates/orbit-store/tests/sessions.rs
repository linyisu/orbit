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
async fn model_token_samples_are_bounded_session_scoped_and_durable() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("sessions.db");
    let store = Store::open(&database).await.unwrap();
    let first = store
        .create(root.path(), "provider-a::model")
        .await
        .unwrap();
    let second = store
        .create(root.path(), "provider-a::model")
        .await
        .unwrap();
    let first_id = first.id().to_owned();

    for index in 1..=12 {
        store
            .record_model_token_sample(first.id(), "provider-a::model", 100, index * 100)
            .await
            .unwrap();
    }
    store
        .record_model_token_sample(first.id(), "provider-b::model", 200, 300)
        .await
        .unwrap();
    assert!(
        store
            .model_token_samples(second.id(), "provider-a::model")
            .await
            .unwrap()
            .is_empty()
    );
    drop(first);
    drop(second);
    drop(store);

    let reopened = Store::open(&database).await.unwrap();
    let samples = reopened
        .model_token_samples(&first_id, "provider-a::model")
        .await
        .unwrap();
    assert_eq!(samples.len(), 9);
    assert_eq!(samples.first(), Some(&(100, 400)));
    assert_eq!(samples.last(), Some(&(100, 1200)));
    assert_eq!(
        reopened
            .model_token_samples(&first_id, "provider-b::model")
            .await
            .unwrap(),
        vec![(200, 300)]
    );
}

#[tokio::test]
async fn upgrades_version_six_with_existing_messages_and_adds_usage_samples() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("sessions.db");
    let store = Store::open(&database).await.unwrap();
    let mut writer = store.create(root.path(), "model-a").await.unwrap();
    let id = writer.id().to_owned();
    writer
        .begin("hello".into(), "model-a".into())
        .await
        .unwrap();
    writer.finish("completed").await.unwrap();
    drop(writer);
    drop(store);

    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch("DROP TABLE model_token_samples; PRAGMA user_version = 6;")
        .unwrap();
    drop(connection);

    let upgraded = Store::open(&database).await.unwrap();
    upgraded
        .record_model_token_sample(&id, "model-a", 100, 120)
        .await
        .unwrap();

    assert_eq!(upgraded.load(&id).await.unwrap().messages.len(), 1);
    assert_eq!(
        upgraded.model_token_samples(&id, "model-a").await.unwrap(),
        vec![(100, 120)]
    );
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
async fn version_five_sessions_upgrade_with_reasoning_profile() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("sessions.db");
    let store = Store::open(&database).await.unwrap();
    let writer = store.create(root.path(), "provider::model").await.unwrap();
    let id = writer.id().to_owned();
    drop(writer);
    drop(store);

    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "DROP TABLE model_token_samples;
             ALTER TABLE sessions DROP COLUMN reasoning_effort;
             PRAGMA user_version = 5;",
        )
        .unwrap();
    drop(connection);

    let upgraded = Store::open(&database).await.unwrap();
    assert_eq!(
        upgraded.session(&id).await.unwrap().model,
        "provider::model"
    );
    assert_eq!(upgraded.session(&id).await.unwrap().reasoning_effort, None);
    upgraded
        .set_session_profile(&id, "provider::model", Some("high"))
        .await
        .unwrap();
    drop(upgraded);

    let reopened = Store::open(&database).await.unwrap();
    assert_eq!(
        reopened
            .session(&id)
            .await
            .unwrap()
            .reasoning_effort
            .as_deref(),
        Some("high")
    );
}

#[tokio::test]
async fn image_attachment_survives_resume_and_detects_blob_tampering() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("sessions.db");
    let source = root.path().join("scene.png");
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
        1,
        1,
        image::Rgba([1, 2, 3, 255]),
    ))
    .write_to(&mut png, image::ImageFormat::Png)
    .unwrap();
    std::fs::write(&source, png.into_inner()).unwrap();
    let store = Store::open(&database).await.unwrap();
    let mut writer = store.create(root.path(), "vision").await.unwrap();
    let id = writer.id().to_owned();

    let invalid = root.path().join("invalid.png");
    std::fs::write(&invalid, b"\x89PNG\r\n\x1a\ninvalid").unwrap();
    assert!(store.save_image(&id, &invalid).await.is_err());

    let attachment = store.save_image(&id, &source).await.unwrap();
    writer
        .begin_user(
            ChatMessage::user_with_attachments("Describe this", vec![attachment.clone()]),
            "vision".into(),
        )
        .await
        .unwrap();
    writer.finish("completed").await.unwrap();
    std::fs::remove_file(&source).unwrap();
    drop(writer);
    drop(store);

    let reopened = Store::open(&database).await.unwrap();
    let snapshot = reopened.load(&id).await.unwrap();
    let ChatMessage::User { attachments, .. } = &snapshot.projected_history()[0] else {
        panic!("image user message expected");
    };
    assert_eq!(attachments, std::slice::from_ref(&attachment));
    assert!(attachment.image().unwrap().1.starts_with("iVBOR"));

    let orbit_model::types::Attachment::Image { path, .. } = &attachment;
    std::fs::write(path, b"tampered").unwrap();
    assert!(attachment.image().is_err());
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
            "DROP TABLE model_token_samples;
             DROP TABLE session_memories;
             DROP TABLE project_tasks;
             DROP TABLE context_checkpoints;
             DROP INDEX messages_session_digest;
             DROP TABLE command_evidence;
             DROP TABLE command_outputs;
             ALTER TABLE sessions DROP COLUMN reasoning_effort;
             PRAGMA user_version = 1;",
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
        .execute_batch(
            "DROP TABLE model_token_samples;
                        DROP TABLE session_memories;
                        DROP TABLE project_tasks;
                        DROP TABLE context_checkpoints;
                        DROP INDEX messages_session_digest;
                        DROP TABLE command_evidence;
                        ALTER TABLE sessions DROP COLUMN reasoning_effort;
                        PRAGMA user_version = 2;",
        )
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
    connection
        .execute_batch(
            "DROP TABLE model_token_samples;
                              DROP TABLE session_memories;
                              DROP TABLE project_tasks;
                              DROP TABLE context_checkpoints;
                              DROP INDEX messages_session_digest;
                              ALTER TABLE sessions DROP COLUMN reasoning_effort;
                              PRAGMA user_version = 3;",
        )
        .unwrap();
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
