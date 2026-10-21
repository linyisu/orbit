use nsh_core::{
    geometry::{px, size},
    ui::Root,
};
use nsh_engine::EngineConfig;
use nsh_testing::{Headless, MemoryStorage};
use orbit_client::remote::{Pairing, vault};
use orbit_protocol::remote::{self, Request};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio_rustls::{
    TlsAcceptor,
    rustls::{self, pki_types::PrivatePkcs8KeyDer},
};

#[test]
fn remote_sessions_tools_approval_questions_and_send_are_connected() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let requests = Arc::new(Mutex::new(Vec::<Request>::new()));
    let recorded = requests.clone();
    let interrupt_history = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (pairing, server) = runtime.block_on(async move {
        let cert = rcgen::generate_simple_self_signed(vec![remote::SERVER_NAME.into()]).unwrap();
        let config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert.cert.der().clone()],
            PrivatePkcs8KeyDer::from(cert.signing_key.serialize_der()).into(),
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let pairing = Pairing {
            host_name: "Sample computer".into(),
            version: remote::VERSION,
            address: listener
                .local_addr()
                .unwrap()
                .to_string(),
            certificate: cert.cert.der().to_vec(),
            device: "synthetic-device".into(),
            secret: "synthetic-secret".into(),
        };
        let server = tokio::spawn(async move {
            let acceptor = TlsAcceptor::from(Arc::new(config));
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                let acceptor = acceptor.clone();
                let recorded = recorded.clone();
                let interrupt_history = interrupt_history.clone();
                tokio::spawn(async move {
                    let mut stream = acceptor.accept(socket).await.unwrap();
                    let auth: remote::Authentication = remote::read(&mut stream).await.unwrap();
                    assert_eq!(auth.secret, "synthetic-secret");
                    remote::write(
                        &mut stream,
                        &remote::Welcome::Connected {
                            workspace: "/synthetic-project".into(),
                            permissions: remote::Permissions {
                                control: true,
                                approve: true,
                            },
                        },
                    )
                    .await
                    .unwrap();
                    while let Ok(request) = remote::read::<Request>(&mut stream).await {
                        let disconnect = matches!(&request, Request::Poll { .. })
                            && interrupt_history.swap(false, std::sync::atomic::Ordering::SeqCst);
                        let mut response = response(&request);
                        if matches!(request, Request::Sessions)
                            && std::env::var_os("ORBIT_MOBILE_NATIVE_STORAGE").is_some()
                        {
                            let rows = response["value"]
                                .as_array_mut()
                                .unwrap();
                            for index in 0..40 {
                                let mut row = rows[0].clone();
                                row["id"] = json!(format!("sample-{index}"));
                                row["title"] = json!(format!("Sample conversation {index:02}"));
                                row["workspace"] = json!(format!("/test-project-{:02}", index / 4));
                                rows.push(row);
                            }
                        }
                        recorded.lock().unwrap().push(request);
                        if disconnect {
                            break;
                        }
                        if remote::write(&mut stream, &response)
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                });
            }
        });
        (pairing, server)
    });
    let encoded = vault::encode_private(&vault::Contents {
        devices: vec![pairing],
        ..Default::default()
    })
    .unwrap();
    // Optional native acceptance uses the same synthetic server and persisted connection.
    if let Some(path) = std::env::var_os("ORBIT_MOBILE_NATIVE_STORAGE") {
        std::fs::write(
            path,
            serde_json::to_vec(&std::collections::BTreeMap::from([(
                "orbit.connections.v2",
                encoded.as_str(),
            )]))
            .unwrap(),
        )
        .unwrap();
        runtime.block_on(async { tokio::time::sleep(std::time::Duration::from_secs(600)).await });
        server.abort();
        return;
    }
    let storage = MemoryStorage::new([("orbit.connections.v2".into(), encoded)]);
    let mut config = EngineConfig::new(Root::of::<orbit_mobile::App>());
    config.viewport = size(px(390.0), px(844.0));
    config.ui_sets = vec![nsh_base::registry()];
    config.storage = Some(storage.clone());
    let mut app = Headless::start(config).unwrap();
    app.wait_for("text:连接你的电脑")
        .unwrap();
    assert!(!app.shows("连接保险箱"));
    app.wait_for("text:连接电脑").unwrap();
    app.press("text:连接电脑").unwrap();
    app.wait_for("label:工作区").unwrap();
    assert!(!app.shows("打开电脑上的项目"));
    app.press("label:工作区").unwrap();
    app.wait_for("label:/synthetic-project")
        .unwrap();
    app.press("label:/synthetic-project")
        .unwrap();
    app.wait_for("label:查看输出").unwrap();
    app.press("label:菜单").unwrap();
    app.press("label:设置").unwrap();
    app.wait_for("text:断开当前连接")
        .unwrap();
    app.back().unwrap();
    app.wait_for("label:查看输出").unwrap();
    app.press("label:菜单").unwrap();
    app.wait_for("text:Synthetic conversation")
        .unwrap();
    app.back().unwrap();
    app.wait_for("label:查看输出").unwrap();
    app.press("label:菜单").unwrap();
    app.press("label:关闭会话菜单").unwrap();
    app.wait_for("label:查看输出").unwrap();
    app.press("label:工作区").unwrap();
    app.press("text:文件").unwrap();
    app.wait_for("text:sample.txt").unwrap();
    app.press("text:上级目录").unwrap();
    app.wait_for("text:sample.txt").unwrap();
    app.press("text:sample.txt").unwrap();
    app.wait_for("text:Generated file content")
        .unwrap();
    app.back().unwrap();
    app.back().unwrap();
    app.press("label:思考过程").unwrap();
    app.wait_for("text:Synthetic reasoning content.")
        .unwrap();
    app.press("label:思考过程").unwrap();
    app.press("label:查看输出").unwrap();
    app.wait_for("text:复制输出").unwrap();
    app.back().unwrap();
    app.wait_for("text:允许本次").unwrap();
    app.press("text:允许本次").unwrap();
    app.wait_for("text:○ First option")
        .unwrap();
    app.press("text:○ First option")
        .unwrap();
    app.press("text:提交回答").unwrap();
    app.type_text("label:描述任务…", "Synthetic follow-up")
        .unwrap();
    app.press("label:发送").unwrap();
    app.wait_until(
        "submitted draft clears",
        std::time::Duration::from_secs(5),
        |screen| matches!(screen.input_text("label:描述任务…"), Ok(Some(""))),
    )
    .unwrap();
    app.type_text("label:描述任务…", "Rejected task")
        .unwrap();
    app.press("label:发送").unwrap();
    app.wait_for("text:Synthetic rejection")
        .unwrap();
    app.wait_for("text:Rejected task")
        .unwrap();
    let requests = requests.lock().unwrap();
    assert!(
        requests
            .iter()
            .filter_map(|r| match r {
                Request::Action { request } => match request.as_ref() {
                    orbit_protocol::Request::Files {
                        action: orbit_protocol::files::Action::List { path, .. },
                        ..
                    } => Some(path),
                    _ => None,
                },
                _ => None,
            })
            .all(|path| !path.is_empty())
    );
    assert!(
        requests
            .iter()
            .filter(|r| matches!(r, Request::Poll { .. }))
            .count()
            >= 2
    );
    assert!(requests.iter().any(
        |r| matches!(r, Request::Workspace { path, resume: None } if path == "/synthetic-project")
    ));
    assert!(requests.iter().any(|r| matches!(
        r,
        Request::Approve {
            approval: 7,
            allow: true,
            ..
        }
    )));
    assert!(requests.iter().any(
        |r| matches!(r, Request::AnswerUserInput {answers:Some(a), ..} if a[0].selected == ["one"])
    ));
    assert!(
        requests
            .iter()
            .any(|r| matches!(r, Request::Submit {prompt, ..} if prompt == "Synthetic follow-up"))
    );
    drop(requests);
    drop(app);
    let mut config = EngineConfig::new(Root::of::<orbit_mobile::App>());
    config.viewport = size(px(390.0), px(844.0));
    config.ui_sets = vec![nsh_base::registry()];
    config.storage = Some(storage);
    let mut reopened = Headless::start(config).unwrap();
    reopened
        .wait_for("label:查看输出")
        .unwrap();
    assert!(!reopened.shows("打开电脑上的项目"));
    server.abort();
}

fn response(request: &Request) -> Value {
    match request {
        Request::Action { request } => match request.as_ref() {
            orbit_protocol::Request::Files { .. } => json!({"type":"files", "value": {
                "kind":"directory", "path":".", "entries":[{"path":"sample.txt","kind":"file"}],
                "next_offset":null, "scan_truncated":false, "readonly":false
            }}),
            orbit_protocol::Request::ReadFile { .. } => {
                json!({"type":"file","value":base64::Engine::encode(&base64::engine::general_purpose::STANDARD, "Generated file content")})
            }
            _ => json!({"type":"ok"}),
        },
        Request::Submit { prompt, .. } if prompt == "Rejected task" => {
            json!({ "type": "error", "value": "Synthetic rejection" })
        }
        Request::Sessions => json!({
            "type": "sessions",
            "value": [{
                "id": "synthetic-session",
                "workspace": "/synthetic-project",
                "project_id": "synthetic-project",
                "model": "fixture",
                "reasoning_effort": null,
                "title": "Synthetic conversation",
                "updated_at": 0
            }]
        }),
        Request::Browse { .. } | Request::Workspace { .. } => json!({
            "type": "opened",
            "value": {
                "id": "synthetic-session",
                "title": "Synthetic conversation",
                "workspace": "/synthetic-project",
                "model": "fixture",
                "models": [],
                "supports_vision": false,
                "reasoning_effort": null,
                "reasoning_efforts": [],
                "context_tokens": 0,
                "input_context_tokens": 0,
                "max_output_tokens": 0
            }
        }),
        Request::Poll { after, .. } => {
            let updates = if *after == 0 {
                vec![
                    json!({ "Started": { "prompt": "Synthetic task" } }),
                    json!({ "Agent": { "Model": { "ReasoningDelta": "Synthetic reasoning content." } } }),
                    json!({ "Agent": { "Model": { "TextDelta": "A **rendered** response." } } }),
                    json!({
                        "Agent": {
                            "ToolFinished": {
                                "call": {
                                    "id": "synthetic-call",
                                    "function": { "name": "Inspect", "arguments": "{}" }
                                },
                                "content": "Synthetic output"
                            }
                        }
                    }),
                ]
            } else {
                vec![]
            };
            json!({
                "type": "page",
                "value": {
                    "next": 3,
                    "more": false,
                    "busy": false,
                    "model": "fixture",
                    "updates": updates,
                    "approvals": [{
                        "id": 7,
                        "preview": {
                            "Command": {
                                "command": "synthetic-command",
                                "cwd": "/synthetic-project"
                            }
                        }
                    }],
                    "user_inputs": [{
                        "id": "synthetic-question",
                        "call_id": "question-call",
                        "source": null,
                        "status": "pending",
                        "answers": [],
                        "form": {
                            "questions": [{
                                "id": "choice",
                                "title": "Choose",
                                "prompt": "Choose a synthetic option",
                                "kind": "single",
                                "allow_custom": true,
                                "required": true,
                                "options": [
                                    { "id": "one", "label": "First option", "description": "" },
                                    { "id": "two", "label": "Second option", "description": "" }
                                ]
                            }]
                        }
                    }]
                }
            })
        }
        _ => json!({ "type": "ok" }),
    }
}
