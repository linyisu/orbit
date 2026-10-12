use super::*;
use orbit_protocol::{SessionAction, resources};

#[tokio::test]
async fn configured_capability_ceiling_does_not_change_reconnect_identity() {
    let fixture = Fixture::new("https://example.com/v1").await;
    let path = &fixture.service.options.config;
    let previous = fs::read_to_string(path).unwrap();
    fs::write(
        path,
        format!("disabled_capabilities = ['web', 'mcp']\n{previous}"),
    )
    .unwrap();
    let open = |resume, policy| Request::Open {
        model: Some("test::test".into()),
        policy,
        resume,
        configuration: None,
    };
    let Response::Opened(first) = fixture
        .request(open(None, Default::default()))
        .await
        .unwrap()
    else {
        panic!("expected new session");
    };
    let Response::Opened(reopened) = fixture
        .request(open(Some(first.id.clone()), Default::default()))
        .await
        .unwrap()
    else {
        panic!("expected reopened session");
    };
    assert_eq!(reopened.id, first.id);
    let handle = fixture.service.session(&first.id).await.unwrap();
    assert!(handle.policy.disabled(orbit_protocol::Capability::Web));
    assert!(handle.policy.disabled(orbit_protocol::Capability::Mcp));
    assert_eq!(handle.requested_policy, Default::default());
    let changed = orbit_protocol::SessionPolicy {
        read_only: true,
        disabled: vec![],
    };
    let error = fixture
        .request(open(Some(first.id), changed))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("different capability policy"));
}

#[tokio::test]
async fn settings_workspace_guard_is_checked_by_the_resource_actor() {
    let fixture = Fixture::new("https://example.com/v1").await;
    fs::create_dir(fixture.external.join(".git")).unwrap();
    let previous = fixture.handle.info.workspace.clone();
    fixture
        .handle
        .action(SessionAction::ChangeDirectory {
            path: fixture.external.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();
    let current = fixture.handle.current_info().await.workspace;
    assert_ne!(previous, current);

    // Bypass dispatch: a guard checked only before enqueuing the actor command
    // would not protect a request queued behind another client's directory change.
    let error = fixture
        .handle
        .resources(resources::Action::Skills, Some(previous.clone()))
        .await
        .unwrap_err();
    assert!(error.to_string().contains("工作区已切换"));

    let save = || resources::Action::SaveSkill {
        name: "scope-check".into(),
        original: None,
        content: "---\ndescription: Check settings scope\n---\nReview the project.\n".into(),
    };
    let error = fixture
        .request(Request::Resources {
            session: fixture.handle.info.id.clone(),
            expected_workspace: Some(previous.clone()),
            action: save(),
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("工作区已切换"));
    let relative = ".orbit/skills/scope-check/SKILL.md";
    assert!(!Path::new(&previous).join(relative).exists());
    assert!(!Path::new(&current).join(relative).exists());

    fixture
        .request(Request::Resources {
            session: fixture.handle.info.id.clone(),
            expected_workspace: Some(current.clone()),
            action: save(),
        })
        .await
        .unwrap();
    assert!(Path::new(&current).join(relative).is_file());
    let Response::Resources(resources::Snapshot::Skills(skills)) = fixture
        .request(Request::Resources {
            session: fixture.handle.info.id.clone(),
            expected_workspace: None,
            action: resources::Action::Skills,
        })
        .await
        .unwrap()
    else {
        panic!("missing legacy resource response");
    };
    assert!(skills.iter().any(|skill| skill.name == "scope-check"));
}

#[tokio::test]
async fn settings_workspace_guard_rejects_stale_plugin_mutations() {
    let fixture = Fixture::new("https://example.com/v1").await;
    let previous = fixture.handle.info.workspace.clone();
    fixture
        .handle
        .action(SessionAction::ChangeDirectory {
            path: fixture.external.to_string_lossy().into_owned(),
        })
        .await
        .unwrap();

    // The stale source must be rejected for its workspace before looking up or
    // modifying any marketplace. This avoids touching the user's registry.
    let error = fixture
        .request(Request::Plugins {
            session: fixture.handle.info.id.clone(),
            expected_workspace: Some(previous),
            action: orbit_protocol::plugins::Action::Remove {
                source: "scope-test-source-does-not-exist".into(),
            },
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("工作区已切换"));
}
