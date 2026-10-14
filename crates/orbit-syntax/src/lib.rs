//! Shared syntax grammars and language recognition for Orbit frontends.
//! Rendering, colors, and layout remain owned by each frontend.
use std::{collections::BTreeMap, sync::LazyLock};

use serde::Deserialize;
use syntect::parsing::{SyntaxReference, SyntaxSet};

/// Grammars linked at build time and loaded once for all code views.
///
/// Parse individual lines without trailing line endings and preserve parser
/// state between lines. Both frontends use this same syntax set.
pub static SYNTAXES: LazyLock<SyntaxSet> = LazyLock::new(|| {
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
    let catalog: Catalog = serde_json::from_str(include_str!("../assets/languages.json"))
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

    label
        .strip_prefix("language-")
        .unwrap_or(&label)
        .to_owned()
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

/// Canonical catalog ID for a code-fence label, retaining unknown labels.
pub fn label_key(info: &str) -> String {
    let full = normalized_label(info);
    let token = full
        .split_whitespace()
        .next()
        .unwrap_or("text")
        .trim_start_matches('.');
    language(&full)
        .or_else(|| language(token))
        .map_or_else(|| token.to_owned(), |language| language.id.clone())
}

/// Resolve a code-fence language, alias, display name, or filename.
///
/// Known unsupported languages return `None` rather than an unrelated grammar.
pub fn syntax_for_label(info: &str) -> Option<&'static SyntaxReference> {
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

    SYNTAXES
        .find_syntax_by_token(token)
        .or_else(|| {
            token
                .rsplit_once('.')
                .and_then(|(_, extension)| SYNTAXES.find_syntax_by_extension(extension))
        })
}

/// Display an unspecified code language as plain text without changing source Markdown.
pub fn display_label(info: &str) -> &str {
    let label = info.trim();
    if label.is_empty() { "text" } else { label }
}

/// Identify a file through the shared grammar catalog, using plain text when unsupported.
pub fn label_for_path(path: &str) -> &'static str {
    syntax_for_path(path)
        .and_then(|syntax| {
            REGISTRY
                .languages
                .iter()
                .find(|language| language.syntect.as_deref() == Some(syntax.name.as_str()))
        })
        .map_or("text", |language| language.id.as_str())
}

/// Resolve a file path by its filename or extension without reading the file.
pub fn syntax_for_path(path: &str) -> Option<&'static SyntaxReference> {
    let filename = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path);
    let extension = filename
        .rsplit_once('.')
        .map_or(filename, |(_, extension)| extension);
    SYNTAXES
        .find_syntax_by_extension(extension)
        .or_else(|| SYNTAXES.find_syntax_by_extension(&extension.to_ascii_lowercase()))
}

/// Whether a code-fence label identifies a Markdown language in the catalog.
pub fn is_markdown_label(info: &str) -> bool {
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
mod tests;
