//! Language labels are data, not renderer branches. The catalog keeps the full
//! pinned Shiki language list and explicit targets in our Syntect bundle.
use std::{collections::BTreeMap, sync::LazyLock};

use serde::Deserialize;
use syntect::parsing::{SyntaxReference, SyntaxSet};

// Link grammars at build time, not when the user first resumes a conversation.
pub(super) static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(|| {
    syntect::dumps::from_uncompressed_data(include_bytes!(concat!(
        env!("OUT_DIR"),
        "/syntaxes.bin"
    )))
    .expect("valid compiled syntax bundle")
});

#[derive(Deserialize)]
struct Catalog {
    languages: Vec<Language>,
    special_languages: Vec<Language>,
}

#[derive(Deserialize)]
struct Language {
    id: String,
    name: String,
    aliases: Vec<String>,
    #[serde(default)]
    compatibility_aliases: Vec<String>,
    syntect: Option<String>,
    #[serde(default)]
    markdown: bool,
}

struct Registry {
    languages: Vec<Language>,
    labels: BTreeMap<String, usize>,
}

static REGISTRY: LazyLock<Registry> = LazyLock::new(|| {
    let catalog: Catalog = serde_json::from_str(include_str!("../../assets/languages.json"))
        .expect("valid bundled language catalog");
    let languages: Vec<_> = catalog
        .languages
        .into_iter()
        .chain(catalog.special_languages)
        .collect();
    let mut labels = BTreeMap::new();

    // IDs and aliases take precedence over potentially ambiguous display names.
    for (index, language) in languages.iter().enumerate() {
        labels
            .entry(language.name.to_ascii_lowercase())
            .or_insert(index);
    }

    for (index, language) in languages.iter().enumerate() {
        for label in std::iter::once(&language.id)
            .chain(&language.aliases)
            .chain(&language.compatibility_aliases)
        {
            labels.insert(label.to_ascii_lowercase(), index);
        }
    }

    Registry { languages, labels }
});

fn normalized_label(info: &str) -> String {
    let label = info
        .trim()
        .trim_matches(|c| c == '{' || c == '}')
        .trim_start_matches('.')
        .to_ascii_lowercase();

    label.strip_prefix("language-").unwrap_or(&label).to_owned()
}

fn language(label: &str) -> Option<&'static Language> {
    REGISTRY
        .labels
        .get(label)
        .map(|index| &REGISTRY.languages[*index])
}

fn syntax(language: &Language) -> Option<&'static SyntaxReference> {
    language
        .syntect
        .as_deref()
        .and_then(|name| SYNTAXES.find_syntax_by_name(name))
}

pub(super) fn syntax_for_label(info: &str) -> Option<&'static SyntaxReference> {
    let full = normalized_label(info);
    if let Some(language) = language(&full) {
        return syntax(language);
    }
    if let Some(syntax) = SYNTAXES.find_syntax_by_token(&full) {
        return Some(syntax);
    }

    let token = full
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_start_matches('.');
    if let Some(language) = language(token) {
        return syntax(language);
    }

    SYNTAXES.find_syntax_by_token(token).or_else(|| {
        token
            .rsplit_once('.')
            .and_then(|(_, extension)| SYNTAXES.find_syntax_by_extension(extension))
    })
}

pub(super) fn is_markdown_label(info: &str) -> bool {
    let full = normalized_label(info);
    let token = full
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .trim_start_matches('.');

    language(&full)
        .or_else(|| language(token))
        .is_some_and(|language| language.markdown)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalog_target_exists_and_all_ids_and_aliases_resolve() {
        for language in &REGISTRY.languages {
            if let Some(target) = &language.syntect {
                assert!(
                    SYNTAXES.find_syntax_by_name(target).is_some(),
                    "{}: missing {target}",
                    language.id
                );
            }

            for label in std::iter::once(&language.id)
                .chain(std::iter::once(&language.name))
                .chain(&language.aliases)
                .chain(&language.compatibility_aliases)
            {
                let actual = syntax_for_label(label).map(|syntax| syntax.name.as_str());
                assert_eq!(
                    actual,
                    language.syntect.as_deref(),
                    "{}: {label}",
                    language.id
                );
            }
        }
    }

    #[test]
    fn unsupported_catalog_languages_do_not_fall_through_to_unrelated_extensions() {
        // Syntect associates .v with Verilog, but Shiki's language ID v means V.
        assert!(syntax_for_label("v").is_none());
        assert_eq!(syntax_for_label("module.v").unwrap().name, "Verilog");
        assert!(syntax_for_label("angular-html").is_none());
        assert!(syntax_for_label("invented-language").is_none());
    }

    #[test]
    fn markdown_aliases_and_attributes_share_the_catalog() {
        for label in [
            "md",
            "markdown",
            "mdx",
            "gfm",
            "commonmark",
            "{.md}",
            "language-md",
            "md title=example",
        ] {
            assert!(is_markdown_label(label), "{label}");
        }

        assert!(!is_markdown_label("rust"));
        assert!(!is_markdown_label("unknown"));
    }
}
