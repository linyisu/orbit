//! Exercise HTML/hosted provider failover against deterministic HTTP responses.
use super::*;
use tokio::io::AsyncWriteExt;

async fn search(query: &str, html: &str, title: &str, url: &str) -> serde_json::Value {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap();
    let proxy = reqwest::Proxy::all(format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let query_copy = query.to_owned();
    let hosted = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "structuredContent": {
                "results": [{
                    "title": title,
                    "url": url,
                    "excerpts": []
                }]
            }
        }
    })
    .to_string();
    let bodies = [html.to_owned(), hosted];
    let server = tokio::spawn(async move {
        for (index, body) in bodies.into_iter().enumerate() {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (headers, payload) = test_support::read_request(&mut stream).await;
            let url = Url::parse(
                headers
                    .split_whitespace()
                    .nth(1)
                    .unwrap(),
            )
            .unwrap();
            if index == 0 {
                assert!(headers.starts_with("GET "));
                assert_eq!(url.host_str(), Some("ddg.invalid"));
                assert_eq!(
                    url.query_pairs()
                        .find(|(key, _)| key == "q")
                        .unwrap()
                        .1,
                    query_copy
                );
            } else {
                assert!(headers.starts_with("POST "));
                assert_eq!(url.host_str(), Some("parallel.invalid"));
                let payload: serde_json::Value = serde_json::from_slice(&payload).unwrap();
                assert_eq!(
                    payload["params"]["arguments"]["search_queries"],
                    json!([query_copy])
                );
            }
            let content_type = if index == 0 {
                "text/html; charset=utf-8"
            } else {
                "application/json"
            };
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: \
                {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
        }
    });
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        route(
            SearchArguments {
                query: query.into(),
                max_results: 8,
                ..Default::default()
            },
            &WebClient::for_test(Some(&proxy)),
            &[
                (SearchProvider::Duckduckgo, "http://ddg.invalid/html/"),
                (SearchProvider::Parallel, "http://parallel.invalid/mcp"),
            ],
            &Mutex::default(),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    server.await.unwrap();
    serde_json::from_str(&result).unwrap()
}

#[tokio::test]
async fn mismatched_query_offsite_results_and_weak_snippets_try_the_next_provider() {
    for (query, bad, reason) in [
        (
            "engine specification",
            "<a class='result__a' href='https://unrelated.example/'>Unrelated page</a>",
            "No query term",
        ),
        (
            "Example Engine Alpha",
            "<input name='q' value='Example'><a class='result__a' \
                href='https://example.org/'>Unrelated page</a>",
            "different query",
        ),
        (
            "site:example.org Alpha",
            "<a class='result__a' href='https://other.example/'>Alpha</a>",
            "ignored site:",
        ),
        (
            "\"Example Engine\" \"Alpha\"",
            "<a class='result__a' href='https://example.org/'>Unrelated page</a>",
            "Relevance is unconfirmed",
        ),
        (
            "engine specification",
            "<html>Please complete the captcha</html>",
            "no readable search results",
        ),
    ] {
        let result = search(
            query,
            bad,
            "Example Engine Alpha specification",
            "https://example.org/engine/specification",
        )
        .await;
        assert_eq!(result["provider"], "Parallel");
        assert_eq!(result["query"], query);
        assert_eq!(result["query_match_unconfirmed"], false);
        assert_eq!(
            result["results"][0]["url"],
            "https://example.org/engine/specification"
        );
        assert!(
            result["fallback_reason"]
                .as_str()
                .unwrap()
                .contains(reason)
        );
        for field in ["request_url", "response_url"] {
            assert_eq!(result[field], "http://parallel.invalid/mcp");
        }
    }
}

#[tokio::test]
async fn weak_results_are_retained_with_a_warning_if_alternatives_are_also_weak() {
    let result = search(
        "\"Example Engine\" \"Alpha\"",
        "<a class='result__a' href='https://example.org/'>Unrelated page</a>",
        "Example Notes",
        "https://other.example/",
    )
    .await;
    assert_eq!(result["provider"], "DuckDuckGo");
    assert_eq!(result["query_match_unconfirmed"], true);
    assert_eq!(
        result["results"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(
        result["warnings"][0]
            .as_str()
            .unwrap()
            .contains("Relevance is unconfirmed")
    );
    assert!(
        result["fallback_reason"]
            .as_str()
            .unwrap()
            .contains("Parallel")
    );
}
