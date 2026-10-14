use super::*;

#[tokio::test]
async fn stale_native_run_guard_never_cancels_a_later_follow_up() {
    let directory = tempfile::tempdir().unwrap();
    let model = GatedModel::new();
    let (service, _parent) = fixture(directory.path(), model.clone(), Default::default()).await;
    let first = spawn(&service, "native-first-run", CancellationToken::new()).await;
    let id = first["agent_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let first_run = first["run_id"].as_i64().unwrap();
    model.received(1).await;
    model.gate.add_permits(1);
    service
        .jobs
        .wait(&[], true, CancellationToken::new())
        .await
        .unwrap();
    let args = json!({
        "agent_id": id,
        "message": "继续核对当前任务"
    });
    let follow_up: Value = serde_json::from_str(
        &service
            .clone()
            .execute(
                Action::Send,
                ToolCall::function("native-follow-up", "send_agent_message", args.clone()),
                args,
                CancellationToken::new(),
            )
            .await
            .unwrap()
            .content,
    )
    .unwrap();
    model.received(2).await;
    let current_run = follow_up["run_id"].as_i64().unwrap();
    assert_ne!(first_run, current_run);
    let cancel = |run| {
        json!({
            "agent_ids": [id],
            "expected_run_id": run
        })
    };
    let error = service
        .clone()
        .execute(
            Action::Cancel,
            ToolCall::function("native-stale-cancel", "cancel_agents", cancel(first_run)),
            cancel(first_run),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("另一轮任务"));
    assert_eq!(
        service
            .store
            .session_revision(&id)
            .await
            .unwrap()
            .unwrap()
            .run,
        current_run
    );
    assert_eq!(
        service
            .store
            .session_revision(&id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "running"
    );
    let job = follow_up["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        service
            .jobs
            .snapshots(&[job], false)
            .unwrap()[0]
            .status,
        JobStatus::Running
    );
    service
        .clone()
        .execute(
            Action::Cancel,
            ToolCall::function(
                "native-current-cancel",
                "cancel_agents",
                cancel(current_run),
            ),
            cancel(current_run),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    service
        .jobs
        .wait(&[], true, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        service
            .store
            .session_revision(&id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "cancelled"
    );
}

#[tokio::test]
async fn run_guard_requires_one_owned_child_and_never_means_cancel_all() {
    let directory = tempfile::tempdir().unwrap();
    let (service, _parent) = fixture(directory.path(), GatedModel::new(), Default::default()).await;
    let first = spawn(&service, "native-a", CancellationToken::new()).await;
    let second = spawn(&service, "native-b", CancellationToken::new()).await;
    let id = first["agent_id"].as_str().unwrap();
    let other_id = second["agent_id"].as_str().unwrap();
    let run = first["run_id"].as_i64().unwrap();
    for args in [
        json!({
            "agent_ids": [],
            "expected_run_id": run
        }),
        json!({
            "agent_ids": [id, other_id],
            "expected_run_id": run
        }),
        json!({
            "agent_ids": [id],
            "expected_run_id": 0
        }),
    ] {
        assert!(
            service
                .clone()
                .execute(
                    Action::Cancel,
                    ToolCall::function("native-invalid-guard", "cancel_agents", args.clone()),
                    args,
                    CancellationToken::new()
                )
                .await
                .is_err()
        );
    }
    assert_eq!(
        service
            .store
            .session_revision(id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "running"
    );
    assert_eq!(
        service
            .store
            .session_revision(other_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        "running"
    );
    // Existing model-tool requests without a guard retain their original
    // selected cancellation semantics, including sibling isolation.
    let args = json!({
        "agent_ids": [id]
    });
    service
        .clone()
        .execute(
            Action::Cancel,
            ToolCall::function("legacy-selected-cancel", "cancel_agents", args.clone()),
            args,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let second_job = second["job_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        service
            .jobs
            .snapshots(&[second_job], false)
            .unwrap()[0]
            .status,
        JobStatus::Running
    );
    service.jobs.cancel(&[]).unwrap();
    service
        .jobs
        .wait(&[], true, CancellationToken::new())
        .await
        .unwrap();
}
