//! Result URL identity and diversity; no relevance ranking or domain blacklist.
use std::collections::{HashMap, HashSet};

use html2text::Handle;

use super::dom::{attribute as attr, text};
use super::{SearchResult, checked_url};

const MAX_PER_HOST: usize = 3;

fn canonical_url(value: &str) -> Option<String> {
    let mut url = checked_url(value).ok()?;
    let query: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| {
            let key = key.to_ascii_lowercase();
            !key.starts_with("utm_") && !matches!(key.as_str(), "fbclid" | "gclid" | "spm")
        })
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();

    url.set_query(None);
    if !query.is_empty() {
        url.query_pairs_mut().extend_pairs(query);
    }
    Some(url.into())
}

pub(super) fn select(results: Vec<SearchResult>, limit: usize) -> Vec<SearchResult> {
    let mut seen = HashSet::new();
    let mut hosts = HashMap::new();
    let mut selected = Vec::new();
    let mut overflow = Vec::new();
    for mut result in results {
        let Some(url) = canonical_url(&result.url) else {
            continue;
        };
        if !seen.insert(url.clone()) {
            continue;
        }
        result.url = url;
        let host = url::Url::parse(&result.url)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_default();
        let count = hosts.entry(host).or_insert(0);
        if *count < MAX_PER_HOST {
            *count += 1;
            selected.push(result);
        } else {
            overflow.push(result);
        }
    }
    // Soft cap: promote other hosts first, but don't discard otherwise useful
    // results when the provider has no alternatives (e.g. site: queries).
    selected.extend(overflow);
    selected.truncate(limit);
    selected
}

pub(super) fn duckduckgo(body: &str, limit: usize) -> Vec<SearchResult> {
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
    fn canonicalizes_tracking_and_promotes_other_hosts_without_losing_site_results() {
        let urls = [
            "https://EXAMPLE.com/a?utm_source=x&x=1",
            "https://example.com/a?x=1&fbclid=y",
            "https://example.com/b",
            "https://example.com/c",
            "https://example.com/d",
            "https://other.org/?",
            "https://example.com/e",
        ];
        let results = urls
            .iter()
            .map(|u| SearchResult {
                title: "Title".into(),
                url: (*u).into(),
                snippet: String::new(),
            })
            .collect();
        let selected = select(results, 5);
        assert_eq!(selected.len(), 5);
        assert_eq!(selected[0].url, "https://example.com/a?x=1");
        assert_eq!(selected[3].url, "https://other.org/");
    }

    #[test]
    fn duckduckgo_extracts_safe_target_and_description() {
        let results = duckduckgo(
            r#"<div><h2><a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.org%2Fa">Example</a></h2><a class="result__snippet">Description</a></div>"#,
            10,
        );
        assert_eq!(results[0].url, "https://example.org/a");
        assert_eq!(results[0].snippet, "Description");
    }
}
