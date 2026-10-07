//! DuckDuckGo HTML requests, locale filters and result validation.
use std::collections::HashSet;

use super::*;
use crate::web_fetch::dom::{attribute as attr, text};
use html2text::Handle;

fn locale(region: &str, language: &str) -> String {
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

pub(super) fn url(args: &SearchArguments, endpoint: &str) -> Result<Url, Failure> {
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
    query.append_pair("kl", &locale(region, lang));
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

    drop(query);
    Ok(url)
}

pub(super) async fn request(
    args: &SearchArguments,
    client: &WebClient,
    endpoint: &str,
) -> Result<SearchPage, Failure> {
    let mut url = url(args, endpoint)?;
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
        let mut results = parse_results(&body, 50);

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

fn parse_results(body: &str, limit: usize) -> Vec<SearchResult> {
    let Ok(dom) = html2text::config::plain().parse_html(body.as_bytes()) else {
        return Vec::new();
    };
    fn collect(node: &Handle, results: &mut Vec<SearchResult>) {
        if attr(node, "class").is_some_and(|v| v.split_whitespace().any(|c| c == "result__a"))
            && let Some(href) = attr(node, "href")
            && let Ok(mut url) = url::Url::parse("https://duckduckgo.com")
                .unwrap()
                .join(&href)
        {
            if url
                .host_str()
                .is_some_and(|h| h == "duckduckgo.com" || h.ends_with(".duckduckgo.com"))
            {
                let destination = url
                    .query_pairs()
                    .find(|(k, _)| k == "uddg")
                    .map(|(_, v)| v.into_owned());
                let Some(destination) = destination else {
                    return;
                };
                let Ok(target) = url::Url::parse(&destination) else {
                    return;
                };
                url = target;
            }

            if let Ok(url) = checked_url(url.as_str()) {
                let title = text(node);
                let snippet = node
                    .get_parent()
                    .and_then(|p| p.get_parent())
                    .map(|p| find_snippet(&p))
                    .unwrap_or_default();

                if !title.is_empty() {
                    results.push(SearchResult {
                        title,
                        url: url.into(),
                        snippet,
                    });
                }
            }
        }

        for child in node.children.borrow().iter() {
            collect(child, results);
        }
    }
    fn find_snippet(node: &Handle) -> String {
        if attr(node, "class").is_some_and(|v| v.split_whitespace().any(|c| c == "result__snippet"))
        {
            return text(node);
        }

        node.children
            .borrow()
            .iter()
            .map(find_snippet)
            .find(|s| !s.is_empty())
            .unwrap_or_default()
    }
    let mut results = Vec::new();
    collect(&dom.document, &mut results);
    results.truncate(limit);
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn duckduckgo_extracts_safe_target_and_description() {
        let results = parse_results(
            r#"<div><h2><a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.org%2Fa">Example</a></h2><a class="result__snippet">Description</a></div>"#,
            10,
        );
        assert_eq!(results[0].url, "https://example.org/a");
        assert_eq!(results[0].snippet, "Description");
    }
}
