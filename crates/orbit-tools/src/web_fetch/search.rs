//! Search routing, provider filters and bounded retry/cooldown policy.
use std::{
    collections::HashMap,
    sync::Mutex,
    time::{Instant, SystemTime},
};

use super::*;

mod duckduckgo;
mod mcp;
#[cfg(test)]
mod routing_tests;
#[cfg(test)]
mod test_support;
mod validation;

struct SearchPage {
    results: Vec<SearchResult>,
    request_url: String,
    response_url: String,
    warnings: Vec<String>,
    unconfirmed: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchProvider {
    Parallel,
    Exa,
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
            Self::Duckduckgo => "DuckDuckGo",
        }
    }

    fn endpoint(self) -> &'static str {
        match self {
            Self::Parallel => "https://search.parallel.ai/mcp",
            Self::Exa => "https://mcp.exa.ai/mcp",
            Self::Duckduckgo => "https://html.duckduckgo.com/html/",
        }
    }
}

pub(super) fn normalize_arguments(args: &mut SearchArguments) -> Result<(), ToolError> {
    if args.query.chars().count() > 512
        || args.query.len() > 2_048
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
    args.query = args
        .query
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if args.query.is_empty() {
        return Err(ToolError::InvalidArguments {
            tool: "web_search".into(),
            message: "query must not be blank".into(),
        });
    }

    if let Some(language) = &mut args.language {
        let parts: Vec<_> = language.split('-').collect();
        if !(2..=3).contains(&parts[0].len())
            || !parts[0]
                .bytes()
                .all(|b| b.is_ascii_alphabetic())
            || parts.len() > 2
            || (parts.len() == 2
                && (parts[1].len() != 2
                    || !parts[1]
                        .bytes()
                        .all(|b| b.is_ascii_alphabetic())))
        {
            return Err(ToolError::InvalidArguments {
                tool: "web_search".into(),
                message: "language must be a language code such as en or zh-CN".into(),
            });
        }

        *language = language.to_ascii_lowercase();
    }

    if let Some(region) = &mut args.region {
        if region.len() != 2
            || !region
                .bytes()
                .all(|b| b.is_ascii_alphabetic())
        {
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
    let endpoints: Vec<_> = providers
        .iter()
        .map(|&p| (p, p.endpoint()))
        .collect();
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
    let result_values: Vec<_> = results
        .iter()
        .map(|r| {
            json!({
                "title": r.title,
                "url": r.url,
                "snippet": r.snippet,
                "snippet_empty": r.snippet.is_empty()
            })
        })
        .collect();
    let mut value = json!({
        "query": args.query,
        "provider": provider.name(),
        "results": result_values,
        "results_returned": results.len(),
        "requested_max_results": args.max_results,
        "fewer_results": results.len() < args.max_results,
        "source_kind": "untrusted_web_search_results",
        "request_url": page.request_url,
        "response_url": page.response_url,
        "query_match_unconfirmed": page.unconfirmed,
        "warnings": page.warnings
    });
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

async fn request(
    args: &SearchArguments,
    client: &WebClient,
    provider: SearchProvider,
    endpoint: &str,
) -> Result<SearchPage, Failure> {
    match provider {
        SearchProvider::Parallel | SearchProvider::Exa => {
            mcp::request(args, client, provider, endpoint).await
        }
        SearchProvider::Duckduckgo => duckduckgo::request(args, client, endpoint).await,
    }
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
        &[(SearchProvider::Duckduckgo, endpoint)],
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
mod tests;
