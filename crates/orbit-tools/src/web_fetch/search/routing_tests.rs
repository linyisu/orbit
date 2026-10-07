//! Exercise provider failover against deterministic HTTP responses.
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn search(query: &str, bing_body: &str, duckduckgo_body: &str) -> serde_json::Value {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = reqwest::Proxy::all(format!("http://{}", listener.local_addr().unwrap())).unwrap();
    let query_copy = query.to_owned();
    let bodies = [bing_body.to_owned(), duckduckgo_body.to_owned()];
    let server = tokio::spawn(async move {
        for (host, body) in ["bing.invalid", "ddg.invalid"].into_iter().zip(bodies) {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                let mut buffer = [0; 4096];
                let size = stream.read(&mut buffer).await.unwrap();
                assert!(size > 0);
                request.extend_from_slice(&buffer[..size]);
            }
            let request = String::from_utf8(request).unwrap();
            let url = Url::parse(request.split_whitespace().nth(1).unwrap()).unwrap();
            assert_eq!(url.host_str(), Some(host));
            assert_eq!(
                url.query_pairs().find(|(key, _)| key == "q").unwrap().1,
                query_copy
            );
            stream
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
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
                (SearchProvider::Bing, "http://bing.invalid/search"),
                (SearchProvider::Duckduckgo, "http://ddg.invalid/html/"),
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
    let valid =
        "<a class='result__a' href='https://github.com/example/rv32i'>Turing Complete RV32I</a>";
    for (query, bad, reason) in [
        (
            "Turing Complete RV32I",
            "<input name='q' value='Turing'><li class='b_algo'><h2><a href='https://example.org/'>Alan Turing</a></h2></li>",
            "different query",
        ),
        (
            "site:github.com RV32I",
            "<li class='b_algo'><h2><a href='https://example.org/'>RV32I</a></h2></li>",
            "ignored site:",
        ),
        (
            "\"Turing Complete\" \"RV32I\"",
            "<li class='b_algo'><h2><a href='https://example.org/'>Alan Turing</a></h2></li>",
            "Relevance is unconfirmed",
        ),
    ] {
        let result = search(query, bad, valid).await;
        assert_eq!(result["provider"], "DuckDuckGo");
        assert_eq!(result["query"], query);
        assert_eq!(result["query_match_unconfirmed"], false);
        assert_eq!(
            result["results"][0]["url"],
            "https://github.com/example/rv32i"
        );
        assert!(result["fallback_reason"].as_str().unwrap().contains(reason));
        for field in ["request_url", "response_url"] {
            let url = Url::parse(result[field].as_str().unwrap()).unwrap();
            assert_eq!(url.host_str(), Some("ddg.invalid"));
            assert_eq!(
                url.query_pairs().find(|(key, _)| key == "q").unwrap().1,
                query
            );
        }
    }
}

#[tokio::test]
async fn weak_results_are_retained_with_a_warning_if_alternatives_are_also_weak() {
    let result = search(
        "\"Turing Complete\" \"RV32I\"",
        "<li class='b_algo'><h2><a href='https://example.org/'>Alan Turing</a></h2></li>",
        "<a class='result__a' href='https://other.example/'>Turing AI</a>",
    )
    .await;
    assert_eq!(result["provider"], "Bing");
    assert_eq!(result["query_match_unconfirmed"], true);
    assert_eq!(result["results"].as_array().unwrap().len(), 1);
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
            .contains("DuckDuckGo")
    );
}
