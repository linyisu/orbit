use super::*;
use clap::Parser;
use orbit_protocol::resources::{Action, Snapshot};

fn options(directory: &std::path::Path) -> Options {
    let mut options = Options::parse_from(["orbit-gui"]);
    options.config = directory.join("unused.toml");
    options.workspace = Some(directory.into());
    options.server = Some(env!("CARGO_BIN_EXE_orbit-gui-fixture").into());
    options.database = Some(directory.join("unused.db"));
    options.auth_dir = Some(directory.join("auth"));
    options.model = Some("fixture::local".into());
    options.resume = Some("demo".into());
    options
}

fn resources(session: &str, workspace: &str, action: Action) -> Request {
    Request::Resources {
        session: session.into(),
        expected_workspace: Some(workspace.into()),
        action,
    }
}

async fn request(backend: &Backend, request: Request) -> Result<Snapshot, String> {
    let (result, receiver) = tokio::sync::oneshot::channel();
    backend.send(Command::Settings { request, result }).unwrap();
    let response = tokio::time::timeout(Duration::from_secs(10), receiver)
        .await
        .expect("skill source request timed out")
        .expect("skill source request was abandoned")?;
    match response {
        Response::Resources(snapshot) => Ok(snapshot),
        other => panic!("expected resource snapshot, got {other:?}"),
    }
}

async fn scan_token(backend: &Backend, workspace: &str) -> String {
    let Snapshot::SkillSource { token, .. } = request(
        backend,
        resources(
            "demo",
            workspace,
            Action::ScanSkills {
                source: "fixture/skills".into(),
            },
        ),
    )
    .await
    .unwrap() else {
        panic!("source preview expected")
    };
    token
}

#[test]
fn skill_source_transport_previews_conflicts_and_imports_only_selected_paths() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join(".fixture-skill-import"), "").unwrap();
    let workspace = directory.path().to_string_lossy().into_owned();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (backend, mut events) = Backend::start(options(directory.path()), &runtime);
    runtime.block_on(async {
        until(&mut events, |event| {
            matches!(event, Event::Response(Response::Opened(_)))
        })
        .await;
        let command = |action| resources("demo", &workspace, action);
        let Snapshot::Skill {
            content: original, ..
        } = request(
            &backend,
            command(Action::ReadSkill {
                name: "review".into(),
            }),
        )
        .await
        .unwrap()
        else {
            panic!("existing skill expected")
        };
        let Snapshot::SkillSource {
            token,
            source,
            skills,
        } = request(
            &backend,
            command(Action::ScanSkills {
                source: " fixture/skills ".into(),
            }),
        )
        .await
        .unwrap()
        else {
            panic!("source preview expected")
        };
        assert_eq!(source, "fixture/skills");
        assert!(!token.is_empty());
        assert_eq!(skills.len(), 3);
        assert!(
            skills
                .iter()
                .all(|skill| !skill.title.is_empty() && !skill.description.is_empty())
        );
        assert!(
            skills
                .iter()
                .find(|skill| skill.name == "review")
                .unwrap()
                .conflict
        );
        let workflow = skills
            .iter()
            .find(|skill| skill.name == "workflow")
            .unwrap();
        assert_eq!(workflow.path, "skills/工具/workflow");
        assert!(!workflow.conflict);
        let Snapshot::Skills(before) = request(&backend, command(Action::Skills)).await.unwrap()
        else {
            panic!("skills expected")
        };
        assert_eq!(
            before.len(),
            1,
            "source preview must not install candidates"
        );

        // A path must identify the full candidate, and the entire selection is checked first.
        let import = |token: &str, paths| {
            command(Action::ImportSkills {
                token: token.into(),
                paths,
            })
        };
        assert!(
            request(
                &backend,
                import(
                    &token,
                    vec![workflow.path.clone(), "frontend-design".into()]
                )
            )
            .await
            .unwrap_err()
            .contains("不在此来源")
        );
        let Snapshot::Skills(after_rejection) =
            request(&backend, command(Action::Skills)).await.unwrap()
        else {
            panic!("skills expected")
        };
        assert_eq!(
            after_rejection.len(),
            1,
            "an invalid selection must not partially install"
        );
        assert!(
            request(&backend, import(&token, vec![workflow.path.clone()]))
                .await
                .unwrap_err()
                .contains("已过期")
        );
        let token = scan_token(&backend, &workspace).await;
        assert!(
            request(&backend, import(&token, vec!["skills/review".into()]))
                .await
                .unwrap_err()
                .contains("已存在")
        );
        let token = scan_token(&backend, &workspace).await;
        let Snapshot::Skills(installed) =
            request(&backend, import(&token, vec![workflow.path.clone()]))
                .await
                .unwrap()
        else {
            panic!("installed skills expected")
        };
        assert_eq!(
            installed
                .iter()
                .map(|skill| skill.name.as_str())
                .collect::<Vec<_>>(),
            ["review", "workflow"]
        );
        let Snapshot::Skill { content, .. } = request(
            &backend,
            command(Action::ReadSkill {
                name: "workflow".into(),
            }),
        )
        .await
        .unwrap() else {
            panic!("imported skill expected")
        };
        assert!(content.contains(&workflow.path));
        let Snapshot::Skill { content, .. } = request(
            &backend,
            command(Action::ReadSkill {
                name: "review".into(),
            }),
        )
        .await
        .unwrap() else {
            panic!("existing skill expected")
        };
        assert_eq!(
            content, original,
            "installing another skill preserves local content"
        );
        assert!(
            request(
                &backend,
                import(&token, vec!["skills/frontend-design".into()])
            )
            .await
            .unwrap_err()
            .contains("已过期")
        );
    });
}

#[test]
fn skill_source_transport_rejects_failed_sources_stale_tokens_and_wrong_scope() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join(".fixture-skill-import"), "").unwrap();
    let workspace = directory.path().to_string_lossy().into_owned();
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (backend, mut events) = Backend::start(options(directory.path()), &runtime);
    runtime.block_on(async {
        until(&mut events, |event| matches!(event, Event::Response(Response::Opened(_)))).await;
        let command = |action| resources("demo", &workspace, action);
        assert!(request(&backend, command(Action::ScanSkills { source: "bad-source".into() })).await.unwrap_err().contains("无法读取技能来源"));
        assert!(request(&backend, command(Action::ScanSkills { source: "  ".into() })).await.unwrap_err().contains("请输入技能来源"));
        let Snapshot::SkillSource { token, .. } = request(&backend, command(Action::ScanSkills { source: "fixture/skills".into() })).await.unwrap() else { panic!("source preview expected") };
        let import = || Action::ImportSkills { token: token.clone(), paths: vec!["skills/frontend-design".into()] };
        assert!(request(&backend, resources("demo", "previous-workspace", import())).await.unwrap_err().contains("工作区已切换"));
        assert!(request(&backend, resources("another-session", &workspace, import())).await.is_err());
        assert!(request(&backend, command(Action::ImportSkills { token: "expired-token".into(), paths: vec!["skills/frontend-design".into()] })).await.unwrap_err().contains("已过期"));
        let Snapshot::Skills(skills) = request(&backend, command(Action::Skills)).await.unwrap() else { panic!("skills expected") };
        assert_eq!(skills.len(), 1, "rejected scope/token requests cannot modify skills");
        assert!(matches!(request(&backend, command(import())).await.unwrap(), Snapshot::Skills(skills) if skills.len() == 2));
    });
}
