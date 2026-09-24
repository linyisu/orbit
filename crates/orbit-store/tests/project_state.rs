use orbit_store::{
    Store,
    state::{MemoryCategory, TaskChanges, TaskStatus},
};

#[tokio::test]
async fn tasks_share_a_workspace_while_memories_remain_session_local_after_reopen() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("project");
    std::fs::create_dir(&workspace).unwrap();
    let other_workspace = root.path().join("other");
    std::fs::create_dir(&other_workspace).unwrap();
    let database = root.path().join("state.db");
    let store = Store::open(&database).await.unwrap();
    let first = store.create(&workspace, "model").await.unwrap();
    let second = store.create(&workspace, "model").await.unwrap();
    let other = store.create(&other_workspace, "model").await.unwrap();
    let path = workspace.to_string_lossy();

    let parent = store
        .create_task(&path, None, "Ship parser".into(), String::new(), 0)
        .await
        .unwrap();
    let child = store
        .create_task(&path, Some(parent.id), "Add tests".into(), String::new(), 5)
        .await
        .unwrap();
    store
        .update_task(
            &path,
            child.id,
            TaskChanges {
                status: Some(TaskStatus::Completed),
                ..TaskChanges::default()
            },
        )
        .await
        .unwrap();
    store
        .save_memory(
            first.id(),
            MemoryCategory::Decision,
            "Parser".into(),
            "Use Pratt parsing".into(),
        )
        .await
        .unwrap();

    assert_eq!(
        store.list_tasks(&path, false, 0, 100).await.unwrap().len(),
        1
    );
    assert_eq!(
        store.list_tasks(&path, true, 0, 100).await.unwrap().len(),
        2
    );
    assert!(
        store
            .list_tasks(&other_workspace.to_string_lossy(), true, 0, 100)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .create_task(
                &other_workspace.to_string_lossy(),
                Some(parent.id),
                "Wrong".into(),
                String::new(),
                0
            )
            .await
            .is_err()
    );
    assert!(
        store
            .update_task(
                &other_workspace.to_string_lossy(),
                parent.id,
                TaskChanges {
                    title: Some("Wrong".into()),
                    ..TaskChanges::default()
                }
            )
            .await
            .is_err()
    );
    assert!(
        store
            .list_memories(second.id(), false, 0, 100)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .list_memories(other.id(), false, 0, 100)
            .await
            .unwrap()
            .is_empty()
    );

    let first_id = first.id().to_owned();
    let second_id = second.id().to_owned();
    drop(first);
    drop(second);
    drop(other);
    drop(store);

    let reopened = Store::open(&database).await.unwrap();
    assert_eq!(
        reopened
            .list_tasks(&path, true, 0, 100)
            .await
            .unwrap()
            .len(),
        2
    );
    let found = reopened
        .search_memories(&first_id, "Pratt", 8)
        .await
        .unwrap();
    assert_eq!(found.len(), 1);
    assert!(
        reopened
            .search_memories(&second_id, "Pratt", 8)
            .await
            .unwrap()
            .is_empty()
    );
    let forgotten = reopened
        .forget_memory(&first_id, found[0].id)
        .await
        .unwrap();
    assert!(!forgotten.active);
    assert!(
        reopened
            .list_memories(&first_id, false, 0, 100)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        reopened
            .list_memories(&first_id, true, 0, 100)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        reopened
            .forget_memory(&second_id, found[0].id)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn version_four_database_upgrades_without_losing_sessions() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("sessions.db");
    let store = Store::open(&database).await.unwrap();
    let writer = store.create(root.path(), "model").await.unwrap();
    let session_id = writer.id().to_owned();
    drop(writer);
    drop(store);

    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch(
            "DROP TABLE model_token_samples;
             DROP TABLE session_memories;
             DROP TABLE project_tasks;
             ALTER TABLE sessions DROP COLUMN reasoning_effort;
             PRAGMA user_version = 4;",
        )
        .unwrap();
    drop(connection);

    let upgraded = Store::open(&database).await.unwrap();
    assert_eq!(upgraded.session(&session_id).await.unwrap().id, session_id);
    let memory = upgraded
        .save_memory(
            &session_id,
            MemoryCategory::Fact,
            "Fact".into(),
            "Verified".into(),
        )
        .await
        .unwrap();
    assert_eq!(memory.content, "Verified");
}
