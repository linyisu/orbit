use super::*;

#[test]
fn executable_identity_uses_content_and_keeps_equal_copies_compatible() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first");
    let second = root.path().join("second");
    fs::write(&first, b"build-a").unwrap();
    fs::write(&second, b"build-a").unwrap();
    assert!(compare_executables(&first, &first).unwrap());
    assert!(compare_executables(&first, &second).unwrap());
    fs::write(&second, b"build-b").unwrap();
    assert!(!compare_executables(&first, &second).unwrap());
    fs::write(&second, b"longer-build").unwrap();
    assert!(!compare_executables(&first, &second).unwrap());
}

#[tokio::test]
async fn equal_build_clients_reuse_the_same_database_endpoint() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("service.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let args = ServiceOptions {
        workspace: root.path().to_string_lossy().into_owned(),
        daemon: false,
        allow_mcp_start: false,
    };
    // The peer is this test binary. Reuse must not send an upgrade request,
    // start a daemon, or create a separate endpoint for a second terminal.
    for _ in 0..2 {
        let stream = connect_or_start(
            &socket,
            &args,
            &root.path().join("config"),
            &root.path().join("db"),
            &root.path().join("auth"),
        )
        .await
        .unwrap();
        let (mut peer, _) = listener.accept().await.unwrap();
        assert!(same_executable(&stream).await.unwrap());
        use tokio::io::AsyncReadExt;
        let mut byte = [0];
        assert!(
            tokio::time::timeout(Duration::from_millis(50), peer.read(&mut byte))
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn service_handover_respects_busy_refusal_and_uses_orderly_shutdown() {
    for (generation, busy) in [(0, false), (0, true), (u64::MAX, false)] {
        let root = tempfile::tempdir().unwrap();
        let args = ServiceOptions {
            workspace: root.path().to_string_lossy().into_owned(),
            daemon: false,
            allow_mcp_start: false,
        };
        let (mut proxy, mut daemon) = UnixStream::pair().unwrap();
        let fake = tokio::spawn(async move {
            let context: ConnectionContext = protocol::read(&mut daemon).await.unwrap();
            assert!(!context.allow_mcp_start);
            assert!(matches!(
                protocol::read::<Request>(&mut daemon).await.unwrap(),
                Request::Hello {
                    version: protocol::VERSION,
                    ..
                }
            ));
            protocol::write(
                &mut daemon,
                &Response::Hello {
                    version: protocol::VERSION,
                    build_generation: generation,
                    workspace: context.workspace.to_string_lossy().into_owned(),
                    capabilities: orbit_protocol::Capabilities {
                        system: "Linux".into(),
                        architecture: "x86_64".into(),
                        shell: "sh".into(),
                        native_processes: true,
                        files: true,
                        sandbox: orbit_protocol::Availability::Unavailable("test".into()),
                        features: vec![],
                    },
                },
            )
            .await
            .unwrap();
            if generation == u64::MAX {
                // An older client closes its probe and reconnects to the
                // active newer service without sending a Shutdown request.
                use tokio::io::AsyncReadExt;
                assert_eq!(daemon.read(&mut [0]).await.unwrap(), 0);
                return;
            }
            assert!(matches!(
                protocol::read::<Request>(&mut daemon).await.unwrap(),
                Request::Shutdown
            ));
            let response = if busy {
                Response::Error("workspace has active tasks".into())
            } else {
                Response::Ok
            };
            protocol::write(&mut daemon, &response).await.unwrap();
        });
        let outcome = retire(
            &mut proxy,
            &args,
            &root.path().join("config"),
            &root.path().join("auth"),
        )
        .await;
        if busy {
            let message = outcome.unwrap_err().to_string();
            assert!(message.contains("active tasks"));
            assert!(message.contains("自动重连"));
        } else {
            assert_eq!(outcome.unwrap(), generation == 0);
        }
        drop(proxy);
        fake.await.unwrap();
    }
}

#[tokio::test]
async fn startup_lock_serializes_concurrent_handover_and_releases_on_cancellation() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("startup");
    let owner = private_file(&path).unwrap();
    acquire(&owner).await.unwrap();
    let contender = private_file(&path).unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(75), acquire(&contender))
            .await
            .is_err()
    );
    drop(owner);
    tokio::time::timeout(Duration::from_secs(1), acquire(&contender))
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn legacy_protocol_endpoints_retire_before_connecting_to_shared_service() {
    for version in [7, 8] {
        for busy in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let database = root.path().join("db");
            let socket = endpoints::shared(root.path(), &database).unwrap();
            let listener = UnixListener::bind(&socket).unwrap();
            let old_socket = endpoints::legacy(root.path(), &database)
                .unwrap()
                .into_iter()
                .find(|(v, _)| *v == version)
                .unwrap()
                .1;
            assert_ne!(socket, old_socket);
            let old_listener = UnixListener::bind(&old_socket).unwrap();
            let old_lock = private_file(&old_socket.with_extension("lock")).unwrap();
            old_lock.try_lock().unwrap();
            let daemon = tokio::spawn(async move {
                let (mut stream, _) = old_listener.accept().await.unwrap();
                let _: ConnectionContext = protocol::read(&mut stream).await.unwrap();
                assert!(
                    matches!(protocol::read::<Request>(&mut stream).await.unwrap(),
                    Request::Hello { version: received, .. } if received == version)
                );
                protocol::write(
                    &mut stream,
                    &Response::Hello {
                        version,
                        build_generation: 0,
                        workspace: "/project".into(),
                        capabilities: orbit_protocol::Capabilities {
                            system: "Linux".into(),
                            architecture: "x86_64".into(),
                            shell: "sh".into(),
                            native_processes: true,
                            files: true,
                            sandbox: orbit_protocol::Availability::Unavailable("test".into()),
                            features: vec![],
                        },
                    },
                )
                .await
                .unwrap();
                assert!(matches!(
                    protocol::read::<Request>(&mut stream).await.unwrap(),
                    Request::Shutdown
                ));
                protocol::write(
                    &mut stream,
                    &if busy {
                        Response::Error("workspace has active tasks".into())
                    } else {
                        Response::Ok
                    },
                )
                .await
                .unwrap();
                if !busy {
                    drop(old_listener);
                    fs::remove_file(old_socket).unwrap();
                }
                drop(old_lock);
            });
            let result = connect_or_start(
                &socket,
                &ServiceOptions {
                    workspace: root.path().to_string_lossy().into_owned(),
                    daemon: false,
                    allow_mcp_start: false,
                },
                &root.path().join("config"),
                &database,
                &root.path().join("auth"),
            )
            .await;
            if busy {
                assert!(result.unwrap_err().to_string().contains("active tasks"));
                assert!(
                    tokio::time::timeout(Duration::from_millis(30), listener.accept())
                        .await
                        .is_err()
                );
            } else {
                let connected = result.unwrap();
                let _accepted = listener.accept().await.unwrap();
                assert!(same_executable(&connected).await.unwrap());
            }
            daemon.await.unwrap();
        }
    }
}

#[tokio::test]
async fn legacy_aliases_share_one_listener_and_block_competing_daemons() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("db");
    let socket = endpoints::shared(root.path(), &database).unwrap();
    let listener = UnixListener::bind(&socket).unwrap();
    let locks = endpoints::lock_legacy(root.path(), &database).unwrap();
    endpoints::publish_aliases(root.path(), &database, &socket).unwrap();
    // Historical addresses still route to the owner; Hello negotiates schema separately.
    for (_, alias) in endpoints::legacy(root.path(), &database).unwrap() {
        assert_eq!(fs::read_link(&alias).unwrap(), socket);
        let competing = private_file(&alias.with_extension("lock")).unwrap();
        assert!(matches!(
            competing.try_lock(),
            Err(std::fs::TryLockError::WouldBlock)
        ));
        let client = UnixStream::connect(alias).await.unwrap();
        let _accepted = listener.accept().await.unwrap();
        assert!(same_executable(&client).await.unwrap());
    }
    assert_ne!(
        socket,
        endpoints::shared(root.path(), &root.path().join("other-db")).unwrap()
    );
    drop(locks);
    endpoints::lock_legacy(root.path(), &database).unwrap();
}

#[tokio::test]
async fn shared_server_negotiates_released_clients_and_rejects_unknown_versions() {
    for (version, compatible) in [
        (7, false),
        (8, false),
        (9, false),
        (orbit_protocol::VERSION, true),
        (orbit_protocol::VERSION + 1, false),
    ] {
        let root = tempfile::tempdir().unwrap();
        let options = crate::WorkspaceOptions {
            model: None,
            initial_directory: root.path().into(),
            auth_directory: root.path().join("auth"),
            config: root.path().join("config"),
            database: root.path().join("db"),
            allow_mcp_start: false,
            home: None,
            config_home: None,
            instruction_dir: None,
            editor_command: "vi".into(),
        };
        let service = Service {
            options: options.clone(),
            sessions: Mutex::new(HashMap::new()),
            opening: Mutex::new(()),
            last_activity: Mutex::new(tokio::time::Instant::now()),
            stop: tokio_util::sync::CancellationToken::new(),
            closing: AtomicBool::new(false),
        };
        let (mut client, server) = tokio::io::duplex(8192);
        let worker = tokio::spawn(async move {
            let (reader, writer) = tokio::io::split(server);
            service.client(reader, writer).await.unwrap();
        });
        protocol::write(
            &mut client,
            &ConnectionContext {
                config: options.config,
                auth: options.auth_directory,
                allow_mcp_start: false,
                workspace: options.initial_directory,
            },
        )
        .await
        .unwrap();
        protocol::write(
            &mut client,
            &Request::Hello {
                version,
                workspace: None,
            },
        )
        .await
        .unwrap();
        let response: Response = protocol::read(&mut client).await.unwrap();
        if compatible {
            assert!(
                matches!(response, Response::Hello { version: negotiated, .. } if negotiated == version)
            );
            protocol::write(&mut client, &Request::Shutdown)
                .await
                .unwrap();
            assert!(matches!(
                protocol::read::<Response>(&mut client).await.unwrap(),
                Response::Ok
            ));
        } else {
            assert!(
                matches!(response, Response::Error(error) if error.contains("protocol mismatch"))
            );
        }
        worker.await.unwrap();
    }
}
