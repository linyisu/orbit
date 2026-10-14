use super::*;
use orbit_agent::{checkpoint::ExecutionCheckpoint, events::AgentEvent};
use orbit_model::types::ChatMessage;

#[tokio::test]
async fn queue_uses_current_project_path_and_rejects_legacy_workspace() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path().join("sessions.db"))
        .await
        .unwrap();
    let workspace = root
        .path()
        .to_string_lossy()
        .into_owned();
    let writer = store
        .create(workspace.clone(), "test".into())
        .await
        .unwrap();
    let session = writer.id().to_owned();
    let id = store
        .queue_session_input(&session, "original".into())
        .await
        .unwrap();
    let stale_session = session.clone();
    store
        .write_access(move |conn| async move {
            sessions::Entity::update_many()
                .col_expr(
                    sessions::Column::Workspace,
                    Expr::value("/legacy-project-path"),
                )
                .filter(sessions::Column::Id.eq(stale_session))
                .exec(&conn)
                .await?;
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .session(&session)
            .await
            .unwrap()
            .workspace,
        workspace
    );
    assert_eq!(
        store
            .input_queue(&session)
            .await
            .unwrap()
            .workspace,
        workspace
    );
    assert!(
        writer
            .edit_queued_input("/legacy-project-path", id, "original", "wrong".into())
            .await
            .is_err()
    );
    writer
        .edit_queued_input(&workspace, id, "original", "edited".into())
        .await
        .unwrap();
    assert!(
        writer
            .remove_queued_input("/legacy-project-path", id, "edited")
            .await
            .is_err()
    );
    writer
        .remove_queued_input(&workspace, id, "edited")
        .await
        .unwrap();
    assert!(
        store
            .input_queue(&session)
            .await
            .unwrap()
            .messages
            .is_empty()
    );
}

#[tokio::test]
async fn exact_content_cas_preserves_ids_order_and_durable_removal() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("sessions.db");
    let workspace = root
        .path()
        .to_string_lossy()
        .into_owned();
    let store = Store::open(&path).await.unwrap();
    let writer = store
        .create(workspace.clone(), "test".into())
        .await
        .unwrap();
    let session = writer.id().to_owned();
    let first = store
        .queue_session_input(&session, "  原文\n第二行  ".into())
        .await
        .unwrap();
    let second = store
        .queue_session_input(&session, "second".into())
        .await
        .unwrap();
    assert!(
        writer
            .edit_queued_input(&workspace, first, "原文\n第二行", "wrong".into())
            .await
            .is_err()
    );
    writer
        .edit_queued_input(&workspace, first, "  原文\n第二行  ", "修改内容".into())
        .await
        .unwrap();
    let entries = store
        .input_queue(&session)
        .await
        .unwrap()
        .messages;
    assert_eq!(
        entries
            .iter()
            .map(|message| message.id)
            .collect::<Vec<_>>(),
        [first, second]
    );
    assert_eq!(entries[0].content, "修改内容");
    assert!(
        writer
            .remove_queued_input(&workspace, first, "  原文\n第二行  ")
            .await
            .is_err()
    );
    writer
        .remove_queued_input(&workspace, second, "second")
        .await
        .unwrap();
    drop(writer);
    drop(store);
    let reopened = Store::open(&path).await.unwrap();
    let entries = reopened
        .input_queue(&session)
        .await
        .unwrap()
        .messages;
    assert_eq!(entries.len(), 1);
    assert_eq!(
        (entries[0].id, entries[0].content.as_str()),
        (first, "修改内容")
    );
}

#[tokio::test]
async fn concurrent_edit_cas_has_one_winner_and_rejects_foreign_session_or_workspace() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path().join("sessions.db"))
        .await
        .unwrap();
    let workspace = root
        .path()
        .to_string_lossy()
        .into_owned();
    let writer = store
        .create(workspace.clone(), "test".into())
        .await
        .unwrap();
    let other = store
        .create(workspace.clone(), "test".into())
        .await
        .unwrap();
    let id = store
        .queue_session_input(writer.id(), "original".into())
        .await
        .unwrap();
    assert!(
        other
            .remove_queued_input(&workspace, id, "original")
            .await
            .is_err()
    );
    assert!(
        writer
            .edit_queued_input("/stale-workspace", id, "original", "foreign".into())
            .await
            .is_err()
    );
    let (first, second) = tokio::join!(
        writer.edit_queued_input(&workspace, id, "original", "first".into()),
        writer.edit_queued_input(&workspace, id, "original", "second".into()),
    );
    assert_ne!(first.is_ok(), second.is_ok());
    let pending = store
        .input_queue(writer.id())
        .await
        .unwrap()
        .messages;
    assert_eq!(pending[0].id, id);
    assert!(matches!(pending[0].content.as_str(), "first" | "second"));
}

#[tokio::test]
async fn checkpoint_consumed_input_cannot_be_edited_or_removed() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path().join("sessions.db"))
        .await
        .unwrap();
    let workspace = root
        .path()
        .to_string_lossy()
        .into_owned();
    let mut writer = store
        .create(workspace.clone(), "test".into())
        .await
        .unwrap();
    writer
        .begin("initial".into(), "test".into())
        .await
        .unwrap();
    let id = store
        .queue_session_input(writer.id(), "consumed".into())
        .await
        .unwrap();
    let journal = writer.execution_journal().unwrap();
    let message = ChatMessage::user("consumed");
    journal.record_message(message.clone());
    journal
        .save(
            ExecutionCheckpoint {
                history: vec![message],
                turns: 0,
                tool_calls: 0,
                in_flight: vec![],
                attachments: vec![],
            },
            Some(AgentEvent::InputReceived {
                attachments: vec![],
                message_id: id,
                content: "consumed".into(),
            }),
        )
        .await
        .unwrap();
    assert!(
        store
            .input_queue(writer.id())
            .await
            .unwrap()
            .messages
            .is_empty()
    );
    assert!(
        writer
            .edit_queued_input(&workspace, id, "consumed", "late".into())
            .await
            .is_err()
    );
    assert!(
        writer
            .remove_queued_input(&workspace, id, "consumed")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn writer_lease_and_external_move_protect_idle_persistent_input() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("sessions.db");
    let store = Store::open(&path).await.unwrap();
    let external = Store::open(&path).await.unwrap();
    let workspace = root
        .path()
        .to_string_lossy()
        .into_owned();
    let writer = store
        .create(workspace.clone(), "test".into())
        .await
        .unwrap();
    let session = writer.id().to_owned();
    let id = store
        .queue_session_input(&session, "pending".into())
        .await
        .unwrap();
    assert!(
        external
            .acquire(&session)
            .await
            .is_err()
    );
    drop(writer);
    let writer = external
        .acquire(&session)
        .await
        .unwrap();
    let moved = root.path().join("moved");
    std::fs::create_dir(&moved).unwrap();
    writer
        .move_to(&moved, None)
        .await
        .unwrap();
    assert!(
        writer
            .remove_queued_input(&workspace, id, "pending")
            .await
            .is_err()
    );
    assert_eq!(
        store
            .input_queue(&session)
            .await
            .unwrap()
            .messages[0]
            .content,
        "pending"
    );
    writer
        .edit_queued_input(moved.to_str().unwrap(), id, "pending", "after move".into())
        .await
        .unwrap();
    assert_eq!(
        store
            .input_queue(&session)
            .await
            .unwrap()
            .messages[0]
            .content,
        "after move"
    );
}

#[tokio::test]
async fn image_only_queue_survives_reload_and_atomic_withdrawal() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path().join("sessions.db"))
        .await
        .unwrap();
    let workspace = root
        .path()
        .to_string_lossy()
        .into_owned();
    let writer = store
        .create(workspace.clone(), "test".into())
        .await
        .unwrap();
    let image = orbit_model::types::Attachment::Image {
        name: "queued.png".into(),
        mime_type: "image/png".into(),
        path: root.path().join("queued.png"),
        lease: None,
    };
    let id = store
        .queue_session_input_with_attachments(writer.id(), String::new(), vec![image.clone()])
        .await
        .unwrap();
    let pending = store
        .pending_agent_messages(writer.id())
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].attachments[0].path(), image.path());
    assert!(pending[0].content.is_empty());
    let queue = store
        .input_queue(writer.id())
        .await
        .unwrap();
    assert_eq!(queue.messages[0].attachments[0].name(), "queued.png");
    writer
        .remove_queued_input(&workspace, id, "")
        .await
        .unwrap();
    assert!(
        store
            .pending_agent_messages(writer.id())
            .await
            .unwrap()
            .is_empty()
    );
}
