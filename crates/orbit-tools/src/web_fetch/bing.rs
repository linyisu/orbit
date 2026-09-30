use super::{SearchResult, checked_url};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use html2text::{Element, Handle};

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
            snippet: find(&card, "p").map(|node| text(&node)).unwrap_or_default(),
        });
        if results.len() >= limit {
            break;
        }
    }
    results
}

fn attr(node: &Handle, key: &str) -> Option<String> {
    let Element { attrs, .. } = &node.data else {
        return None;
    };
    attrs
        .borrow()
        .iter()
        .find(|a| a.name.local.as_ref() == key)
        .map(|a| a.value.to_string())
}

fn collect(node: &Handle, cards: &mut Vec<Handle>) {
    // Restrict extraction to organic results, excluding navigation and ads.
    if node.element_name().as_deref() == Some("li")
        && attr(node, "class").is_some_and(|v| v.split_whitespace().any(|c| c == "b_algo"))
    {
        cards.push(node.clone());
        return;
    }
    for child in node.children.borrow().iter() {
        collect(child, cards);
    }
}

fn find(node: &Handle, tag: &str) -> Option<Handle> {
    if node.element_name().as_deref() == Some(tag) {
        return Some(node.clone());
    }
    node.children
        .borrow()
        .iter()
        .find_map(|child| find(child, tag))
}

fn text(node: &Handle) -> String {
    let mut html = Vec::new();
    for child in node.children.borrow().iter() {
        if child.serialize(&mut html).is_err() {
            return String::new();
        }
    }
    html2text::config::plain_no_decorate()
        .string_from_read(html.as_slice(), 4096)
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
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
    fn extracts_only_organic_results_and_keeps_snippets_with_their_card() {
        let body = r#"<h2><a href="https://example.org/ad">Ad</a></h2>
        <li class="b_algo"><h2><a href="https://github.com/atx/wtype">wtype &amp; Wayland</a></h2><div class="b_caption"><p>Keyboard <b>input</b>.</p></div></li>
        <li class="b_algo"><h2><a href="https://example.org/empty">No snippet</a></h2></li>
        <li class="b_algo"><h2><a href="http://127.0.0.1/private">Private</a></h2><p>Secret</p></li>
        <li class="b_algo"><h2><a href="https://github.com/atx/wtype">Duplicate</a></h2></li>"#;
        let results = parse_results(body, 10);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "wtype & Wayland");
        assert_eq!(results[0].snippet, "Keyboard input.");
        assert!(results[1].snippet.is_empty());
        assert_eq!(parse_results(body, 1).len(), 1);
        assert!(parse_results("<html>Verify you are human</html>", 10).is_empty());
    }
    #[test]
    fn decodes_tracking_links_and_validates_destination() {
        let encoded = URL_SAFE_NO_PAD.encode("https://github.com/atx/wtype");
        assert_eq!(
            target(&format!("/ck/a?!&&u=a1{encoded}")),
            Some("https://github.com/atx/wtype".into())
        );
        let encoded = URL_SAFE_NO_PAD.encode("http://127.0.0.1/private");
        assert!(target(&format!("/ck/a?u=a1{encoded}")).is_none());
        assert!(target("/search?q=wtype").is_none());
    }
}
