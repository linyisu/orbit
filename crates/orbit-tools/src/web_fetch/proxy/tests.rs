use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

fn automatic(matcher: Matcher) -> WebClient {
    WebClient {
        sandbox: false,
        explicit: None,
        automatic: Arc::new(matcher),
    }
}

fn proxy_port(client: &WebClient, value: &str) -> Option<u16> {
    client
        .route(&Url::parse(value).unwrap())
        .unwrap()
        .and_then(|route| route.browser_proxy)
        .and_then(|url| url.port())
}

#[test]
fn environment_routes_are_inherited_without_a_web_section() {
    const CHILD: &str = "ORBIT_PROXY_TEST_CHILD";
    if let Ok(mode) = std::env::var(CHILD) {
        let client = WebClient::new(&WebConfig::default()).unwrap();
        let (http, https) = if mode == "all" {
            (8003, 8003)
        } else {
            (8001, 8002)
        };

        assert_eq!(proxy_port(&client, "http://public.invalid/"), Some(http));
        assert_eq!(proxy_port(&client, "https://public.invalid/"), Some(https));
        assert_eq!(proxy_port(&client, "https://sub.excluded.invalid/"), None);
        assert_eq!(proxy_port(&client, "http://203.0.113.4/"), None);
        return;
    }

    // Each child has its own environment: never mutate process-wide variables
    // while other HTTP or model tests are running on parallel threads.
    for mode in ["upper", "lower", "all"] {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "web_fetch::proxy::tests::environment_routes_are_inherited_without_a_web_section",
            ])
            .env_clear()
            .env(CHILD, mode);
        let key = |name: &str| {
            if mode == "lower" {
                name.to_ascii_lowercase()
            } else {
                name.into()
            }
        };

        command.env(key("ALL_PROXY"), "http://127.0.0.1:8003");
        command.env(key("NO_PROXY"), ".excluded.invalid,203.0.113.0/24");
        if mode != "all" {
            command.env(key("HTTP_PROXY"), "http://127.0.0.1:8001");
            command.env(key("HTTPS_PROXY"), "http://127.0.0.1:8002");
        }

        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "{mode}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
    }
}

#[test]
fn explicit_proxy_overrides_environment_and_no_proxy() {
    let mut client = automatic(
        Matcher::builder()
            .all("http://127.0.0.1:8001")
            .no("*")
            .build(),
    );

    client.explicit = WebClient::new(&WebConfig {
        proxy: Some("http://127.0.0.1:7897".into()),
        ..Default::default()
    })
    .unwrap()
    .explicit;
    for value in ["http://public.invalid/", "https://public.invalid/"] {
        assert_eq!(proxy_port(&client, value), Some(7897));
    }
}

#[test]
fn automatic_bypass_and_direct_fallback_use_the_transport_matcher() {
    let client = automatic(
        Matcher::builder()
            .all("http://127.0.0.1:8001")
            .no(".example.org,1.1.1.0/24,2606:4700:4700::1111")
            .build(),
    );

    for value in [
        "https://example.org/",
        "https://sub.example.org/",
        "http://1.1.1.1/",
        "https://[2606:4700:4700::1111]/",
    ] {
        assert!(
            client.route(&Url::parse(value).unwrap()).unwrap().is_none(),
            "{value}"
        );
    }

    assert_eq!(proxy_port(&client, "http://notexample.org/"), Some(8001));
    let direct = automatic(Matcher::builder().build());
    assert!(
        direct
            .route(&Url::parse("https://example.org/").unwrap())
            .unwrap()
            .is_none()
    );
}

#[test]
fn credentials_are_redacted_and_browser_fallback_requires_compatible_proxy() {
    for value in [
        "http://secret:password@127.0.0.1:8001",
        "https://127.0.0.1:8001",
        "socks5h://secret:password@127.0.0.1:8001",
    ] {
        let client = automatic(Matcher::builder().all(value).build());
        let route = client
            .route(&Url::parse("https://example.org/").unwrap())
            .unwrap()
            .unwrap();
        assert!(route.browser_proxy.is_none());
        let debug = format!("{client:?}");
        assert!(!debug.contains("secret") && !debug.contains("password"));
    }
}

#[tokio::test]
async fn automatic_proxy_serves_pages_search_and_images_without_local_dns() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let client = automatic(Matcher::builder().all(format!("http://{address}")).build());
    let server = tokio::spawn(async move {
        for path in ["page", "search", "image"] {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = [0; 4096];
            let count = stream.read(&mut bytes).await.unwrap();
            let request = String::from_utf8_lossy(&bytes[..count]);
            assert!(
                request.starts_with(&format!("GET http://proxy-only.invalid/{path}")),
                "{request}"
            );
            let (content_type, body) = match path {
                "page" => ("text/html", "<h1>Proxy page</h1>"),
                "search" => (
                    "text/html",
                    "<li class='b_algo'><h2><a href='https://example.org/'>Proxy search</a></h2></li>",
                ),

                _ => ("image/png", "image payload"),
            };

            stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
        }
    });
    let output = WebFetch::with_client(client.clone())
        .execute(
            ToolCall::function(
                "page",
                "web_fetch",
                json!({"url":"http://proxy-only.invalid/page"}),
            ),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert!(output.contains("Proxy page") && output.contains("trusted_proxy_resolved"));
    // Use a local endpoint with the production routing function rather than
    // sending a search to a real provider or resolving .invalid locally.
    let output = search::route_for_test(
        SearchArguments {
            query: "test".into(),
            ..Default::default()
        },
        &client,
        "http://proxy-only.invalid/search",
    )
    .await
    .unwrap();

    assert!(output.contains("Proxy search"));
    assert_eq!(
        client
            .fetch_image("http://proxy-only.invalid/image")
            .await
            .unwrap(),
        b"image payload"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn automatic_proxy_cannot_access_private_targets_or_private_redirects() {
    let client = automatic(Matcher::builder().all("http://127.0.0.1:9").build());
    for value in [
        "http://127.0.0.1/a",
        "http://198.18.0.40/a",
        "https://[::1]/a",
        "http://localhost/a",
        "https://metadata.internal/a",
    ] {
        let call = ToolCall::function("page", "web_fetch", json!({"url":value}));
        assert!(
            WebFetch::with_client(client.clone())
                .execute(call, CancellationToken::new())
                .await
                .is_err(),
            "{value}"
        );
        assert!(client.fetch_image(value).await.is_err(), "{value}");
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = automatic(
        Matcher::builder()
            .all(format!("http://{}", listener.local_addr().unwrap()))
            .build(),
    );
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = [0; 4096];
        assert!(stream.read(&mut request).await.unwrap() > 0);
        stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
    });
    let error = client
        .fetch_image("http://proxy-only.invalid/image")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("non-public"));
    server.await.unwrap();
}
