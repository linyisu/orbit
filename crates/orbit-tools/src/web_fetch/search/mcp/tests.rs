use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn envelope(result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": result
    })
}

fn parallel() -> Value {
    json!({
        "results": [
            {
                "url": "https://example.org/isa",
                "title": "Turing Complete RV32I",
                "excerpts": ["Custom ISA", "RISC-V implementation"]
            },
            {
                "url": "javascript:alert(1)",
                "title": "unsafe",
                "excerpts": []
            },
            {
                "url": "https://example.com/docs",
                "title": null,
                "excerpts": []
            }
        ],
        "warnings": [{
            "message": "Some sources unavailable"
        }]
    })
}

#[test]
fn defaults_and_serialized_configuration_choose_hosted_search_first() {
    let config: WebConfig = serde_json::from_value(json!({})).unwrap();
    assert_eq!(
        config.search_providers,
        vec![
            SearchProvider::Parallel,
            SearchProvider::Exa,
            SearchProvider::Duckduckgo
        ]
    );
    let explicit: WebConfig = serde_json::from_value(json!({
        "search_providers": ["exa", "parallel", "duckduckgo"]
    }))
    .unwrap();
    assert_eq!(
        explicit.search_providers,
        vec![
            SearchProvider::Exa,
            SearchProvider::Parallel,
            SearchProvider::Duckduckgo
        ]
    );
}

#[test]
fn retired_or_unknown_providers_report_supported_configuration_values() {
    for provider in ["google", "bing", "unknown"] {
        let error = serde_json::from_value::<WebConfig>(json!({
            "search_providers": [provider]
        }))
        .unwrap_err()
        .to_string();
        assert!(error.contains(provider));
        for supported in ["parallel", "exa", "duckduckgo"] {
            assert!(error.contains(supported), "{error}");
        }
    }
}

#[test]
fn parallel_accepts_structured_content_and_json_text_without_duplicate_results() {
    let structured = parallel();
    for result in [
        json!({
            "structuredContent": structured,
            "content": [{
                "type": "text",
                "text": structured.to_string()
            }]
        }),
        json!({
            "content": [{
                "type": "text",
                "text": structured.to_string()
            }]
        }),
    ] {
        let result = tool_result(envelope(result)).unwrap();
        let (results, warnings) = parse_results(&result, SearchProvider::Parallel).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].snippet, "Custom ISA RISC-V implementation");
        assert_eq!(results[1].title, "https://example.com/docs");
        assert!(results[1].snippet.is_empty());
        assert_eq!(warnings, ["Some sources unavailable"]);
    }
}

#[test]
fn exa_parses_multiple_text_blocks_and_multiline_highlights() {
    let result = json!({
        "content": [
            {
                "type": "text",
                "text": "Title: Turing Complete RV32I\nURL: https://example.org/isa\nPublished \
                    Date: 2026-01-01\nText: Custom ISA\nRISC-V\n\n---\n\nTitle: Compiler\nURL: \
                    https://example.com/compiler\nHighlights:\nAssembler docs"
            },
            {
                "type": "text",
                "text": "Title: unsafe\nURL: file:///etc/passwd\nText: ignore"
            },
            {
                "type": "text",
                "text": "Title: Third\nURL: https://other.example/\nText: Details"
            }
        ]
    });
    let (results, _) = parse_results(&result, SearchProvider::Exa).unwrap();
    assert_eq!(results.len(), 3);
    assert_eq!(results[0].snippet, "Custom ISA RISC-V");
    assert_eq!(results[1].title, "Compiler");
    assert_eq!(results[1].snippet, "Assembler docs");
    assert_eq!(results[2].title, "Third");
}

#[test]
fn sse_waits_for_complete_event_and_matches_id_with_multiline_data() {
    assert!(
        sse_envelope("event: message\ndata: {\"id\":1}")
            .unwrap()
            .is_none()
    );
    let body = "event: message\r\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\
        \r\n\r\ndata: {\"id\":2}\r\n\r\nevent: message\r\ndata: {\"jsonrpc\":\"2.0\",\r\ndata: \
        \"id\":1, \"result\":{\"content\":[]}}\r\n\r\n";
    let value = sse_envelope(body).unwrap().unwrap();
    assert!(tool_result(value).is_ok());
    assert!(sse_envelope("data: not json\n\n").is_err());
}

#[test]
fn quota_and_tool_errors_are_failures_even_with_http_success() {
    for result in [
        json!({
            "_meta": {
                "ai.exa/rateLimited": true
            },
            "content": [{
                "type": "text",
                "text": "Create an API key"
            }]
        }),
        json!({
            "isError": true,
            "content": [{
                "type": "text",
                "text": "Rate limit exceeded"
            }]
        }),
    ] {
        let failure = tool_result(envelope(result)).unwrap_err();
        assert!(failure.limited);
        assert_eq!(failure.retry, Some(Duration::from_secs(60)));
    }
    let failure = tool_result(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "error": {
            "code": 429,
            "message": "Limited"
        }
    }))
    .unwrap_err();
    assert!(failure.limited);
    let failure = tool_result(envelope(json!({
        "isError": true,
        "content": [{
            "type": "text",
            "text": "Invalid query"
        }]
    })))
    .unwrap_err();
    assert!(!failure.limited);
    assert!(
        tool_result(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {}
        }))
        .is_err()
    );
    assert!(
        tool_result(json!({
            "jsonrpc": "2.0",
            "id": 1
        }))
        .is_err()
    );
}

// Read the full POST body rather than just its headers, even when TCP splits it.
async fn read_request(stream: &mut tokio::net::TcpStream) -> (String, Value) {
    let (headers, body) = super::super::test_support::read_request(stream).await;
    (headers, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn http_200_exa_quota_falls_back_and_stays_in_cooldown() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap();
    let proxy = reqwest::Proxy::all(format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let query = "\"Turing Complete\" RV32I site:example.org";
    let server = tokio::spawn(async move {
        for index in 0..3 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let (headers, body) = read_request(&mut stream).await;
            assert!(headers.starts_with("POST "));
            let lower = headers.to_ascii_lowercase();
            assert!(!lower.contains("authorization:"));
            assert!(lower.contains("user-agent: orbit/"));
            let reply = if index == 0 {
                assert!(headers.contains("exa.invalid"));
                assert_eq!(body["params"]["name"], "web_search_exa");
                assert_eq!(body["params"]["arguments"]["query"], query);
                assert_eq!(body["params"]["arguments"]["numResults"], 2);
                envelope(json!({
                    "_meta": {
                        "ai.exa/rateLimited": true
                    }
                }))
            } else {
                assert!(headers.contains("parallel.invalid"));
                assert_eq!(body["params"]["name"], "web_search");
                assert_eq!(
                    body["params"]["arguments"]["search_queries"],
                    json!([query])
                );
                assert_eq!(body["params"]["arguments"]["objective"], query);
                assert!(
                    body["params"]["arguments"]
                        .get("model_name")
                        .is_none()
                );
                envelope(json!({
                    "content": [{
                        "type": "text",
                        "text": parallel().to_string()
                    }]
                }))
            };
            let response = format!("event: message\ndata: {reply}\n\n");
            // Chunked SSE deliberately remains open after the requested event.
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: \
                text/event-stream\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n{response}\r\n",
                        response.len()
                    )
                    .as_bytes(),
                )
                .await
                .unwrap();
            let mut byte = [0];
            let _ = tokio::time::timeout(Duration::from_secs(3), stream.read(&mut byte)).await;
        }
    });
    let cooldown = Mutex::default();
    for index in 0..2 {
        let result = tokio::time::timeout(
            Duration::from_secs(3),
            route(
                SearchArguments {
                    query: query.into(),
                    max_results: 2,
                    language: Some("en".into()),
                    ..Default::default()
                },
                &WebClient::for_test(Some(&proxy)),
                &[
                    (SearchProvider::Exa, "http://exa.invalid/mcp"),
                    (SearchProvider::Parallel, "http://parallel.invalid/mcp"),
                ],
                &cooldown,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        let value: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(value["provider"], "Parallel");
        assert_eq!(value["query"], query);
        assert_eq!(value["results_returned"], 1);
        assert_eq!(value["results"][0]["url"], "https://example.org/isa");
        let reason = value["fallback_reason"]
            .as_str()
            .unwrap();
        assert!(reason.contains(if index == 0 { "rate limit" } else { "cooldown" }));
        assert!(
            value["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| {
                    w.as_str()
                        .unwrap()
                        .contains("does not apply these filters: language")
                })
        );
    }
    server.await.unwrap();
}

#[tokio::test]
#[ignore = "requires public search network access; free-tier limits may apply"]
async fn live_parallel_returns_structured_search_results() {
    for query in [
        "site:rust-lang.org Rust programming language",
        "\"Turing Complete\" game custom ISA RISC-V",
    ] {
        let result = run(
            SearchArguments {
                query: query.into(),
                max_results: 3,
                ..Default::default()
            },
            &WebClient::new(&WebConfig::default()).unwrap(),
            &[SearchProvider::Parallel],
            &Mutex::default(),
        )
        .await
        .unwrap();
        let value: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(value["provider"], "Parallel");
        assert!(
            !value["results"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(value["query_match_unconfirmed"], false);
        println!("Query: {query}");
        for result in value["results"].as_array().unwrap() {
            println!("{} — {}", result["title"], result["url"]);
        }
    }
}

#[tokio::test]
#[ignore = "requires public search network access; free-tier limits may apply"]
async fn live_exa_returns_results_or_an_explicit_quota_failure() {
    let args = SearchArguments {
        query: "Rust programming language".into(),
        max_results: 2,
        ..Default::default()
    };
    match request(
        &args,
        &WebClient::new(&WebConfig::default()).unwrap(),
        SearchProvider::Exa,
        SearchProvider::Exa.endpoint(),
    )
    .await
    {
        Ok(page) => assert!(!page.results.is_empty()),
        Err(failure) => {
            assert!(failure.limited, "{}", failure.message);
            println!("{}", failure.message);
        }
    }
}
