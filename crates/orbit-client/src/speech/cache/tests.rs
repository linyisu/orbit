use super::*;
use tokio::net::TcpListener;

struct Reply {
    status: &'static str,
    range: Option<String>,
    length: usize,
    body: Vec<u8>,
}

async fn serve(reply: Reply) -> (String, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let request = read_request(&mut socket).await;
        let mut header = format!(
            "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n",
            reply.status, reply.length
        );
        if let Some(range) = reply.range {
            header.push_str(&format!("Content-Range: {range}\r\n"));
        }
        header.push_str("\r\n");
        let _ = socket
            .write_all(header.as_bytes())
            .await;
        let _ = socket.write_all(&reply.body).await;
        request
    });
    (url, task)
}

async fn read_request(socket: &mut tokio::net::TcpStream) -> String {
    let mut request = Vec::new();
    loop {
        let mut bytes = [0; 1_024];
        let n = socket.read(&mut bytes).await.unwrap();
        assert_ne!(n, 0);
        request.extend_from_slice(&bytes[..n]);
        assert!(request.len() < 16_384);
        if request
            .windows(4)
            .any(|bytes| bytes == b"\r\n\r\n")
        {
            break;
        }
    }
    String::from_utf8(request)
        .unwrap()
        .to_ascii_lowercase()
}

fn asset<'a>(source: &'a str, bytes: &[u8]) -> Asset<'a> {
    Asset {
        directory: "sample",
        name: "model.bin",
        source,
        bytes: bytes.len() as u64,
        sha256: "c94ac58e816e82992a8598f141d7402a031778001d60fe987a5c438c3d874dc0",
    }
}

const PAYLOAD: &[u8] = b"independently generated speech model payload";

fn file(root: &Path) -> std::path::PathBuf {
    root.join("sample/model.bin")
}

fn pending(root: &Path) -> std::path::PathBuf {
    let asset = asset("unused", PAYLOAD);
    root.join(format!("sample/model.bin.{}.part", asset.sha256))
}

async fn run(root: &Path, source: &str, cancel: &CancellationToken) -> Result<()> {
    let (progress, _) = watch::channel(Progress::Waiting);
    prepare(root, &[asset(source, PAYLOAD)], cancel, &progress).await
}

#[tokio::test]
async fn interrupted_download_resumes_from_persisted_bytes_and_installs_atomically() {
    let root = tempfile::tempdir().unwrap();
    tokio::fs::create_dir_all(root.path().join("sample"))
        .await
        .unwrap();
    tokio::fs::write(file(root.path()), b"previous cache")
        .await
        .unwrap();
    let cancel = CancellationToken::new();
    let (url, server) = serve(Reply {
        status: "200 OK",
        range: None,
        length: PAYLOAD.len(),
        body: PAYLOAD[..7].to_vec(),
    })
    .await;
    assert!(
        run(root.path(), &url, &cancel)
            .await
            .is_err()
    );
    let request = server.await.unwrap();
    assert!(request.contains("range: bytes=0-"));
    assert!(request.contains(concat!("user-agent: orbit/", env!("CARGO_PKG_VERSION"))));
    assert_eq!(
        tokio::fs::read(pending(root.path()))
            .await
            .unwrap(),
        &PAYLOAD[..7]
    );
    assert_eq!(
        tokio::fs::read(file(root.path()))
            .await
            .unwrap(),
        b"previous cache"
    );
    let (url, server) = serve(Reply {
        status: "206 Partial Content",
        range: Some(format!("bytes 7-{}/{}", PAYLOAD.len() - 1, PAYLOAD.len())),
        length: PAYLOAD.len() - 7,
        body: PAYLOAD[7..].to_vec(),
    })
    .await;
    run(root.path(), &url, &cancel)
        .await
        .unwrap();
    assert!(
        server
            .await
            .unwrap()
            .contains("range: bytes=7-")
    );
    assert_eq!(
        tokio::fs::read(file(root.path()))
            .await
            .unwrap(),
        PAYLOAD
    );
    assert!(!pending(root.path()).exists());
    // A complete verified cache must work even when the download source is unreachable.
    run(root.path(), "http://127.0.0.1:9", &cancel)
        .await
        .unwrap();
}

#[tokio::test]
async fn cancellation_retains_a_download_and_stops_waiting_for_a_stalled_server() {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let cancel = CancellationToken::new();
    let server_cancel = cancel.clone();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        read_request(&mut socket).await;
        socket
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n",
                    PAYLOAD.len()
                )
                .as_bytes(),
            )
            .await
            .unwrap();
        socket
            .write_all(&PAYLOAD[..7])
            .await
            .unwrap();
        server_cancel.cancelled().await;
    });
    let (progress, mut updates) = watch::channel(Progress::Waiting);
    let cancel_on_progress = async {
        loop {
            let update = *updates.borrow_and_update();
            if let Progress::Downloading { received_bytes, .. } = update
                && received_bytes >= 7
            {
                cancel.cancel();
                break;
            }
            updates.changed().await.unwrap();
        }
    };
    let assets = [asset(&url, PAYLOAD)];
    let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
        tokio::join!(
            prepare(root.path(), &assets, &cancel, &progress),
            cancel_on_progress
        )
    })
    .await
    .unwrap();
    assert!(result.is_err());
    server.await.unwrap();
    assert_eq!(
        tokio::fs::read(pending(root.path()))
            .await
            .unwrap(),
        &PAYLOAD[..7]
    );
    assert!(!file(root.path()).exists());
}

#[tokio::test]
async fn ignored_ranges_restart_safely_and_bad_ranges_never_append() {
    let root = tempfile::tempdir().unwrap();
    tokio::fs::create_dir_all(root.path().join("sample"))
        .await
        .unwrap();
    tokio::fs::write(pending(root.path()), &PAYLOAD[..7])
        .await
        .unwrap();
    let cancel = CancellationToken::new();
    for range in ["bytes 0-1/2", "bytes 7-9/*", "bytes 8-9/43", "invalid"] {
        let (url, server) = serve(Reply {
            status: "206 Partial Content",
            range: Some(range.into()),
            length: 2,
            body: vec![0; 2],
        })
        .await;
        assert!(
            run(root.path(), &url, &cancel)
                .await
                .is_err()
        );
        server.await.unwrap();
        assert_eq!(
            tokio::fs::read(pending(root.path()))
                .await
                .unwrap(),
            &PAYLOAD[..7]
        );
    }
    let (url, server) = serve(Reply {
        status: "200 OK",
        range: None,
        length: 2,
        body: vec![0; 2],
    })
    .await;
    assert!(
        run(root.path(), &url, &cancel)
            .await
            .is_err()
    );
    server.await.unwrap();
    assert_eq!(
        tokio::fs::read(pending(root.path()))
            .await
            .unwrap(),
        &PAYLOAD[..7]
    );
    let (url, server) = serve(Reply {
        status: "200 OK",
        range: None,
        length: PAYLOAD.len(),
        body: PAYLOAD.to_vec(),
    })
    .await;
    run(root.path(), &url, &cancel)
        .await
        .unwrap();
    assert!(
        server
            .await
            .unwrap()
            .contains("range: bytes=7-")
    );
    assert_eq!(
        tokio::fs::read(file(root.path()))
            .await
            .unwrap(),
        PAYLOAD
    );
}

#[tokio::test]
async fn large_downloads_advance_ranges_and_report_complete_byte_counts() {
    let root = tempfile::tempdir().unwrap();
    let bytes = vec![0x5a; DOWNLOAD_RANGE_BYTES as usize + 17];
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let server_bytes = bytes.clone();
    let server = tokio::spawn(async move {
        for (start, end) in [
            (0, DOWNLOAD_RANGE_BYTES - 1),
            (DOWNLOAD_RANGE_BYTES, DOWNLOAD_RANGE_BYTES + 16),
        ] {
            let (mut socket, _) = listener.accept().await.unwrap();
            let request = read_request(&mut socket).await;
            assert!(request.contains(&format!("range: bytes={start}-{end}")));
            let header = format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\n\
                 Content-Range: bytes {start}-{end}/{}\r\nConnection: close\r\n\r\n",
                end - start + 1,
                server_bytes.len()
            );
            socket
                .write_all(header.as_bytes())
                .await
                .unwrap();
            socket
                .write_all(&server_bytes[start as usize..=end as usize])
                .await
                .unwrap();
        }
    });
    let (progress, updates) = watch::channel(Progress::Waiting);
    prepare(
        root.path(),
        &[Asset {
            directory: "sample",
            name: "model.bin",
            source: &url,
            bytes: bytes.len() as u64,
            sha256: "049c48c7c56f0a13fba12e64f711a10ff9f29f4573e30789c6e94b3e30dd5473",
        }],
        &CancellationToken::new(),
        &progress,
    )
    .await
    .unwrap();
    server.await.unwrap();
    assert_eq!(
        tokio::fs::read(file(root.path()))
            .await
            .unwrap(),
        bytes
    );
    assert_eq!(
        *updates.borrow(),
        Progress::Verifying {
            completed_bytes: bytes.len() as u64,
            total_bytes: bytes.len() as u64
        }
    );
}

#[tokio::test]
async fn corrupt_complete_downloads_are_discarded_without_replacing_the_previous_cache() {
    let root = tempfile::tempdir().unwrap();
    tokio::fs::create_dir_all(root.path().join("sample"))
        .await
        .unwrap();
    tokio::fs::write(file(root.path()), b"previous cache")
        .await
        .unwrap();
    let cancel = CancellationToken::new();
    let (url, server) = serve(Reply {
        status: "200 OK",
        range: None,
        length: PAYLOAD.len(),
        body: vec![0; PAYLOAD.len()],
    })
    .await;
    assert!(
        run(root.path(), &url, &cancel)
            .await
            .is_err()
    );
    server.await.unwrap();
    assert!(!pending(root.path()).exists());
    assert_eq!(
        tokio::fs::read(file(root.path()))
            .await
            .unwrap(),
        b"previous cache"
    );
    tokio::fs::write(pending(root.path()), PAYLOAD)
        .await
        .unwrap();
    run(root.path(), "http://127.0.0.1:9", &cancel)
        .await
        .unwrap();
    assert_eq!(
        tokio::fs::read(file(root.path()))
            .await
            .unwrap(),
        PAYLOAD
    );
}

#[tokio::test]
async fn cancelled_lock_waiters_do_not_start_competing_downloads() {
    let root = tempfile::tempdir().unwrap();
    let first = lock(root.path(), &CancellationToken::new())
        .await
        .unwrap();
    let cancel = CancellationToken::new();
    let operation = lock(root.path(), &cancel);
    let interrupt = async {
        tokio::time::sleep(Duration::from_millis(20)).await;
        cancel.cancel();
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(operation, interrupt)
    })
    .await
    .unwrap();
    assert!(result.is_err());
    drop(first);
    lock(root.path(), &CancellationToken::new())
        .await
        .unwrap();
}
