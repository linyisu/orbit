//! Search routing, provider filters and bounded retry/cooldown policy.
use std::{
    collections::{HashMap, HashSet},
    sync::Mutex,
    time::{Instant, SystemTime},
};

use super::*;

mod mcp;
#[cfg(test)]
mod routing_tests;
mod validation;

struct SearchPage {
    results: Vec<SearchResult>,
    request_url: String,
    response_url: String,
    warnings: Vec<String>,
    unconfirmed: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum SearchProvider {
    Parallel,
    Exa,
    Google,
    Bing,
    Duckduckgo,
}

#[derive(Clone, Copy, Debug, Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub(super) enum Freshness {
    Day,
    Week,
    Month,
    Year,
}

impl SearchProvider {
    fn name(self) -> &'static str {
        match self {
            Self::Parallel => "Parallel",
            Self::Exa => "Exa",
            Self::Google => "Google",
            Self::Bing => "Bing",
            Self::Duckduckgo => "DuckDuckGo",
        }
    }

    fn endpoint(self) -> &'static str {
        match self {
            Self::Parallel => "https://search.parallel.ai/mcp",
            Self::Exa => "https://mcp.exa.ai/mcp",
            Self::Google => "https://www.google.com/search?gbv=1",
            Self::Bing => "https://www.bing.com/search",
            Self::Duckduckgo => "https://html.duckduckgo.com/html/",
        }
    }
}

pub(super) fn normalize_arguments(args: &mut SearchArguments) -> Result<(), ToolError> {
    if args.query.chars().count() > 512
        || args.query.len() > 2048
        || !(1..=10).contains(&args.max_results)
    {
        return Err(ToolError::InvalidArguments {
            tool: "web_search".into(),
            message:
                "query must be 1–512 characters (at most 2048 bytes) and max_results must be 1–10"
                    .into(),
        });
    }
    // Do not remove quotes/operators: they express the caller's search intent.
    args.query = args.query.split_whitespace().collect::<Vec<_>>().join(" ");
    if args.query.is_empty() {
        return Err(ToolError::InvalidArguments {
            tool: "web_search".into(),
            message: "query must not be blank".into(),
        });
    }

    if let Some(language) = &mut args.language {
        let parts: Vec<_> = language.split('-').collect();
        if !(2..=3).contains(&parts[0].len())
            || !parts[0].bytes().all(|b| b.is_ascii_alphabetic())
            || parts.len() > 2
            || (parts.len() == 2
                && (parts[1].len() != 2 || !parts[1].bytes().all(|b| b.is_ascii_alphabetic())))
        {
            return Err(ToolError::InvalidArguments {
                tool: "web_search".into(),
                message: "language must be a language code such as en or zh-CN".into(),
            });
        }

        *language = language.to_ascii_lowercase();
    }

    if let Some(region) = &mut args.region {
        if region.len() != 2 || !region.bytes().all(|b| b.is_ascii_alphabetic()) {
            return Err(ToolError::InvalidArguments {
                tool: "web_search".into(),
                message: "region must be a two-letter country code".into(),
            });
        }

        *region = region.to_ascii_uppercase();
    }

    Ok(())
}

#[derive(Debug)]
struct Failure {
    message: String,
    retry: Option<Duration>,
    limited: bool,
}

impl From<ToolError> for Failure {
    fn from(error: ToolError) -> Self {
        let message = match error {
            ToolError::ExecutionFailed(message) | ToolError::InvalidArguments { message, .. } => {
                message
            }
            other => other.to_string(),
        };
        Self {
            message,
            retry: None,
            limited: false,
        }
    }
}

fn retry_after(value: Option<&header::HeaderValue>) -> Option<Duration> {
    let value = value?.to_str().ok()?;
    value
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
        .or_else(|| {
            httpdate::parse_http_date(value)
                .ok()?
                .duration_since(SystemTime::now())
                .ok()
        })
}

fn status_failure(status: reqwest::StatusCode, headers: &header::HeaderMap) -> Failure {
    let transient = status.as_u16() == 429 || status.is_server_error();
    Failure {
        message: format!("web search provider returned HTTP {}", status.as_u16()),
        retry: transient.then(|| {
            retry_after(headers.get(header::RETRY_AFTER))
                .unwrap_or(Duration::from_millis(500))
                .max(Duration::from_millis(500))
        }),
        limited: status.as_u16() == 429
            || (transient
                && retry_after(headers.get(header::RETRY_AFTER))
                    .is_some_and(|delay| delay > Duration::from_secs(2))),
    }
}

pub(super) async fn run(
    args: SearchArguments,
    client: &WebClient,
    providers: &[SearchProvider],
    cooldown: &Mutex<HashMap<SearchProvider, Option<Instant>>>,
) -> Result<String, ToolError> {
    let endpoints: Vec<_> = providers.iter().map(|&p| (p, p.endpoint())).collect();
    route(args, client, &endpoints, cooldown).await
}

async fn route(
    args: SearchArguments,
    client: &WebClient,
    providers: &[(SearchProvider, &str)],
    cooldown: &Mutex<HashMap<SearchProvider, Option<Instant>>>,
) -> Result<String, ToolError> {
    let mut reasons = Vec::new();
    let mut unconfirmed = None;
    let deadline = Instant::now() + REQUEST_TIMEOUT;
    for &(provider, endpoint) in providers {
        if cooldown
            .lock()
            .map_err(|_| failed("search cooldown lock poisoned"))?
            .get(&provider)
            .is_some_and(|until| until.is_none_or(|until| until > Instant::now()))
        {
            reasons.push(format!("{}: provider cooldown", provider.name()));
            continue;
        }

        let mut outcome = attempt(&args, client, provider, endpoint, deadline).await;
        if let Err(error) = &outcome {
            // Rate-limited providers yield immediately to the next engine. Never
            // sleep through a long Retry-After or retry an authentication failure.
            if !error.limited
                && let Some(delay) = error.retry
                && delay <= Duration::from_secs(2)
                && Instant::now() + delay + Duration::from_secs(1) < deadline
            {
                tokio::time::sleep(delay).await;
                outcome = attempt(&args, client, provider, endpoint, deadline).await;
            }
        }

        match outcome {
            Ok(page) => {
                if page.unconfirmed {
                    reasons.push(format!("{}: {}", provider.name(), page.warnings.join("; ")));
                    unconfirmed.get_or_insert((provider, page));
                    continue;
                }
                return Ok(output(&args, provider, page, &reasons));
            }

            Err(error) => {
                if error.limited {
                    let delay = error
                        .retry
                        .unwrap_or(Duration::from_secs(60))
                        .max(Duration::from_secs(60));
                    cooldown
                        .lock()
                        .map_err(|_| failed("search cooldown lock poisoned"))?
                        .insert(provider, Instant::now().checked_add(delay));
                }

                reasons.push(format!("{}: {}", provider.name(), error.message));
            }
        }
    }

    match unconfirmed {
        Some((provider, page)) => Ok(output(&args, provider, page, &reasons)),
        None => Err(failed(reasons.join("; "))),
    }
}

fn output(
    args: &SearchArguments,
    provider: SearchProvider,
    page: SearchPage,
    reasons: &[String],
) -> String {
    let results = search_results::select(page.results, args.max_results);
    let result_values: Vec<_> = results.iter().map(|r| json!({"title": r.title, "url": r.url, "snippet": r.snippet, "snippet_empty": r.snippet.is_empty()})).collect();
    let mut value = json!({"query": args.query, "provider": provider.name(), "results": result_values,
        "results_returned": results.len(), "requested_max_results": args.max_results,
        "fewer_results": results.len() < args.max_results, "source_kind": "untrusted_web_search_results",
        "request_url": page.request_url, "response_url": page.response_url,
        "query_match_unconfirmed": page.unconfirmed, "warnings": page.warnings});
    if !reasons.is_empty() {
        value["fallback_reason"] = reasons.join("; ").into();
    }
    if let Some(freshness) = &args.freshness {
        value["freshness"] = json!(freshness);
    }
    if let Some(language) = &args.language {
        value["language"] = language.clone().into();
    }
    if let Some(region) = &args.region {
        value["region"] = region.clone().into();
    }
    value.to_string()
}

async fn attempt(
    args: &SearchArguments,
    client: &WebClient,
    provider: SearchProvider,
    endpoint: &str,
    deadline: Instant,
) -> Result<SearchPage, Failure> {
    let budget = deadline
        .saturating_duration_since(Instant::now())
        .min(Duration::from_secs(15));
    match tokio::time::timeout(budget, request(args, client, provider, endpoint)).await {
        Ok(result) => result,
        Err(_) => Err(Failure {
            message: "search timed out".into(),
            retry: Some(Duration::from_millis(500)),
            limited: false,
        }),
    }
}

fn duckduckgo_locale(region: &str, language: &str) -> String {
    // DuckDuckGo uses a few legacy identifiers rather than ISO/BCP-47.
    // https://duckduckgo.com/duckduckgo-help-pages/settings/params
    match (region.to_ascii_uppercase().as_str(), language) {
        ("GB", "en") => "uk-en".into(),
        ("JP", "ja") => "jp-jp".into(),
        ("KR", "ko") => "kr-kr".into(),
        ("TW", "zh") => "tw-tzh".into(),
        ("HK", "zh") => "hk-tzh".into(),
        _ => format!("{}-{language}", region.to_ascii_lowercase()),
    }
}

fn provider_url(
    args: &SearchArguments,
    provider: SearchProvider,
    endpoint: &str,
) -> Result<Url, Failure> {
    let mut url = Url::parse(endpoint).map_err(|_| failed("invalid search provider endpoint"))?;
    let language = args
        .language
        .as_deref()
        .unwrap_or(if args.query.is_ascii() { "en" } else { "zh" });
    let lang = language.split('-').next().unwrap();
    let region = args
        .region
        .as_deref()
        .or_else(|| language.split('-').nth(1))
        .unwrap_or(if lang == "zh" { "CN" } else { "US" });
    let mut query = url.query_pairs_mut();
    query.append_pair("q", &args.query);
    match provider {
        SearchProvider::Parallel | SearchProvider::Exa => {
            return Err(failed("MCP search uses POST arguments, not HTML query parameters").into());
        }
        SearchProvider::Google => {
            query
                .append_pair("hl", language)
                .append_pair("gl", region)
                .append_pair("num", &args.max_results.to_string());
            if let Some(freshness) = args.freshness {
                query.append_pair(
                    "tbs",
                    match freshness {
                        Freshness::Day => "qdr:d",
                        Freshness::Week => "qdr:w",
                        Freshness::Month => "qdr:m",
                        Freshness::Year => "qdr:y",
                    },
                );
            }
        }

        SearchProvider::Bing => {
            query
                .append_pair("setlang", language)
                .append_pair("mkt", &format!("{lang}-{region}"))
                .append_pair("count", &args.max_results.to_string());
            if let Some(freshness) = args.freshness {
                query.append_pair(
                    "qft",
                    match freshness {
                        Freshness::Day => " filterui:age-lt1440",
                        Freshness::Week => " filterui:age-lt10080",
                        Freshness::Month => " filterui:age-lt43200",
                        Freshness::Year => " filterui:age-lt525600",
                    },
                );
            }
        }

        SearchProvider::Duckduckgo => {
            query.append_pair("kl", &duckduckgo_locale(region, lang));
            if let Some(freshness) = args.freshness {
                query.append_pair(
                    "df",
                    match freshness {
                        Freshness::Day => "d",
                        Freshness::Week => "w",
                        Freshness::Month => "m",
                        Freshness::Year => "y",
                    },
                );
            }
        }
    }

    drop(query);
    Ok(url)
}

async fn request(
    args: &SearchArguments,
    client: &WebClient,
    provider: SearchProvider,
    endpoint: &str,
) -> Result<SearchPage, Failure> {
    if matches!(provider, SearchProvider::Parallel | SearchProvider::Exa) {
        return mcp::request(args, client, provider, endpoint).await;
    }
    let mut url = provider_url(args, provider, endpoint)?;
    let request_url = url.to_string();
    let mut visited = HashSet::new();
    let mut clients = RequestClients::default();
    for hop in 0..=5 {
        if !visited.insert(url.to_string()) {
            return Err(failed("search redirect loop detected").into());
        }
        let language = args
            .language
            .as_deref()
            .unwrap_or(if args.query.is_ascii() {
                "en-US"
            } else {
                "zh-CN"
            });
        let route = client.route(&url)?;
        let response = clients
            .get(&url, route.as_ref().map(|route| &route.proxy))
            .await?
            .get(url.clone())
            .header(
                header::ACCEPT,
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
            )
            .header(
                header::ACCEPT_LANGUAGE,
                format!("{language},en;q=0.8,zh;q=0.7"),
            )
            .header(header::ACCEPT_ENCODING, "identity")
            .send()
            .await
            .map_err(|error| Failure {
                message: error.without_url().to_string(),
                retry: Some(Duration::from_millis(500)),
                limited: false,
            })?;

        if matches!(response.status().as_u16(), 301 | 302 | 303 | 307 | 308) {
            if hop == 5 {
                return Err(failed("too many redirects (maximum 5)").into());
            }
            let location = response
                .headers()
                .get(header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| failed("redirect has no valid Location header"))?;

            url = redirect_target(&url, location)?;
            continue;
        }

        if !response.status().is_success() {
            return Err(status_failure(response.status(), response.headers()));
        }
        if response
            .headers()
            .get(header::CONTENT_ENCODING)
            .is_some_and(|v| v != "identity")
        {
            return Err(failed(
                "search provider ignored identity encoding; compressed response unsupported",
            )
            .into());
        }

        let media = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_owned();
        let bytes = read_body(response).await?;
        let body = encoding::text(&bytes, &media, true)?;
        validation::query_echo(&args.query, &url, &body)?;
        let mut results = match provider {
            SearchProvider::Parallel | SearchProvider::Exa => unreachable!(),
            SearchProvider::Google => google::parse_results(&body, 50),
            SearchProvider::Bing => bing::parse_results(&body, 50),
            SearchProvider::Duckduckgo => search_results::duckduckgo(&body, 50),
        };

        if results.is_empty() {
            return Err(failed(
                "no readable search results (empty page, consent, challenge or unsupported layout)",
            )
            .into());
        }

        let (warnings, unconfirmed) = validation::results(&args.query, &mut results)?;
        return Ok(SearchPage {
            results,
            request_url,
            response_url: url.to_string(),
            warnings,
            unconfirmed,
        });
    }

    unreachable!()
}

#[cfg(test)]
pub(super) async fn search_with_endpoints(
    args: SearchArguments,
    proxy: Option<&reqwest::Proxy>,
    google: &str,
    bing: &str,
) -> Result<String, ToolError> {
    route(
        args,
        &WebClient::for_test(proxy),
        &[
            (SearchProvider::Google, google),
            (SearchProvider::Bing, bing),
        ],
        &Mutex::default(),
    )
    .await
}

#[cfg(test)]
pub(super) async fn route_for_test(
    args: SearchArguments,
    client: &WebClient,
    endpoint: &str,
) -> Result<String, ToolError> {
    route(
        args,
        client,
        &[(SearchProvider::Bing, endpoint)],
        &Mutex::default(),
    )
    .await
}

#[cfg(test)]
pub(super) async fn search_at(
    args: SearchArguments,
    proxy: Option<&reqwest::Proxy>,
    endpoint: &str,
) -> Result<String, ToolError> {
    route_for_test(args, &WebClient::for_test(proxy), endpoint).await
}

#[cfg(test)]
mod tests {
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
    fn provider_filters_and_counts_are_explicit() {
        let args = SearchArguments {
            query: "rust \"async io\"".into(),
            max_results: 10,
            language: Some("en".into()),
            region: Some("GB".into()),
            freshness: Some(Freshness::Week),
        };

        for provider in [
            SearchProvider::Google,
            SearchProvider::Bing,
            SearchProvider::Duckduckgo,
        ] {
            let url = provider_url(&args, provider, provider.endpoint()).unwrap();
            let q: HashMap<_, _> = url.query_pairs().collect();
            assert_eq!(q["q"], args.query);
            match provider {
                SearchProvider::Parallel | SearchProvider::Exa => unreachable!(),
                SearchProvider::Google => {
                    assert_eq!(q["hl"], "en");
                    assert_eq!(q["gl"], "GB");
                    assert_eq!(q["tbs"], "qdr:w");
                    assert_eq!(q["num"], "10");
                }

                SearchProvider::Bing => {
                    assert_eq!(q["mkt"], "en-GB");
                    assert_eq!(q["qft"], " filterui:age-lt10080");
                    assert_eq!(q["count"], "10");
                }

                SearchProvider::Duckduckgo => {
                    assert_eq!(q["kl"], "uk-en");
                    assert_eq!(q["df"], "w");
                }
            }
        }
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
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy =
            reqwest::Proxy::all(format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let server = tokio::spawn(async move {
            for index in 0..3 {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut bytes = [0; 4096];
                let n = stream.read(&mut bytes).await.unwrap();
                assert!(n > 0);
                let req = String::from_utf8_lossy(&bytes[..n]);
                if index == 0 {
                    assert!(req.contains("google.invalid"));
                    stream.write_all(b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: 120\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
                } else {
                    assert!(req.contains("bing.invalid"));
                    let body =
                        "<li class='b_algo'><h2><a href='https://example.org/'>Rust</a></h2></li>";
                    stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
                }
            }
        });
        let cooldown = Mutex::default();
        let args = SearchArguments {
            query: "rust".into(),
            max_results: 8,
            ..Default::default()
        };

        for i in 0..2 {
            let result = route(
                args.clone(),
                &WebClient::for_test(Some(&proxy)),
                &[
                    (SearchProvider::Google, "http://google.invalid/"),
                    (SearchProvider::Bing, "http://bing.invalid/"),
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
            assert!(result["fewer_results"].as_bool().unwrap());
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
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let proxy =
                reqwest::Proxy::all(format!("http://{}", listener.local_addr().unwrap())).unwrap();
            let server = tokio::spawn(async move {
                let count = if code == 403 { 1 } else { 2 };
                for _ in 0..count {
                    let (mut s, _) = listener.accept().await.unwrap();
                    let mut bytes = [0; 4096];
                    assert!(s.read(&mut bytes).await.unwrap() > 0);
                    s.write_all(format!("HTTP/1.1 {code} Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
                }

                assert!(
                    tokio::time::timeout(Duration::from_millis(100), listener.accept())
                        .await
                        .is_err()
                );
            });
            let result = route(
                SearchArguments {
                    query: "rust".into(),
                    max_results: 1,
                    ..Default::default()
                },
                &WebClient::for_test(Some(&proxy)),
                &[(SearchProvider::Bing, "http://bing.invalid/")],
                &Mutex::default(),
            )
            .await;

            assert!(result.is_err());
            server.await.unwrap();
        }
    }
}
