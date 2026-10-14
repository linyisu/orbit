//! Conservative, on-demand reference hints for languages without a grammar.
//! Only sampled declaration names are retained, never every token in a file.
use std::collections::BTreeSet;

use super::Identifier;

pub(crate) fn references(source: &str, names: &BTreeSet<&str>) -> (Vec<Identifier>, bool) {
    const MAX_REFERENCES: usize = 512;
    let mut found = Vec::new();
    let mut seen = BTreeSet::new();
    let mut chars = source.char_indices().peekable();
    let mut line = 1;
    while let Some((start, ch)) = chars.next() {
        if ch == '\n' {
            line += 1;
            continue;
        }
        // Common comment/string forms. These remain lexical candidates, not
        // language-aware resolution (e.g. heredocs require a real grammar).
        if ch == '#'
            || (ch == '/'
                && chars
                    .peek()
                    .is_some_and(|(_, c)| *c == '/'))
        {
            for (_, c) in chars.by_ref() {
                if c == '\n' {
                    line += 1;
                    break;
                }
            }
            continue;
        }
        if ch == '/'
            && chars
                .peek()
                .is_some_and(|(_, c)| *c == '*')
        {
            chars.next();
            let mut previous = '\0';
            for (_, c) in chars.by_ref() {
                if c == '\n' {
                    line += 1;
                }
                if previous == '*' && c == '/' {
                    break;
                }
                previous = c;
            }
            continue;
        }
        if matches!(ch, '\'' | '"' | '`') {
            let mut escaped = false;
            for (_, c) in chars.by_ref() {
                if c == '\n' {
                    line += 1;
                }
                if !escaped && c == ch {
                    break;
                }
                escaped = !escaped && c == '\\';
            }
            continue;
        }
        if ch.is_alphanumeric() || ch == '_' || ch == '$' {
            let mut end = start + ch.len_utf8();
            while let Some(&(offset, c)) = chars.peek() {
                if !(c.is_alphanumeric() || c == '_' || c == '$') {
                    break;
                }
                end = offset + c.len_utf8();
                chars.next();
            }
            let name = &source[start..end];
            if !ch.is_numeric() && names.contains(name) && seen.insert((name, line)) {
                if found.len() == MAX_REFERENCES {
                    return (found, true);
                }
                found.push(Identifier {
                    name: name.into(),
                    line,
                });
            }
        }
    }
    (found, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn references_exclude_literals_comments_numbers_and_duplicate_tokens() {
        let source = "resolve resolve\n// resolve\n/* resolve\nresolve */\n\"resolve\" 'resolve' \
            `resolve`\n# resolve\n123resolve 123\nresolve";
        let (found, truncated) = references(source, &BTreeSet::from(["resolve", "123"]));
        assert!(!truncated);
        assert_eq!(
            found
                .iter()
                .map(|r| (r.name.as_str(), r.line))
                .collect::<Vec<_>>(),
            [("resolve", 1), ("resolve", 8)]
        );
        let (found, truncated) =
            references(&"resolve\n".repeat(10_000), &BTreeSet::from(["resolve"]));
        assert!(truncated);
        assert_eq!(found.len(), 512);
    }
}
