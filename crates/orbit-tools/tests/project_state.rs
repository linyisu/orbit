use orbit_agent::tool::ToolRegistry;
use orbit_model::types::ToolCall;
use orbit_store::Store;
use orbit_tools::project_state;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

async fn call(tools: &ToolRegistry, name: &str, arguments: Value) -> Value {
    let output = tools
        .execute(
            ToolCall::function("state-call", name, arguments),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    serde_json::from_str(&output).unwrap()
}

#[tokio::test]
async fn registered_tools_share_tasks_and_isolate_conversation_memories() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(root.path().join("sessions.db")).await.unwrap();
    let first = store.create(root.path(), "model").await.unwrap();
    let second = store.create(root.path(), "model").await.unwrap();
    let workspace = root.path().to_string_lossy().into_owned();
    let mut first_tools = ToolRegistry::new();
    project_state::register(
        &mut first_tools,
        store.clone(),
        workspace.clone(),
        first.id().to_owned(),
    );
    let mut second_tools = ToolRegistry::new();
    project_state::register(&mut second_tools, store, workspace, second.id().to_owned());

    let task = call(
        &first_tools,
        "task_update",
        json!({ "action": "create", "title": "Verify parser" }),
    )
    .await;
    let task_id = task["task"]["id"].as_i64().unwrap();
    let listed = call(&second_tools, "task_list", json!({})).await;
    assert_eq!(listed["tasks"][0]["id"], task_id);
    call(
        &second_tools,
        "task_update",
        json!({ "action": "update", "id": task_id, "status": "completed" }),
    )
    .await;
    assert_eq!(
        call(&first_tools, "task_list", json!({})).await["tasks"],
        json!([])
    );
    for title in ["Inspect lexer", "Implement parser", "Run tests"] {
        call(
            &first_tools,
            "task_update",
            json!({ "action": "create", "title": title }),
        )
        .await;
    }
    let first_page = call(&first_tools, "task_list", json!({ "limit": 2 })).await;
    assert_eq!(first_page["tasks"].as_array().unwrap().len(), 2);
    assert_eq!(first_page["next_offset"], 2);
    let second_page = call(
        &first_tools,
        "task_list",
        json!({ "offset": 2, "limit": 2 }),
    )
    .await;
    assert_eq!(second_page["tasks"].as_array().unwrap().len(), 1);
    assert_eq!(second_page["has_more"], false);

    let saved = call(
        &first_tools,
        "memory_save",
        json!({
            "category": "decision",
            "title": "Parser strategy",
            "content": "Use Pratt parsing"
        }),
    )
    .await;
    let memory_id = saved["memory"]["id"].as_i64().unwrap();
    assert_eq!(
        call(&first_tools, "memory_search", json!({ "query": "Pratt" })).await["memories"][0]["id"],
        memory_id
    );
    assert_eq!(
        call(&second_tools, "memory_list", json!({})).await["memories"],
        json!([])
    );
    assert!(
        second_tools
            .execute(
                ToolCall::function("wrong-session", "memory_forget", json!({ "id": memory_id })),
                CancellationToken::new(),
            )
            .await
            .is_err()
    );
    call(&first_tools, "memory_forget", json!({ "id": memory_id })).await;
    assert_eq!(
        call(&first_tools, "memory_list", json!({})).await["memories"],
        json!([])
    );
    assert_eq!(
        call(
            &first_tools,
            "memory_list",
            json!({ "include_inactive": true })
        )
        .await["memories"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
