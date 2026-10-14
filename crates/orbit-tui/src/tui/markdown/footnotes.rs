//! Parser-defined footnotes share numbering, styling and message-local targets.
use std::{
    collections::BTreeMap,
    ops::Range,
    time::{Duration, Instant},
};

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use ratatui::{
    style::{Modifier, Style},
    text::Span,
};

use super::{Context, LinkedLine, THEME};

const TARGET_PREFIX: &str = "orbit://footnote/";

const FOCUS_DURATION: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub(in crate::tui) struct Focus {
    pub entry: usize,
    pub target: String,
    pub since: Instant,
}

impl Focus {
    pub fn expired(&self) -> bool {
        self.since.elapsed() >= FOCUS_DURATION
    }
}

#[derive(Clone, Default)]
pub(super) struct Catalog {
    pub numbers: BTreeMap<String, usize>,
    definitions: Vec<Definition>,
}

#[derive(Clone)]
struct Definition {
    label: String,
    range: Range<usize>,
    body: String,
}

pub(super) fn key(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub(super) fn target(label: &str) -> String {
    format!(
        "{TARGET_PREFIX}{}",
        url::form_urlencoded::byte_serialize(key(label).as_bytes()).collect::<String>()
    )
}

pub(in crate::tui) fn is_target(value: &str) -> bool {
    value
        .strip_prefix(TARGET_PREFIX)
        .is_some_and(|label| !label.is_empty())
}

pub(super) fn style() -> Style {
    Style::default()
        .fg(THEME.md_link)
        .add_modifier(Modifier::UNDERLINED)
}

impl Catalog {
    pub fn new(source: &str) -> Self {
        if !source.contains("[^") {
            return Self::default();
        }
        let mut catalog = Self::default();
        let mut definition = None;
        let options = Options::ENABLE_FOOTNOTES | Options::ENABLE_TABLES | Options::ENABLE_MATH;
        for (event, range) in Parser::new_ext(source, options).into_offset_iter() {
            match event {
                Event::Start(Tag::FootnoteDefinition(label)) => {
                    definition = Some((key(&label), range.start, None));
                }
                Event::End(TagEnd::FootnoteDefinition) => {
                    if let Some((label, start, body_start)) = definition.take() {
                        let body_start = body_start.unwrap_or(range.end);
                        let mut lines = source[body_start..range.end].lines();
                        let mut body = lines
                            .next()
                            .unwrap_or_default()
                            .to_owned();
                        for line in lines {
                            body.push('\n');
                            body.push_str(
                                line.strip_prefix("    ")
                                    .or_else(|| line.strip_prefix('\t'))
                                    .unwrap_or(line),
                            );
                        }

                        catalog.definitions.push(Definition {
                            label,
                            range: start..range.end,
                            body,
                        });
                    }
                }

                Event::FootnoteReference(label) => {
                    let next = catalog.numbers.len() + 1;
                    catalog
                        .numbers
                        .entry(key(&label))
                        .or_insert(next);
                }

                _ => {
                    if let Some((_, _, body_start)) = &mut definition {
                        body_start.get_or_insert(range.start);
                    }
                }
            }
        }

        for definition in &catalog.definitions {
            let next = catalog.numbers.len() + 1;
            catalog
                .numbers
                .entry(definition.label.clone())
                .or_insert(next);
        }

        catalog
    }

    pub fn block(
        &self,
        offset: usize,
        width: u16,
        path: &str,
        depth: usize,
        context: &Context<'_>,
    ) -> Option<(usize, Vec<LinkedLine>)> {
        let definition = self
            .definitions
            .iter()
            .find(|definition| definition.range.start == offset)?;
        let number = self.numbers.get(&definition.label)?;
        let marker = format!("[{number}]");
        let inset = marker.len() + 1;
        // A note's body retains the full document's links and footnote catalog;
        // its dedented source no longer shares the document's byte offsets.
        let mut lines = super::document_with_context(
            &definition.body,
            width.saturating_sub(inset as u16),
            path,
            depth,
            context,
            None,
        );
        if lines.is_empty() {
            lines.push(LinkedLine::default());
        }
        for (index, line) in lines.iter_mut().enumerate() {
            line.strip_body_margin();
            let prefix = if index == 0 {
                vec![
                    Span::raw("  "),
                    Span::styled(marker.clone(), style()),
                    Span::raw(" "),
                ]
            } else {
                vec![Span::raw(" ".repeat(inset + 2))]
            };

            *line = line.clone().framed(prefix, Vec::new());
            line.anchors
                .push(target(&definition.label));
            if index == 0 {
                // The label is semantic text, not generated border decoration.
                line.decorations
                    .retain(|range| range.end <= 2 || range.start >= inset + 2);
            }
        }

        Some((definition.range.len(), lines))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_and_named_notes_share_numbered_references_and_definition_anchors() {
        let source = "这是第一条[^1]，这是命名脚注[^note2]，再次引用[^1]。\n\n[^1]: 第一条 **补充说明**。\n[^note2]: \
            第二条支持[链接](https://example.com)。\n";
        let lines = super::super::document(source, 80);
        let text = lines
            .iter()
            .map(LinkedLine::text)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            text.contains("这是第一条[1]，这是命名脚注[2]，再次引用[1]。"),
            "{text}"
        );
        assert!(text.contains("[1] 第一条 补充说明。"), "{text}");
        assert!(text.contains("[2] 第二条支持链接。"), "{text}");
        assert_eq!(
            lines[0]
                .links
                .iter()
                .map(|link| link.url.as_str())
                .collect::<Vec<_>>(),
            vec![target("1"), target("note2"), target("1")]
        );
        let anchors = lines
            .iter()
            .flat_map(|line| &line.anchors)
            .collect::<Vec<_>>();
        assert_eq!(anchors, vec![&target("1"), &target("note2")]);
        for span in lines[0]
            .line
            .spans
            .iter()
            .filter(|span| span.content.starts_with('['))
        {
            assert_eq!(span.style, style());
        }
    }

    #[test]
    fn footnotes_keep_multiline_markdown_and_leave_missing_escaped_or_code_references_literal() {
        let source = "引用[^命名]；未定义[^missing]；代码 `[^命名]`；转义 \\[^命名]。\n\n[^命名]: **第一段**\n\n    \
            第二段带[链接](https://example.com)。\n\n后文\n";
        let lines = super::super::document(source, 80);
        let text = lines
            .iter()
            .map(LinkedLine::text)
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            text.contains("引用[1]；未定义[^missing]；代码 [^命名]；转义 [^命名]。"),
            "{text}"
        );
        assert!(text.contains("[1] 第一段"), "{text}");
        assert!(text.contains("第二段带链接。"), "{text}");
        assert!(text.contains("后文"));
        assert_eq!(lines[0].links.len(), 1);
        assert!(
            lines
                .iter()
                .flat_map(|line| &line.links)
                .any(|link| link.url == "https://example.com")
        );
    }

    #[test]
    fn note_body_uses_document_reference_links_and_other_footnote_numbers() {
        let source = "引用[^first] 和[^second]。\n\n[^first]: [外部链接][web]，也参考[^second]。\n[^second]: \
            第二条说明。\n\n[web]: https://example.com\n";
        let lines = super::super::document(source, 80);
        let note = lines
            .iter()
            .find(|line| line.anchors.contains(&target("first")))
            .unwrap();
        assert!(
            note.text()
                .contains("[1] 外部链接，也参考[2]。"),
            "{}",
            note.text()
        );
        assert_eq!(
            note.links
                .iter()
                .map(|link| link.url.as_str())
                .collect::<Vec<_>>(),
            vec!["https://example.com".to_owned(), target("second")]
        );
    }
}
