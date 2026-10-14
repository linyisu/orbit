use super::*;
#[test]
fn unicode_length_and_whitespace_match_schema_without_destroying_operators() {
    let mut args = SearchArguments {
        query: "中".repeat(512),
        max_results: 10,
        ..Default::default()
    };

    normalize_arguments(&mut args).unwrap();
    args.query.push('中');
    assert!(normalize_arguments(&mut args).is_err());
    args.query = "  site:github.com  \"mouse   click\"\nOR C++  ".into();
    normalize_arguments(&mut args).unwrap();
    assert_eq!(args.query, "site:github.com \"mouse click\" OR C++");
    args.language = Some("EN-us".into());
    args.region = Some("gb".into());
    normalize_arguments(&mut args).unwrap();
    assert_eq!(args.language.as_deref(), Some("en-us"));
    args.language = Some("en\r\nInjected: x".into());
    assert!(normalize_arguments(&mut args).is_err());
}

#[test]
fn duckduckgo_preserves_query_and_applies_locale_and_freshness() {
    let args = SearchArguments {
        query: "rust \"async io\"".into(),
        max_results: 10,
        language: Some("en".into()),
        region: Some("GB".into()),
        freshness: Some(Freshness::Week),
    };

    let url = duckduckgo::url(&args, SearchProvider::Duckduckgo.endpoint()).unwrap();
    let query: HashMap<_, _> = url.query_pairs().collect();
    assert_eq!(query["q"], args.query);
    assert_eq!(query["kl"], "uk-en");
    assert_eq!(query["df"], "w");
}

#[test]
fn retries_only_transient_status_and_understands_retry_after_dates() {
    let mut headers = header::HeaderMap::new();
    headers.insert(header::RETRY_AFTER, header::HeaderValue::from_static("3"));
    for code in [400, 401, 403, 404] {
        assert!(
            status_failure(reqwest::StatusCode::from_u16(code).unwrap(), &headers)
                .retry
                .is_none()
        );
    }

    for code in [429, 500, 503] {
        assert_eq!(
            status_failure(reqwest::StatusCode::from_u16(code).unwrap(), &headers).retry,
            Some(Duration::from_secs(3))
        );
    }

    let date = httpdate::fmt_http_date(SystemTime::now() + Duration::from_secs(60));
    assert!(
        retry_after(Some(&header::HeaderValue::from_str(&date).unwrap())).unwrap()
            > Duration::from_secs(58)
    );
}

#[tokio::test]
async fn rate_limit_falls_back_and_next_call_skips_cooling_provider() {
    use tokio::io::AsyncWriteExt;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap();
    let proxy = reqwest::Proxy::all(format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let server = tokio::spawn(async move {
        for index in 0..3 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (req, _) = test_support::read_request(&mut stream).await;
            if index == 0 {
                assert!(req.contains("parallel.invalid"));
                stream
                    .write_all(
                        b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: \
                    120\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await
                    .unwrap();
            } else {
                assert!(req.contains("ddg.invalid"));
                let body = "<a class='result__a' href='https://example.org/'>Example</a>";
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: \
                    close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await
                    .unwrap();
            }
        }
    });
    let cooldown = Mutex::default();
    let args = SearchArguments {
        query: "example".into(),
        max_results: 8,
        ..Default::default()
    };

    for i in 0..2 {
        let result = route(
            args.clone(),
            &WebClient::for_test(Some(&proxy)),
            &[
                (SearchProvider::Parallel, "http://parallel.invalid/"),
                (SearchProvider::Duckduckgo, "http://ddg.invalid/"),
            ],
            &cooldown,
        )
        .await
        .unwrap();
        let result: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert!(
            !result["fallback_reason"]
                .as_str()
                .unwrap()
                .contains("tool execution failed")
        );
        assert!(
            result["fewer_results"]
                .as_bool()
                .unwrap()
        );
        assert_eq!(result["results"][0]["snippet_empty"], true);
        assert!(
            result["fallback_reason"]
                .as_str()
                .unwrap()
                .contains(if i == 0 { "429" } else { "cooldown" })
        );
    }

    server.await.unwrap();
}

#[tokio::test]
async fn forbidden_is_not_replayed_and_transient_server_error_is_retried() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for code in [403, 503] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap();
        let proxy =
            reqwest::Proxy::all(format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let server = tokio::spawn(async move {
            let count = if code == 403 { 1 } else { 2 };
            for _ in 0..count {
                let (mut s, _) = listener.accept().await.unwrap();
                let mut bytes = [0; 4_096];
                assert!(s.read(&mut bytes).await.unwrap() > 0);
                s.write_all(
                    format!(
                        "HTTP/1.1 {code} Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            }

            assert!(
                tokio::time::timeout(Duration::from_millis(100), listener.accept())
                    .await
                    .is_err()
            );
        });
        let result = route(
            SearchArguments {
                query: "example".into(),
                max_results: 1,
                ..Default::default()
            },
            &WebClient::for_test(Some(&proxy)),
            &[(SearchProvider::Duckduckgo, "http://ddg.invalid/")],
            &Mutex::default(),
        )
        .await;

        assert!(result.is_err());
        server.await.unwrap();
    }
}
