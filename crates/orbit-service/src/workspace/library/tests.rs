use super::*;
use base64::Engine;
use orbit_config::test_support::{TestConfig, TestProvider};
use orbit_protocol::{
    library::{Bundle, PackageFile},
    resources::Action as ResourceAction,
};
use std::fs;

fn options(root: &std::path::Path) -> crate::WorkspaceOptions {
    fs::create_dir_all(root.join("workspace/.git")).unwrap();
    fs::create_dir_all(root.join("home")).unwrap();
    TestConfig::new()
        .provider(TestProvider::new("test").models(["mock"]))
        .write(root.join("config.toml"))
        .unwrap();
    crate::WorkspaceOptions {
        model: Some("test::mock".into()),
        initial_directory: root.join("workspace"),
        config: root.join("config.toml"),
        auth_directory: root.join("auth"),
        database: root.join("sessions.db"),
        home: Some(root.join("home")),
        config_home: Some(root.join("config")),
        allow_mcp_start: false,
        instruction_dir: None,
        editor_command: "vi".into(),
    }
}

fn contents(response: resources::Snapshot) -> Vec<Skill> {
    let resources::Snapshot::Skills(skills) = response else {
        panic!("skill catalog expected")
    };
    skills
}

async fn apply(
    setup: &mut crate::PreparedWorkspace,
    bundle: Bundle,
) -> Result<resources::Snapshot> {
    super::super::resources::execute(
        setup,
        ResourceAction::ApplyExtension {
            bundle: bundle.into(),
        },
        false,
    )
    .await
}

#[tokio::test]
async fn global_management_is_independent_and_application_is_durable_and_session_owned() {
    let root = tempfile::tempdir().unwrap();
    let options = options(root.path());
    let mut library = Library::isolated(&options);
    let body = "---\ndescription: Inspect generated metadata\n---\nCompare the declared fields.";
    let skills = contents(
        library
            .manage_skills(ResourceAction::SaveSkill {
                name: "metadata-check".into(),
                original: None,
                content: body.into(),
            })
            .await
            .unwrap(),
    );
    assert_eq!(skills.len(), 1);
    assert!(
        root.path()
            .join("config/orbit/skills/metadata-check/SKILL.md")
            .is_file()
    );
    assert!(
        !options
            .initial_directory
            .join(".orbit")
            .exists()
    );
    let mut first = super::super::setup::prepare_service(&options, None)
        .await
        .unwrap();
    let second = super::super::setup::prepare_service(&options, None)
        .await
        .unwrap();
    assert!(first.resources.budget.skills.is_empty());
    assert!(
        second
            .resources
            .budget
            .skills
            .is_empty()
    );
    let Snapshot::Bundle(bundle) = library
        .execute(Action::ExportSkill {
            name: "metadata-check".into(),
        })
        .await
        .unwrap()
    else {
        panic!("bundle expected")
    };
    assert!(
        super::super::resources::execute(
            &mut first,
            ResourceAction::ApplyExtension {
                bundle: Box::new(bundle.clone())
            },
            true
        )
        .await
        .is_err()
    );
    assert!(!first.session.is_persisted());
    apply(&mut first, bundle.clone())
        .await
        .unwrap();
    assert!(first.session.is_persisted());
    assert_eq!(first.resources.budget.skills.len(), 1);
    assert!(
        second
            .resources
            .budget
            .skills
            .is_empty()
    );
    assert!(
        second
            .session
            .store
            .session_extensions(second.session.id())
            .await
            .unwrap()
            .is_empty()
    );
    let old_skill = first.resources.budget.skills.clone();
    library
        .manage_skills(ResourceAction::SaveSkill {
            name: "metadata-check".into(),
            original: Some(body.into()),
            content: "Updated global definition.".into(),
        })
        .await
        .unwrap();
    let id = first.session.id().to_owned();
    drop(first);
    fs::remove_dir_all(root.path().join("config/orbit/skills")).unwrap();
    let mut restored = super::super::setup::prepare_service(&options, Some(id))
        .await
        .unwrap();
    let path = &restored
        .resources
        .budget
        .skills
        .get("metadata-check")
        .unwrap()
        .path;
    assert_eq!(fs::read_to_string(path).unwrap(), body);
    assert_eq!(
        fs::read_to_string(
            &old_skill
                .get("metadata-check")
                .unwrap()
                .path
        )
        .unwrap(),
        body
    );
    let invalid = Bundle {
        files: vec![PackageFile {
            path: "../escape".into(),
            content: String::new(),
            executable: false,
        }],
        ..bundle.clone()
    };
    assert!(
        apply(&mut restored, invalid)
            .await
            .is_err()
    );
    assert_eq!(
        restored
            .session
            .store
            .session_extensions(restored.session.id())
            .await
            .unwrap()
            .len(),
        1
    );
    super::super::resources::execute(
        &mut restored,
        ResourceAction::RemoveExtension {
            key: bundle.extension.key,
        },
        false,
    )
    .await
    .unwrap();
    assert!(
        restored
            .resources
            .budget
            .skills
            .is_empty()
    );
    assert!(
        restored
            .session
            .store
            .session_extensions(restored.session.id())
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn global_import_freezes_resources_without_writing_any_project() {
    let root = tempfile::tempdir().unwrap();
    let options = options(root.path());
    let mut library = Library::isolated(&options);
    let source = root
        .path()
        .join("incoming/format-audit");
    fs::create_dir_all(source.join("references")).unwrap();
    fs::write(
        source.join("SKILL.md"),
        "---\ndescription: Audit formatting\n---\nRead references/rules.md.",
    )
    .unwrap();
    fs::write(
        source.join("references/rules.md"),
        "Generated reference version one.",
    )
    .unwrap();
    let resources::Snapshot::SkillSource { token, skills, .. } = library
        .manage_skills(ResourceAction::ScanSkills {
            source: source.display().to_string(),
        })
        .await
        .unwrap()
    else {
        panic!("scan expected")
    };
    assert_eq!(skills.len(), 1);
    fs::write(
        source.join("references/rules.md"),
        "Changed outside the snapshot.",
    )
    .unwrap();
    library
        .manage_skills(ResourceAction::ImportSkills {
            token,
            paths: vec![".".into()],
        })
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(
            root.path()
                .join("config/orbit/skills/format-audit/references/rules.md")
        )
        .unwrap(),
        "Generated reference version one."
    );
    assert!(
        !options
            .initial_directory
            .join(".orbit")
            .exists()
    );
    let Snapshot::Bundle(bundle) = library
        .execute(Action::ExportSkill {
            name: "format-audit".into(),
        })
        .await
        .unwrap()
    else {
        panic!("bundle expected")
    };
    let reference = bundle
        .files
        .iter()
        .find(|file| {
            file.path
                .ends_with("references/rules.md")
        })
        .unwrap();
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(&reference.content)
            .unwrap(),
        b"Generated reference version one."
    );
}

#[tokio::test]
async fn global_market_updates_do_not_change_applied_plugin_snapshots() {
    let root = tempfile::tempdir().unwrap();
    let options = options(root.path());
    let mut library = Library::isolated(&options);
    let market = root.path().join("catalog-source");
    fs::create_dir_all(market.join(".claude-plugin")).unwrap();
    fs::create_dir_all(market.join("plugins/format-tools/.claude-plugin")).unwrap();
    fs::create_dir_all(market.join("plugins/format-tools/skills/format-review")).unwrap();
    fs::write(
        market.join(".claude-plugin/marketplace.json"),
        concat!(
            r#"{"name":"generated-catalog","plugins":["#,
            r#"{"name":"format-tools","source":"./plugins/format-tools"}]}"#,
        ),
    )
    .unwrap();
    fs::write(
        market.join("plugins/format-tools/.claude-plugin/plugin.json"),
        r#"{"name":"format-tools","version":"1.0.0"}"#,
    )
    .unwrap();
    fs::write(
        market.join("plugins/format-tools/skills/format-review/SKILL.md"),
        "---\ndescription: Review spacing\n---\nVerify generated indentation.",
    )
    .unwrap();
    let source = market.display().to_string();
    let catalog = library
        .manage_plugins(plugins::Action::Add {
            source: source.clone(),
        })
        .await
        .unwrap();
    assert!(catalog.applied.is_empty());
    let selection = plugins::Selection {
        marketplace_source: source.clone(),
        plugin_name: "format-tools".into(),
        package_source: "./plugins/format-tools".into(),
        version: Some("1.0.0".into()),
        marketplace_revision: None,
    };
    let Snapshot::Bundle(bundle) = library
        .execute(Action::ExportPlugin {
            plugin: selection.clone(),
        })
        .await
        .unwrap()
    else {
        panic!("bundle expected")
    };
    let mut session = super::super::setup::prepare_service(&options, None)
        .await
        .unwrap();
    apply(&mut session, bundle)
        .await
        .unwrap();
    library
        .manage_plugins(plugins::Action::Remove { source })
        .await
        .unwrap();
    assert!(
        library
            .manage_plugins(plugins::Action::List)
            .await
            .unwrap()
            .marketplaces
            .is_empty()
    );
    let id = session.session.id().to_owned();
    drop(session);
    let restored = super::super::setup::prepare_service(&options, Some(id))
        .await
        .unwrap();
    assert!(
        restored
            .resources
            .budget
            .skills
            .get("format-review")
            .is_some()
    );
    let stored = restored
        .session
        .store
        .session_extensions(restored.session.id())
        .await
        .unwrap();
    assert_eq!(stored[0].extension.kind, ExtensionKind::Plugin(selection));
}

#[test]
fn received_packages_reject_duplicate_paths_missing_roots_and_skill_collisions() {
    let original = test_bundle(
        "generated-audit",
        "---\ndescription: Generated audit\n---\nInspect fields.",
    );
    let mut duplicate = original.clone();
    duplicate
        .files
        .push(duplicate.files[0].clone());
    assert!(package::validate(&duplicate).is_err());
    let mut missing = original.clone();
    missing.skill_roots = vec!["0/missing".into()];
    assert!(package::validate(&missing).is_err());
    assert!(package::materialize(&[original.clone(), original]).is_err());
}

#[test]
fn skill_edits_preserve_conflicts_and_reject_traversal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("demo/SKILL.md");
    assert!(save_skill(&path, "../bad", None, "hello").is_err());
    save_skill(&path, "demo", None, "first").unwrap();
    assert!(save_skill(&path, "demo", None, "replace").is_err());
    assert!(save_skill(&path, "demo", Some("stale"), "replace").is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), "first");
    save_skill(&path, "demo", Some("first"), "second").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "second");
}
