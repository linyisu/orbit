//! Check query identity and explicit URL restrictions; visible snippets only
//! provide a weak signal about quoted phrases, not proof of page relevance.
use super::{Failure, SearchResult, Url, failed};
use crate::web_fetch::dom::attribute;
use html2text::Handle;

fn whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn query_echo(query: &str, url: &Url, body: &str) -> Result<(), Failure> {
    let expected = whitespace(query);
    for (_, actual) in url.query_pairs().filter(|(key, _)| key == "q") {
        if whitespace(&actual) != expected {
            return Err(failed("search redirect changed the query").into());
        }
    }
    let Ok(dom) = html2text::config::plain().parse_html(body.as_bytes()) else {
        return Ok(());
    };
    fn input(node: &Handle) -> Option<String> {
        if node.element_name().as_deref() == Some("input")
            && attribute(node, "name").as_deref() == Some("q")
            && attribute(node, "type").is_none_or(|kind| matches!(kind.as_str(), "text" | "search"))
        {
            return attribute(node, "value");
        }
        node.children.borrow().iter().find_map(input)
    }
    if input(&dom.document).is_some_and(|actual| whitespace(&actual) != expected) {
        return Err(failed("search page displays a different query").into());
    }
    // The title is intentionally not used: engines can abbreviate it or add branding.
    Ok(())
}

fn clauses(query: &str) -> (Vec<String>, Vec<String>) {
    let mut words = Vec::new();
    let mut phrases = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut excluded = false;
    for character in query.chars() {
        if character == '"' {
            if quoted {
                if !excluded && !word.is_empty() {
                    phrases.push(std::mem::take(&mut word));
                }
                word.clear();
            } else {
                // Operators immediately before a quote need full parsing. Only
                // plain quoted phrases participate in the weak visible-match signal.
                excluded = !word.is_empty();
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            quoted = !quoted;
        } else if character.is_whitespace() && !quoted {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
        } else {
            word.push(character);
        }
    }
    if !quoted && !word.is_empty() {
        words.push(word);
    }
    (words, phrases)
}

fn simple_site(words: &[String]) -> Option<Url> {
    if words
        .iter()
        .any(|word| matches!(word.as_str(), "OR" | "NOT" | "|") || word.contains(['(', ')', '|']))
    {
        return None;
    }
    let mut sites = words.iter().filter_map(|word| word.strip_prefix("site:"));
    let site = sites.next()?;
    if sites.next().is_some() || site.is_empty() {
        return None;
    }
    let url = Url::parse(&format!("https://{site}")).ok()?;
    (url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.port().is_none())
    .then_some(url)
}

fn within_site(value: &str, site: &Url) -> bool {
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    let (Some(host), Some(expected)) = (url.host_str(), site.host_str()) else {
        return false;
    };
    let host = host.trim_end_matches('.');
    let expected = expected.trim_end_matches('.');
    let path = site.path().trim_end_matches('/');
    (host == expected || host.ends_with(&format!(".{expected}")))
        && (path.is_empty() || url.path() == path || url.path().starts_with(&format!("{path}/")))
}

fn normalized(value: &str) -> String {
    whitespace(
        &value
            .chars()
            .flat_map(char::to_lowercase)
            .map(|character| {
                if character.is_alphanumeric() {
                    character
                } else {
                    ' '
                }
            })
            .collect::<String>(),
    )
}

pub(super) fn results(
    query: &str,
    results: &mut Vec<SearchResult>,
) -> Result<(Vec<String>, bool), Failure> {
    let (words, phrases) = clauses(query);
    let mut warnings = Vec::new();
    if let Some(site) = simple_site(&words) {
        let original = results.len();
        results.retain(|result| within_site(&result.url, &site));
        if results.is_empty() {
            return Err(failed(
                "search provider ignored site: restriction; no result URL matches it",
            )
            .into());
        }
        if results.len() < original {
            warnings.push(format!(
                "Excluded {} result(s) outside the site: restriction",
                original - results.len()
            ));
        }
    }
    // OR queries can legitimately match an unquoted alternative. Never demand
    // visible evidence for every term: snippets omit much of the indexed page.
    let alternatives = words
        .iter()
        .any(|word| matches!(word.as_str(), "OR" | "|") || word.contains(['(', ')', '|']));
    let phrases: Vec<_> = phrases
        .iter()
        .map(|phrase| normalized(phrase))
        .filter(|phrase| !phrase.is_empty())
        .collect();
    let terms: Vec<_> = words
        .iter()
        .filter(|word| word.len() >= 3 && word.bytes().all(|b| b.is_ascii_alphanumeric()))
        .map(|word| word.to_ascii_lowercase())
        .filter(|word| {
            !matches!(
                word.as_str(),
                "the" | "and" | "for" | "with" | "from" | "how" | "what" | "are" | "not"
            )
        })
        .collect();
    // For simple unquoted Latin queries, a response with none of the words
    // visible is also suspect. This is a weak signal, not a semantic filter.
    let clues = if phrases.is_empty() && terms.len() >= 2 {
        &terms
    } else {
        &phrases
    };
    let unconfirmed = !alternatives
        && !clues.is_empty()
        && !results.iter().any(|result| {
            let visible = format!(
                " {} ",
                normalized(&format!(
                    "{} {} {}",
                    result.title, result.snippet, result.url
                ))
            );
            clues
                .iter()
                .any(|phrase| visible.contains(&format!(" {phrase} ")))
        });
    if unconfirmed {
        let missing = if phrases.is_empty() {
            "No query term"
        } else {
            "No quoted phrase"
        };
        warnings.push(format!("{missing} appears in result titles, snippets or URLs. Relevance is unconfirmed; inspect pages or refine the query before using these results as evidence."));
    }
    Ok((warnings, unconfirmed))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn result(title: &str, url: &str) -> SearchResult {
        SearchResult {
            title: title.into(),
            url: url.into(),
            snippet: String::new(),
        }
    }
    #[test]
    fn query_identity_survives_encoding_but_rewritten_responses_are_rejected() {
        let mut url = Url::parse("https://search.example/query").unwrap();
        url.query_pairs_mut()
            .append_pair("q", "\"Turing Complete\" RV32I");
        assert!(
            query_echo(
                "\"Turing Complete\" RV32I",
                &url,
                r#"<input name="q" type="search" value="&quot;Turing Complete&quot; RV32I">"#
            )
            .is_ok()
        );
        assert!(query_echo("other", &url, "").is_err());
        assert!(
            query_echo(
                "\"Turing Complete\" RV32I",
                &url,
                r#"<input name="q" value="Turing">"#
            )
            .is_err()
        );
    }
    #[test]
    fn site_scope_rejects_unrelated_hosts_and_path_prefix_collisions() {
        let mut results = vec![
            result(
                "Guide",
                "https://steamcommunity.com/sharedfiles/filedetails/?id=1",
            ),
            result(
                "Bad host",
                "https://steamcommunity.com.evil.example/sharedfiles/filedetails",
            ),
            result(
                "Bad path",
                "https://steamcommunity.com/sharedfiles/filedetails-other",
            ),
            result("Other", "https://example.org/"),
        ];
        let (warnings, unconfirmed) = super::results(
            "site:steamcommunity.com/sharedfiles/filedetails guide",
            &mut results,
        )
        .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(warnings.len(), 1);
        assert!(!unconfirmed);
        assert!(super::results("site:github.com guide", &mut results).is_err());
    }
    #[test]
    fn alternatives_and_excluded_operators_do_not_become_mandatory_site_filters() {
        for query in [
            "site:github.com OR site:gitlab.com rust",
            "rust OR site:github.com",
            "-site:github.com rust",
            "\"site:github.com\" OR rust",
        ] {
            let mut results = vec![result("Rust", "https://gitlab.com/rust")];
            assert!(!super::results(query, &mut results).unwrap().1);
            assert_eq!(results.len(), 1);
        }
    }
    #[test]
    fn generic_turing_results_are_suspect_but_hidden_page_terms_are_not_required() {
        let query = "\"Turing Complete\" \"RV32I\" custom instructions architecture .txt or github";
        let mut results = vec![
            result("Alan Turing", "https://example.org/turing"),
            result("Turing AI", "https://turing.ai/"),
        ];
        assert!(super::results(query, &mut results).unwrap().1);
        results.push(result(
            "RV32I single cycle processor",
            "https://github.com/example/rv32i",
        ));
        assert!(!super::results(query, &mut results).unwrap().1);
        assert!(
            !super::results("\"Turing Complete\" OR processor", &mut results)
                .unwrap()
                .1
        );
        assert!(
            !super::results("-\"Turing Complete\" processor", &mut results)
                .unwrap()
                .1
        );
    }
}
