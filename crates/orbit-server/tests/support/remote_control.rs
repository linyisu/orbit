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
        let start = || Request::Start {
            session: session.clone(),
            request_id: "remote-generated-request".into(),
            prompt: "Create the generated fixture file".into(),
        };
        assert!(matches!(
            phone.request(start()).await.unwrap(),
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
