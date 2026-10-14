//! Result URL identity and diversity; no relevance ranking or domain blacklist.
use std::collections::{HashMap, HashSet};

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
        url.query_pairs_mut()
            .extend_pairs(query);
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
}
