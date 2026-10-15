use super::*;

fn tab(target: Target, id: &str) -> SavedTab {
    SavedTab {
        id: Some(id.into()),
        key: String::new(),
        draft: Draft::default(),
        title: id.into(),
        target,
        workspace: "/project".into(),
        config: "/config.toml".into(),
        auth: "/auth".into(),
        database: None,
        allow_mcp_start: false,
        server: None,
    }
}

#[tokio::test]
async fn last_window_merges_targets_and_crashed_window_records() {
    let root = tempfile::tempdir().unwrap();
    let first = TuiWindow::open(root.path().into())
        .await
        .unwrap();
    let mut crashed = TuiWindow::open(root.path().into())
        .await
        .unwrap();
    let local = tab(Target::Local, "same-id");
    let ssh = tab(
        Target::Ssh {
            host: "dev".into(),
            options: Default::default(),
        },
        "same-id",
    );
    first
        .update(vec![local.clone()])
        .await
        .unwrap();
    crashed
        .update(vec![ssh.clone(), local.clone()])
        .await
        .unwrap();
    crashed.lease.take();
    drop(crashed);
    first.close().await.unwrap();
    let restored = TuiWindow::open(root.path().into())
        .await
        .unwrap();
    assert_eq!(restored.previous().len(), 2);
    assert!(restored.previous().contains(&local));
    assert!(restored.previous().contains(&ssh));
    restored.close().await.unwrap();
}

#[tokio::test]
async fn new_windows_restore_live_tabs_without_overwriting_them_with_an_empty_window() {
    let root = tempfile::tempdir().unwrap();
    let previous = tab(Target::Local, "previous");
    let first = TuiWindow::open(root.path().into())
        .await
        .unwrap();
    first
        .update(vec![previous.clone()])
        .await
        .unwrap();
    first.close().await.unwrap();
    let second = TuiWindow::open(root.path().into())
        .await
        .unwrap();
    second
        .update(vec![tab(Target::Local, "running")])
        .await
        .unwrap();
    let third = TuiWindow::open(root.path().into())
        .await
        .unwrap();
    assert_eq!(second.previous(), [previous]);
    assert_eq!(third.previous(), [tab(Target::Local, "running")]);
    third.close().await.unwrap();
    second.close().await.unwrap();
}

#[tokio::test]
async fn legacy_tabs_seed_only_the_first_client_registry() {
    let root = tempfile::tempdir().unwrap();
    let legacy = tab(Target::Local, "legacy");
    let migrated = TuiWindow::open_with_previous(root.path().into(), vec![legacy.clone()])
        .await
        .unwrap();
    assert_eq!(migrated.previous(), [legacy]);
    let current = tab(Target::Local, "current");
    migrated
        .update(vec![current.clone()])
        .await
        .unwrap();
    migrated.close().await.unwrap();
    let next = TuiWindow::open_with_previous(root.path().into(), vec![tab(Target::Local, "stale")])
        .await
        .unwrap();
    assert_eq!(next.previous(), [current]);
    next.close().await.unwrap();
}
fn profile(root: &Path) -> Profile {
    Profile {
        config: root.join("settings.toml"),
        auth: root.join("auth"),
        database: None,
    }
}

#[test]
fn profiles_restore_drafts_assets_selection_and_keep_distinct_data_sources() {
    let root = tempfile::tempdir().unwrap();
    let profile = profile(root.path());
    let desktop = ClientWindow::open_profile(&profile, None).unwrap();
    let mut first = tab(Target::Local, "shared-id");
    first.key = "first-tab".into();
    first.config = profile.config.clone();
    first.auth = profile.auth.clone();
    let mut second = first.clone();
    second.key = "second-tab".into();
    second.database = Some(root.path().join("separate.db"));
    second.draft.text = "Generated draft with independent material".into();
    let asset = root.path().join("generated.txt");
    std::fs::write(&asset, b"independent file material").unwrap();
    second
        .draft
        .attachments
        .push(DraftAttachment {
            name: "notes.txt".into(),
            path: asset.clone(),
        });
    let mut fresh = first.clone();
    fresh.id = None;
    fresh.key = "unsent-tab".into();
    fresh.draft.text = "Unsent generated task".into();
    desktop
        .update_blocking(vec![first, second, fresh], Some(1))
        .unwrap();
    drop(desktop);
    std::fs::remove_file(asset).unwrap();
    let terminal = ClientWindow::open_profile(&profile, None).unwrap();
    assert_eq!(terminal.previous().len(), 3);
    assert_eq!(terminal.active(), Some(1));
    assert_eq!(terminal.previous()[2].id, None);
    let saved = &terminal.previous()[1].draft;
    assert_eq!(saved.text, "Generated draft with independent material");
    assert_eq!(saved.attachments[0].name, "notes.txt");
    assert_eq!(
        std::fs::read(&saved.attachments[0].path).unwrap(),
        b"independent file material"
    );
    assert!(!saved.attachment_handles()[0].is_image());
    let cached = saved.attachments[0].path.clone();
    terminal
        .update_blocking(Vec::new(), None)
        .unwrap();
    drop(terminal);
    assert!(
        !cached.exists(),
        "closed drafts release unreferenced private assets"
    );
    let other = Profile {
        database: Some(root.path().join("other.db")),
        ..profile
    };
    assert!(
        ClientWindow::open_profile(&other, None)
            .unwrap()
            .previous()
            .is_empty()
    );
}

#[test]
fn closed_tabs_do_not_return_from_a_stale_desktop_import() {
    let root = tempfile::tempdir().unwrap();
    let profile = profile(root.path());
    let source = root.path().join("desktop.json");
    std::fs::write(&source, b"legacy metadata").unwrap();
    let imported = tab(Target::Local, "legacy-session");
    let desktop = ClientWindow::open_profile(
        &profile,
        Some(LegacyTabs {
            source: source.clone(),
            tabs: vec![imported.clone()],
            active: Some(0),
        }),
    )
    .unwrap();
    assert_eq!(desktop.previous(), std::slice::from_ref(&imported));
    desktop
        .update_blocking(Vec::new(), None)
        .unwrap();
    drop(desktop);
    std::fs::write(&source, b"newer view-only metadata").unwrap();
    let terminal = ClientWindow::open_profile(
        &profile,
        Some(LegacyTabs {
            source,
            tabs: vec![imported],
            active: Some(0),
        }),
    )
    .unwrap();
    assert!(terminal.previous().is_empty());
}

#[test]
fn interrupted_startup_keeps_the_existing_restore_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let profile = profile(root.path());
    let original = ClientWindow::open_profile(&profile, None).unwrap();
    original
        .update_blocking(vec![tab(Target::Local, "retained")], Some(0))
        .unwrap();
    drop(original);
    let mut interrupted = ClientWindow::open_profile(&profile, None).unwrap();
    interrupted.lease.take();
    drop(interrupted);
    assert_eq!(
        ClientWindow::open_profile(&profile, None)
            .unwrap()
            .previous()[0]
            .id
            .as_deref(),
        Some("retained")
    );
}

#[test]
fn desktop_legacy_migration_is_typed_scoped_and_preserves_remote_drafts() {
    let root = tempfile::tempdir().unwrap();
    let profile = profile(root.path());
    let source = root.path().join("desktop.json");
    let origin =
        serde_json::json!({ "config": profile.config, "database": null, "auth_dir": profile.auth });
    let mut options = origin.clone();
    options["workspace"] = serde_json::json!("~/generated-project");
    options["resume"] = serde_json::json!("generated-history");
    options["ssh"] = serde_json::json!("generated-host");
    options["ssh_port"] = serde_json::json!(2_244);
    options["server"] = serde_json::json!("/opt/generated/server");
    std::fs::write(
        &source,
        serde_json::to_vec(&serde_json::json!({
            "origin": origin,
            "active": "generated-key",
            "tasks": [{
                "key": "generated-key",
                "options": options,
                "title": "Generated remote task",
                "text": "Independent draft",
                "paths": [],
            }],
        }))
        .unwrap(),
    )
    .unwrap();
    let imported = legacy_desktop(&source, &profile)
        .unwrap()
        .unwrap();
    assert_eq!(imported.active, Some(0));
    assert_eq!(imported.tabs[0].draft.text, "Independent draft");
    assert_eq!(
        imported.tabs[0].server.as_deref(),
        Some("/opt/generated/server")
    );
    assert!(
        matches!(&imported.tabs[0].target, Target::Ssh { options, .. } if options.port == Some(2_244))
    );
    let other = Profile {
        database: Some(root.path().join("other.db")),
        ..profile
    };
    assert!(
        legacy_desktop(&source, &other)
            .unwrap()
            .is_none()
    );
}

#[test]
fn concurrent_clients_keep_divergent_drafts_and_unique_view_keys() {
    let root = tempfile::tempdir().unwrap();
    let profile = profile(root.path());
    let first = ClientWindow::open_profile(&profile, None).unwrap();
    let mut draft = tab(Target::Local, "shared-session");
    draft.key = "shared-key".into();
    draft.draft.text = "First generated draft".into();
    first
        .update_blocking(vec![draft.clone()], Some(0))
        .unwrap();
    let second = ClientWindow::open_profile(&profile, None).unwrap();
    assert_eq!(second.previous()[0].draft.text, draft.draft.text);
    draft.draft.text = "Second independent draft".into();
    second
        .update_blocking(vec![draft], Some(0))
        .unwrap();
    let third = ClientWindow::open_profile(&profile, None).unwrap();
    assert_eq!(third.previous().len(), 2);
    assert_ne!(third.previous()[0].key, third.previous()[1].key);
    assert_eq!(third.active(), Some(1));
}
