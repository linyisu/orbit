use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use html2text::Handle;

use super::dom::{attribute as attr, find, find_class, text};
use super::{SearchResult, checked_url};

pub(super) fn parse_results(body: &str, limit: usize) -> Vec<SearchResult> {
    let Ok(dom) = html2text::config::plain().parse_html(body.as_bytes()) else {
        return Vec::new();
    };
    let mut cards = Vec::new();
    collect(&dom.document, &mut cards);
    let mut results = Vec::new();
    for card in cards {
        let Some(heading) = find(&card, "h2") else {
            continue;
        };
        let Some(link) = find(&heading, "a") else {
            continue;
        };
        let Some(url) = attr(&link, "href").and_then(|href| target(&href)) else {
            continue;
        };
        let title = text(&link);
        if title.is_empty() || results.iter().any(|r: &SearchResult| r.url == url) {
            continue;
        }
        results.push(SearchResult {
            title,
            url,
            snippet: find_class(&card, "b_caption")
                .and_then(|caption| find(&caption, "p"))
                .or_else(|| find(&card, "p"))
                .map(|node| text(&node))
                .unwrap_or_default(),
        });
        if results.len() >= limit {
            break;
        }
    }

    results
}

fn collect(node: &Handle, cards: &mut Vec<Handle>) {
    // Restrict extraction to organic results, excluding navigation and ads.
    if attr(node, "class").is_some_and(|v| v.split_whitespace().any(|c| c == "b_algo")) {
        cards.push(node.clone());
        return;
    }

    for child in node.children.borrow().iter() {
        collect(child, cards);
    }
}

fn target(href: &str) -> Option<String> {
    let mut url = url::Url::parse("https://www.bing.com")
        .ok()?
        .join(href)
        .ok()?;
    let bing = |url: &url::Url| {
        url.host_str()
            .is_some_and(|h| h == "bing.com" || h.ends_with(".bing.com"))
    };

    if bing(&url) {
        if url.path() != "/ck/a" {
            return None;
        }
        let encoded = url.query_pairs().find(|(k, _)| k == "u")?.1.into_owned();
        let bytes = URL_SAFE_NO_PAD
            .decode(encoded.strip_prefix("a1")?.trim_end_matches('='))
            .ok()?;
        url = url::Url::parse(std::str::from_utf8(&bytes).ok()?).ok()?;
    }

    if bing(&url) {
        return None;
    }
    checked_url(url.as_str()).ok().map(Into::into)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_sanitized_live_result_cards() {
        let results = parse_results(include_str!("fixtures/bing-results.html"), 10);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].url, "https://github.com/linyisu/orbit");
        assert!(!results[0].snippet.is_empty());
    }

    #[test]
    fn extracts_only_organic_results_and_keeps_snippets_with_their_card() {
        let body = r#"<h2><a href="https://example.org/ad">Ad</a></h2>
        <li class="b_algo"><h2><a href="https://github.com/linyisu/orbit">Orbit &amp; Wayland</a></h2><div class="b_caption"><p>Keyboard <b>input</b>.</p></div></li>
        <li class="b_algo"><h2><a href="https://example.org/empty">No snippet</a></h2></li>
        <li class="b_algo"><h2><a href="http://127.0.0.1/private">Private</a></h2><p>Secret</p></li>
        <li class="b_algo"><h2><a href="https://github.com/linyisu/orbit">Duplicate</a></h2></li>"#;
        let results = parse_results(body, 10);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Orbit & Wayland");
        assert_eq!(results[0].snippet, "Keyboard input.");
        assert!(results[1].snippet.is_empty());
        assert_eq!(parse_results(body, 1).len(), 1);
        assert!(parse_results("<html>Verify you are human</html>", 10).is_empty());
    }

    #[test]
    fn decodes_tracking_links_and_validates_destination() {
        let encoded = URL_SAFE_NO_PAD.encode("https://github.com/linyisu/orbit");
        assert_eq!(
            target(&format!("/ck/a?!&&u=a1{encoded}")),
            Some("https://github.com/linyisu/orbit".into())
        );
        let encoded = URL_SAFE_NO_PAD.encode("http://127.0.0.1/private");
        assert!(target(&format!("/ck/a?u=a1{encoded}")).is_none());
        assert!(target("/search?q=orbit").is_none());
    }
}
