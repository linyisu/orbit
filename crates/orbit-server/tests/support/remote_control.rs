use super::*;
use orbit_client::remote::{Client as RemoteClient, Pairing, Request, Response};

fn server(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_orbit-workspace-server"));
    command
        .args(["--config"])
        .arg(root.join("config.toml"))
        .arg("--database")
        .arg(root.join("sessions.db"))
        .arg("--auth-dir")
        .arg(root.join("auth"))
        .arg("remote")
        .arg("--state")
        .arg(root.join("remote"))
        .arg("--workspace")
        .arg(root)
        .env("HOME", root)
        .env("XDG_CONFIG_HOME", root.join("config-home"))
        .kill_on_drop(true);
    command
}

#[tokio::test]
async fn authenticated_phone_controls_the_shared_session_and_revocation_closes_access() {
    tokio::time::timeout(Duration::from_secs(90), async {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let (url, model) = mock_model().await;
        std::fs::write(
            root.join("config.toml"),
            TestConfig::new()
                .provider(
                    TestProvider::new("test")
                        .endpoint(&url)
                        .key("test-only")
                        .models([TestModel::new("mock")]),
                )
                .providers_toml()
                .unwrap(),
        )
        .unwrap();
        let mut local = Client::connect(root).await;
        let opened = local
            .request(json!({ "method": "open", "resume": null, "model": "test::mock" }))
            .await;
        let session = opened["value"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        assert_eq!(
            local
                .request(json!({ "method": "set_mode", "session": session, "mode": "ask" }))
                .await["type"],
            "profile"
        );
        let reservation = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let address = reservation
            .local_addr()
            .unwrap()
            .to_string();
        let output = server(root)
            .args([
                "authorize",
                "--address",
                &address,
                "--control",
                "--approve",
                "--output",
            ])
            .arg(root.join("phone.json"))
            .output()
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let pairing: Pairing =
            serde_json::from_slice(&std::fs::read(root.join("phone.json")).unwrap()).unwrap();
        drop(reservation);
        let mut gateway = server(root)
            .args(["listen", "--bind", &address])
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let mut phone = loop {
            if let Ok(client) = RemoteClient::connect(&pairing).await {
                break client;
            }
            assert!(gateway.try_wait().unwrap().is_none());
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        let mut invalid = pairing.clone();
        invalid.secret = "invalid-generated-secret".into();
        assert!(
            RemoteClient::connect(&invalid)
                .await
                .is_err()
        );
        assert!(matches!(
            phone
                .request(Request::Browse {
                    resume: Some(session.clone())
                })
                .await
                .unwrap(),
            Response::Opened(_)
        ));
        let readonly_output = server(root)
            .args(["authorize", "--address", &address, "--output"])
            .arg(root.join("readonly.json"))
            .output()
            .await
            .unwrap();
        assert!(readonly_output.status.success());
        let readonly: Pairing =
            serde_json::from_slice(&std::fs::read(root.join("readonly.json")).unwrap()).unwrap();
        let mut reader = RemoteClient::connect(&readonly)
            .await
            .unwrap();
        assert!(matches!(
            reader
                .request(Request::Browse {
                    resume: Some(session.clone())
                })
                .await
                .unwrap(),
            Response::Opened(_)
        ));
        assert!(matches!(
            reader
                .request(Request::Cancel {
                    session: session.clone()
                })
                .await
                .unwrap(),
            Response::Error(_)
        ));
        drop(reader);
        let mut reader = RemoteClient::connect(&readonly)
            .await
            .unwrap();
        assert!(matches!(
            reader
                .request(Request::Browse {
                    resume: Some(session.clone())
                })
                .await
                .unwrap(),
            Response::Opened(_)
        ));
        assert!(matches!(
            reader
                .request(Request::UploadBegin {
                    session: session.clone(),
                    id: "denied-upload".into(),
                    name: "example.txt".into(),
                    bytes: 3,
                    sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
                        .into(),
                    image: false,
                })
                .await
                .unwrap(),
            Response::Error(_)
        ));
        drop(reader);
        let mut reader = RemoteClient::connect(&readonly)
            .await
            .unwrap();
        assert!(matches!(
            reader
                .request(Request::Browse { resume: None })
                .await
                .unwrap(),
            Response::Error(_)
        ));
        drop(reader);
        let unrelated = tempfile::tempdir().unwrap();
        let identity = server(unrelated.path())
            .args(["authorize", "--address", &address, "--output"])
            .arg(unrelated.path().join("phone.json"))
            .output()
            .await
            .unwrap();
        assert!(identity.status.success());
        let other: Pairing =
            serde_json::from_slice(&std::fs::read(unrelated.path().join("phone.json")).unwrap())
                .unwrap();
        let mut wrong_host = pairing.clone();
        wrong_host.certificate = other.certificate;
        assert!(
            RemoteClient::connect(&wrong_host)
                .await
                .is_err()
        );
        let start = || Request::Submit {
            session: session.clone(),
            request_id: "remote-generated-request".into(),
            prompt: "Create the generated fixture file".into(),
            attachments: vec!["generated-upload".into()],
        };
        assert!(matches!(
            phone
                .request(Request::UploadBegin {
                    session: session.clone(),
                    id: "generated-upload".into(),
                    name: "example.txt".into(),
                    bytes: 3,
                    sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
                        .into(),
                    image: false,
                })
                .await
                .unwrap(),
            Response::Ok
        ));
        assert!(matches!(
            phone
                .request(Request::UploadChunk {
                    id: "generated-upload".into(),
                    offset: 0,
                    data: "YWJj".into(),
                })
                .await
                .unwrap(),
            Response::Ok
        ));
        assert!(matches!(
            phone
                .request(Request::Submit {
                    session: session.clone(),
                    request_id: "remote-generated-request".into(),
                    prompt: "Create the generated fixture file".into(),
                    attachments: vec!["generated-upload".into()],
                })
                .await
                .unwrap(),
            Response::Ok
        ));
        let approval = loop {
            let Response::Page(page) = phone
                .request(Request::Poll {
                    session: session.clone(),
                    after: 0,
                })
                .await
                .unwrap()
            else {
                panic!("expected page");
            };
            if let Some(approval) = page.approvals.first() {
                break approval.id;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        drop(phone);
        assert!(!root.join("result.txt").exists());
        let mut phone = RemoteClient::connect(&pairing)
            .await
            .unwrap();
        phone
            .request(Request::Browse {
                resume: Some(session.clone()),
            })
            .await
            .unwrap();
        assert!(matches!(
            phone
                .request(Request::UploadBegin {
                    session: session.clone(),
                    id: "generated-upload".into(),
                    name: "example.txt".into(),
                    bytes: 3,
                    sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
                        .into(),
                    image: false,
                })
                .await
                .unwrap(),
            Response::Ok
        ));
        assert!(matches!(
            phone
                .request(Request::UploadChunk {
                    id: "generated-upload".into(),
                    offset: 0,
                    data: "YWJj".into(),
                })
                .await
                .unwrap(),
            Response::Ok
        ));
        assert!(matches!(
            phone.request(start()).await.unwrap(),
            Response::Ok
        ));
        assert!(matches!(
            phone
                .request(Request::Approve {
                    session: session.clone(),
                    approval,
                    allow: true
                })
                .await
                .unwrap(),
            Response::Ok
        ));
        loop {
            let Response::Page(page) = phone
                .request(Request::Poll {
                    session: session.clone(),
                    after: 0,
                })
                .await
                .unwrap()
            else {
                panic!("expected page");
            };
            if !page.busy {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert_eq!(
            std::fs::read_to_string(root.join("result.txt")).unwrap(),
            "written remotely"
        );
        let revoked = server(root)
            .args(["revoke", &pairing.device])
            .output()
            .await
            .unwrap();
        assert!(revoked.status.success());
        assert!(
            phone
                .request(Request::Sessions)
                .await
                .is_err()
        );
        assert!(
            RemoteClient::connect(&pairing)
                .await
                .is_err()
        );
        gateway.kill().await.unwrap();
        local
            .request(json!({ "method": "shutdown" }))
            .await;
        model.abort();
    })
    .await
    .expect("remote control test timed out");
}

#[tokio::test]
#[ignore = "requires public relay connectivity"]
async fn public_iroh_desktop_pairing_opens_existing_sessions_and_revokes_devices() {
    use orbit_protocol::remote::{HostAction, Permissions};
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let mut desktop = Client::connect(root).await;
    let opened = desktop
        .request(json!({"method":"browse","resume":null}))
        .await;
    let session = opened["value"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let action = |action: HostAction| {
        serde_json::to_value(orbit_protocol::Request::RemoteHost { action }).unwrap()
    };
    let enabled = desktop
        .request(action(HostAction::Enable))
        .await;
    assert_eq!(enabled["type"], "remote_host", "{enabled}");
    assert_eq!(enabled["value"]["enabled"], true);
    let invitation = desktop
        .request(action(HostAction::Invite {
            permissions: Permissions {
                control: true,
                approve: false,
            },
        }))
        .await;
    let code = invitation["value"]["invitation"]
        .as_str()
        .unwrap()
        .to_owned();
    let pairing = orbit_client::remote::pair(&code)
        .await
        .unwrap();
    let mut phone = RemoteClient::connect(&pairing)
        .await
        .unwrap();
    assert!(matches!(
        phone
            .request(Request::Browse {
                resume: Some(session)
            })
            .await
            .unwrap(),
        Response::Opened(_)
    ));
    assert!(matches!(
        phone
            .request(Request::Sessions)
            .await
            .unwrap(),
        Response::Sessions(_)
    ));
    desktop
        .request(action(HostAction::Revoke {
            device: pairing.device,
        }))
        .await;
    assert!(
        phone
            .request(Request::Sessions)
            .await
            .is_err()
    );
    desktop
        .request(action(HostAction::Disable))
        .await;
    desktop
        .request(json!({"method":"shutdown"}))
        .await;
}

#[tokio::test]
async fn computer_pairing_switches_projects_and_uses_desktop_operations() {
    tokio::time::timeout(Duration::from_secs(90), async {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let first = root.join("first-project");
        let second = root.join("second-project");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        std::fs::write(second.join("sample.txt"), "generated sample").unwrap();
        let reservation = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let address = reservation
            .local_addr()
            .unwrap()
            .to_string();
        let output = server(root)
            .args([
                "authorize",
                "--address",
                &address,
                "--control",
                "--approve",
                "--output",
            ])
            .arg(root.join("controller.json"))
            .output()
            .await
            .unwrap();
        assert!(output.status.success());
        let pairing: Pairing =
            serde_json::from_slice(&std::fs::read(root.join("controller.json")).unwrap()).unwrap();
        drop(reservation);
        let mut gateway = server(root)
            .args(["listen", "--bind", &address])
            .spawn()
            .unwrap();
        let mut phone = loop {
            if let Ok(client) = RemoteClient::connect(&pairing).await {
                break client;
            }
            assert!(gateway.try_wait().unwrap().is_none());
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        let denied = phone
            .request(Request::Workspace {
                path: first.display().to_string(),
                resume: None,
            })
            .await
            .unwrap();
        assert!(
            matches!(&denied, Response::Error(message) if message.contains("重新扫码")),
            "{denied:?}"
        );
        drop(phone);
        // Simulate the explicit computer grant issued by confirmed QR pairing.
        let grant_path = root
            .join("remote")
            .join(format!("device-{}.json", pairing.device));
        let mut grant: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&grant_path).unwrap()).unwrap();
        grant["scope"] = json!("computer");
        std::fs::write(grant_path, serde_json::to_vec(&grant).unwrap()).unwrap();
        let mut phone = RemoteClient::connect(&pairing)
            .await
            .unwrap();
        for project in [&first, &second] {
            let Response::Opened(info) = phone
                .request(Request::Workspace {
                    path: project.display().to_string(),
                    resume: None,
                })
                .await
                .unwrap()
            else {
                panic!("expected draft session");
            };
            assert_eq!(Path::new(&info.workspace), project.canonicalize().unwrap());
            let mut reader = RemoteClient::connect(&pairing)
                .await
                .unwrap();
            assert!(matches!(
                reader
                    .request(Request::Workspace {
                        path: info.workspace.clone(),
                        resume: Some(info.id.clone())
                    })
                    .await
                    .unwrap(),
                Response::Opened(_)
            ));
            assert!(matches!(
                reader
                    .request(Request::Poll {
                        session: info.id.clone(),
                        after: 0
                    })
                    .await
                    .unwrap(),
                Response::Page(_)
            ));
            let request = orbit_protocol::Request::Files {
                session: info.id.clone(),
                expected_workspace: Some(info.workspace.clone()),
                action: orbit_protocol::files::Action::List {
                    path: ".".into(),
                    offset: 0,
                    include_hidden: true,
                },
            };
            let response = phone
                .request(Request::Action {
                    request: Box::new(request),
                })
                .await
                .unwrap();
            assert!(matches!(response, Response::Files(_)), "{response:?}");
            let invalid = phone
                .request(Request::Action {
                    request: Box::new(orbit_protocol::Request::Files {
                        session: info.id.clone(),
                        expected_workspace: Some(info.workspace.clone()),
                        action: orbit_protocol::files::Action::List {
                            path: String::new(),
                            offset: 0,
                            include_hidden: true,
                        },
                    }),
                })
                .await
                .unwrap();
            assert!(matches!(invalid, Response::Error(_)), "{invalid:?}");
            assert!(matches!(
                phone
                    .request(Request::Sessions)
                    .await
                    .unwrap(),
                Response::Sessions(_)
            ));

            if project == &second {
                let file = phone
                    .request(Request::Action {
                        request: Box::new(orbit_protocol::Request::ReadFile {
                            session: info.id.clone(),
                            path: "sample.txt".into(),
                            allow_missing: false,
                        }),
                    })
                    .await
                    .unwrap();
                assert!(
                    matches!(&file, Response::File(text) if text == "Z2VuZXJhdGVkIHNhbXBsZQ=="),
                    "{file:?}"
                );
            }
            assert!(matches!(
                phone
                    .request(Request::Action {
                        request: Box::new(orbit_protocol::Request::Shutdown)
                    })
                    .await
                    .unwrap(),
                Response::Error(_)
            ));
        }
        gateway.kill().await.unwrap();
    })
    .await
    .unwrap();
}
