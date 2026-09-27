use super::{SearchResult, checked_url};
use html2text::{Element, Handle};

pub(super) fn parse_results(body: &str, limit: usize) -> Vec<SearchResult> {
    let Ok(dom) = html2text::config::plain().parse_html(body.as_bytes()) else {
        return Vec::new();
    };
    let mut headings = Vec::new();
    collect_headings(&dom.document, &mut headings);
    let mut results = Vec::new();

    for heading in headings {
        let Some(link) = result_link(&heading) else {
            continue;
        };
        let Some(target) = attribute(&link, "href").and_then(|href| target_url(&href)) else {
            continue;
        };
        let Ok(url) = checked_url(&target) else {
            continue;
        };
        if url
            .host_str()
            .is_some_and(|host| host == "google.com" || host.ends_with(".google.com"))
        {
            continue;
        }
        let title = text(&heading);
        if title.is_empty()
            || results
                .iter()
                .any(|result: &SearchResult| result.url == url.as_str())
        {
            continue;
        }
        results.push(SearchResult {
            title,
            url: url.into(),
            snippet: result_snippet(&heading),
        });
        if results.len() >= limit {
            break;
        }
    }
    results
}

fn attribute(node: &Handle, key: &str) -> Option<String> {
    if let Element { attrs, .. } = &node.data {
        attrs
            .borrow()
            .iter()
            .find(|attr| attr.name.local.as_ref() == key)
            .map(|attr| attr.value.to_string())
    } else {
        None
    }
}

fn collect_headings(node: &Handle, headings: &mut Vec<Handle>) {
    if node.element_name().as_deref() == Some("h3") {
        headings.push(node.clone());
    }
    for child in node.children.borrow().iter() {
        collect_headings(child, headings);
    }
}

fn result_link(heading: &Handle) -> Option<Handle> {
    let mut parent = heading.get_parent();
    while let Some(node) = parent {
        if node.element_name().as_deref() == Some("a") {
            return Some(node);
        }
        parent = node.get_parent();
    }
    heading
        .children
        .borrow()
        .iter()
        .find(|node| node.element_name().as_deref() == Some("a"))
        .cloned()
}

fn target_url(href: &str) -> Option<String> {
    if !href.starts_with("/url?") {
        return Some(href.to_owned());
    }
    let redirect = url::Url::parse(&format!("https://www.google.com{href}")).ok()?;
    redirect
        .query_pairs()
        .find_map(|(key, value)| matches!(key.as_ref(), "q" | "url").then(|| value.into_owned()))
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

fn result_snippet(heading: &Handle) -> String {
    let mut parent = heading.get_parent();
    while let Some(node) = parent {
        // Stop before crossing a result boundary. Never borrow another result's
        // description, even when this result has no snippet.
        let mut headings = Vec::new();
        collect_headings(&node, &mut headings);
        if headings.len() != 1 || matches!(node.element_name().as_deref(), Some("body" | "html")) {
            break;
        }
        if let Some(snippet) = find_snippet(&node) {
            return text(&snippet);
        }
        parent = node.get_parent();
    }
    String::new()
}

fn find_snippet(node: &Handle) -> Option<Handle> {
    if matches!(
        node.element_name().as_deref(),
        Some("a" | "h3" | "script" | "style" | "nav")
    ) {
        return None;
    }
    let classes = attribute(node, "class").unwrap_or_default();
    if attribute(node, "data-sncf").is_some()
        || classes
            .split_whitespace()
            .any(|class| matches!(class, "VwiC3b" | "aCOpRe" | "IsZvec" | "s3v9rd"))
    {
        return Some(node.clone());
    }
    node.children.borrow().iter().find_map(find_snippet)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snippets_stay_inside_their_result_and_preserve_nested_text() {
        let html = r#"
            <main>
              <section><a href="https://example.org/one"><h3>First</h3></a>
                <div class="VwiC3b"><span>Long <em>screenshot</em></span> stitching &#38; alignment.</div>
              </section>
              <section><a href="https://example.org/two"><h3>No snippet</h3></a></section>
              <section><a href="https://example.org/three"><h3>Third</h3></a>
                <div data-sncf="1">Another <b>result</b>.</div>
              </section>
              <section><a href="https://example.org/four"><h3>Basic layout</h3></a>
                <div class="BNeawe s3v9rd AP7Wnd">Basic description.</div>
              </section>
            </main>
        "#;
        let results = parse_results(html, 10);
        assert_eq!(results.len(), 4);
        assert_eq!(results[0].snippet, "Long screenshot stitching & alignment.");
        assert_eq!(results[1].snippet, "");
        assert_eq!(results[2].snippet, "Another result.");
        assert_eq!(results[3].snippet, "Basic description.");
    }

    #[test]
    fn extracts_result_headings_and_decodes_redirects_without_navigation() {
        let html = r#"
            <a href="/preferences">Settings</a>
            <a href="/url?q=https%3A%2F%2Fexample.com%2Fpaper&amp;sa=U"><h3>A <b>paper</b> &amp; code</h3></a>
            <a href="https://example.org/"><h3>Second paper</h3></a>
            <a href="https://example.org/"><h3>Duplicate</h3></a>
            <a href="http://127.0.0.1/"><h3>Private</h3></a>
            <a href="https://accounts.google.com/"><h3>Sign in</h3></a>
        "#;
        let results = parse_results(html, 10);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "A paper & code");
        assert_eq!(results[0].url, "https://example.com/paper");
        assert_eq!(parse_results(html, 1).len(), 1);
        assert!(parse_results("<html>Before you continue to Google</html>", 10).is_empty());
    }
}
