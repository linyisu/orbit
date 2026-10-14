use super::*;
use orbit_agent::{
    agent::{Agent, AgentConfig},
    checkpoint::{CheckpointFuture, ExecutionCheckpoint, ExecutionJournal},
    events::AgentEvent,
    inbox::InboxMessage,
    model::MockModel,
    tool::ToolRegistry,
};
use orbit_config::test_support::{TestConfig, TestModel, TestProvider};
use orbit_model::types::{ChatCompletion, ChatMessage, ChatRequest};
use std::sync::Arc;

async fn setup(root: &std::path::Path) -> PreparedWorkspace {
    let config = root.join("config.toml");
    TestConfig::new()
        .provider(
            TestProvider::new("test")
                .endpoint("http://127.0.0.1:1/v1")
                .key("test-only")
                .models([TestModel::new("test")]),
        )
        .write(&config)
        .unwrap();
    let options = crate::WorkspaceOptions {
        model: Some("test::test".into()),
        initial_directory: root.into(),
        auth_directory: root.join("auth"),
        config,
        database: root.join("sessions.db"),
        allow_mcp_start: false,
        home: Some(root.into()),
        config_home: None,
        instruction_dir: None,
        editor_command: "vi".into(),
    };
    let mut setup = crate::workspace::setup::prepare_workspace(&options, root, None)
        .await
        .unwrap();
    setup.start_session().await.unwrap();
    setup.session.enable_shared();
    setup
}

fn model() -> MockModel {
    MockModel::new([ChatCompletion {
        id: None,
        model: None,
        message: ChatMessage::assistant("done"),
        finish_reason: Some("stop".into()),
        usage: None,
    }])
}

#[tokio::test]
async fn running_edit_remove_use_real_inbox_and_canonical_consumption_under_sending_policy() {
    let root = tempfile::tempdir().unwrap();
    let mut setup = setup(root.path()).await;
    let workspace = setup
        .session
        .snapshot
        .session
        .workspace
        .clone();
    setup.resources.policy = orbit_protocol::SessionPolicy {
        read_only: true,
        disabled: vec![
            orbit_protocol::Capability::Files,
            orbit_protocol::Capability::Write,
        ],
    };
    setup
        .session
        .begin("initial".into(), "test::test".into())
        .await
        .unwrap();
    let run = setup.session.active_run_id();
    let inbox = AgentInbox::default();
    let mut ids = Vec::new();
    for content in ["first", "original", "last"] {
        let id = setup
            .session
            .store
            .queue_session_input(setup.session.id(), content.into())
            .await
            .unwrap();
        inbox
            .push(InboxMessage {
                attachments: vec![],
                id,
                content: content.into(),
            })
            .await
            .unwrap();
        ids.push(id);
    }
    let Snapshot::Edited { entry } = execute(
        &mut setup,
        Some(inbox.clone()),
        &workspace,
        Action::Edit {
            id: ids[1],
            expected: "original".into(),
            content: "  edited\ncontext  ".into(),
            run,
        },
    )
    .await
    .unwrap() else {
        panic!("edit receipt expected")
    };
    assert_eq!(
        (entry.id, entry.content.as_str()),
        (ids[1], "edited\ncontext")
    );
    assert!(
        execute(
            &mut setup,
            Some(inbox.clone()),
            &workspace,
            Action::Edit {
                id: ids[1],
                expected: "original".into(),
                content: "stale".into(),
                run
            }
        )
        .await
        .is_err()
    );
    execute(
        &mut setup,
        Some(inbox.clone()),
        &workspace,
        Action::Remove {
            id: ids[0],
            expected: "first".into(),
            run,
        },
    )
    .await
    .unwrap();
    assert!(
        execute(
            &mut setup,
            Some(inbox.clone()),
            &workspace,
            Action::Remove {
                id: ids[2],
                expected: "last".into(),
                run: None
            }
        )
        .await
        .is_err()
    );
    let journal = setup
        .session
        .execution_journal()
        .unwrap();
    let model = model();
    let result = Agent::new(model.clone(), ToolRegistry::new(), AgentConfig::default())
        .with_inbox(inbox)
        .with_execution_journal(journal)
        .run(
            ChatRequest::new(vec![ChatMessage::user("initial")]),
            Default::default(),
        )
        .await
        .unwrap();
    let users = model.requests()[0]
        .messages
        .iter()
        .filter_map(|message| match message {
            ChatMessage::User { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(users, ["initial", "edited\ncontext", "last"]);
    assert!(
        setup
            .session
            .store
            .input_queue(setup.session.id())
            .await
            .unwrap()
            .messages
            .is_empty()
    );
    let (_, canonical) = setup
        .session
        .store
        .canonical_messages(setup.session.id(), None, 100)
        .await
        .unwrap();
    assert!(canonical.iter().any(|record| matches!(
        &record.message,
        ChatMessage::User { content, .. } if content == "edited\ncontext"
    )));
    assert!(!canonical.iter().any(|record| matches!(
        &record.message,
        ChatMessage::User { content, .. } if content == "first" || content == "original"
    )));
    setup
        .session
        .finish("completed", Some(result.history))
        .await
        .unwrap();
}

struct GatedJournal {
    inner: Arc<dyn ExecutionJournal>,
    claimed: tokio::sync::Notify,
    release: tokio::sync::Semaphore,
}

impl ExecutionJournal for GatedJournal {
    fn record(&self, event: AgentEvent) {
        self.inner.record(event);
    }

    fn record_message(&self, message: ChatMessage) {
        self.inner.record_message(message);
    }

    fn flush(&self) -> CheckpointFuture<'_> {
        self.inner.flush()
    }

    fn save<'a>(
        &'a self,
        checkpoint: ExecutionCheckpoint,
        event: Option<AgentEvent>,
    ) -> CheckpointFuture<'a> {
        Box::pin(async move {
            if matches!(event, Some(AgentEvent::InputReceived { .. })) {
                self.claimed.notify_one();
                self.release
                    .acquire()
                    .await
                    .unwrap()
                    .forget();
            }
            self.inner.save(checkpoint, event).await
        })
    }
}

#[tokio::test]
async fn drained_but_not_checkpointed_is_claimed_and_cannot_be_rewritten_in_database() {
    let root = tempfile::tempdir().unwrap();
    let mut setup = setup(root.path()).await;
    let workspace = setup
        .session
        .snapshot
        .session
        .workspace
        .clone();
    setup
        .session
        .begin("initial".into(), "test::test".into())
        .await
        .unwrap();
    let run = setup.session.active_run_id();
    let id = setup
        .session
        .store
        .queue_session_input(setup.session.id(), "claimed".into())
        .await
        .unwrap();
    let inbox = AgentInbox::default();
    inbox
        .push(InboxMessage {
            attachments: vec![],
            id,
            content: "claimed".into(),
        })
        .await
        .unwrap();
    let journal = Arc::new(GatedJournal {
        inner: setup
            .session
            .execution_journal()
            .unwrap(),
        claimed: tokio::sync::Notify::new(),
        release: tokio::sync::Semaphore::new(0),
    });
    let agent = Agent::new(model(), ToolRegistry::new(), AgentConfig::default())
        .with_inbox(inbox.clone())
        .with_execution_journal(journal.clone());
    let task = tokio::spawn(async move {
        agent
            .run(
                ChatRequest::new(vec![ChatMessage::user("initial")]),
                Default::default(),
            )
            .await
    });
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        journal.claimed.notified(),
    )
    .await
    .unwrap();
    let Snapshot::List { entries, .. } = execute(
        &mut setup,
        Some(inbox.clone()),
        &workspace,
        Action::List { after: None },
    )
    .await
    .unwrap() else {
        panic!("list expected")
    };
    assert_eq!(entries[0].state, State::Claimed);
    assert!(!entries[0].editable());
    for action in [
        Action::Edit {
            id,
            expected: "claimed".into(),
            content: "late".into(),
            run,
        },
        Action::Remove {
            id,
            expected: "claimed".into(),
            run,
        },
    ] {
        let error = execute(&mut setup, Some(inbox.clone()), &workspace, action)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("领取"));
    }
    assert_eq!(
        setup
            .session
            .store
            .input_queue(setup.session.id())
            .await
            .unwrap()
            .messages[0]
            .content,
        "claimed"
    );
    journal.release.add_permits(1);
    let result = task.await.unwrap().unwrap();
    assert!(
        setup
            .session
            .store
            .input_queue(setup.session.id())
            .await
            .unwrap()
            .messages
            .is_empty()
    );
    setup
        .session
        .finish("completed", Some(result.history))
        .await
        .unwrap();
}

#[tokio::test]
async fn foreign_live_writer_blocks_idle_mutation_and_external_workspace_change_is_checked() {
    let root = tempfile::tempdir().unwrap();
    let mut setup = setup(root.path()).await;
    let workspace = setup
        .session
        .snapshot
        .session
        .workspace
        .clone();
    let session = setup.session.id().to_owned();
    let store = setup.session.store.clone();
    let id = store
        .queue_session_input(&session, "pending".into())
        .await
        .unwrap();
    let mut foreign = store.acquire(&session).await.unwrap();
    foreign
        .begin("foreign run".into(), "test::test".into())
        .await
        .unwrap();
    let Snapshot::List { entries, .. } =
        execute(&mut setup, None, &workspace, Action::List { after: None })
            .await
            .unwrap()
    else {
        panic!("list expected")
    };
    assert_eq!(entries[0].state, State::Unavailable);
    assert!(
        execute(
            &mut setup,
            None,
            &workspace,
            Action::Remove {
                id,
                expected: "pending".into(),
                run: None
            }
        )
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
    foreign
        .finish("cancelled", vec![], None)
        .await
        .unwrap();
    let moved = root.path().join("moved");
    std::fs::create_dir(&moved).unwrap();
    foreign
        .move_to(&moved, None)
        .await
        .unwrap();
    drop(foreign);
    assert!(
        execute(
            &mut setup,
            None,
            &workspace,
            Action::Edit {
                id,
                expected: "pending".into(),
                content: "stale workspace".into(),
                run: None
            }
        )
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
}

#[tokio::test]
async fn idle_inputs_keep_ids_and_order_and_pagination_accounts_for_json_escaping() {
    let root = tempfile::tempdir().unwrap();
    let mut setup = setup(root.path()).await;
    let workspace = setup
        .session
        .snapshot
        .session
        .workspace
        .clone();
    let content = "\0".repeat(800_000);
    let mut ids = Vec::new();
    for _ in 0..8 {
        ids.push(
            setup
                .session
                .store
                .queue_session_input(setup.session.id(), content.clone())
                .await
                .unwrap(),
        );
    }
    let mut after = None;
    let mut found = Vec::new();
    loop {
        let page = execute(&mut setup, None, &workspace, Action::List { after })
            .await
            .unwrap();
        assert!(serde_json::to_vec(&page).unwrap().len() < orbit_protocol::MAX_FRAME_BYTES);
        let Snapshot::List { entries, next, run } = page else {
            panic!("list expected")
        };
        assert_eq!(run, None);
        assert_eq!(
            entries.len(),
            1,
            "escaped payload exceeds the multi-row page budget"
        );
        assert!(entries[0].editable());
        found.push(entries[0].id);
        let Some(next) = next else { break };
        assert!(after.is_none_or(|after| next > after));
        after = Some(next);
    }
    assert_eq!(found, ids);
    execute(
        &mut setup,
        None,
        &workspace,
        Action::Edit {
            id: ids[3],
            expected: content.clone(),
            content: "edited idle".into(),
            run: None,
        },
    )
    .await
    .unwrap();
    execute(
        &mut setup,
        None,
        &workspace,
        Action::Remove {
            id: ids[1],
            expected: content,
            run: None,
        },
    )
    .await
    .unwrap();
    let pending = setup
        .session
        .store
        .input_queue(setup.session.id())
        .await
        .unwrap()
        .messages;
    assert_eq!(
        pending
            .iter()
            .map(|message| message.id)
            .collect::<Vec<_>>(),
        ids.iter()
            .copied()
            .filter(|id| *id != ids[1])
            .collect::<Vec<_>>()
    );
    assert_eq!(
        pending
            .iter()
            .find(|message| message.id == ids[3])
            .unwrap()
            .content,
        "edited idle"
    );
}
