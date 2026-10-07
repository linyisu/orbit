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
