//! Standard HTML fragments projected onto the same terminal block layout as Markdown.
//! No browser, script execution or resource fetch is involved.
use std::cell::RefCell;

use html2text::{Element, Handle, RcDom};
use html5ever::{
    QualName, local_name, ns,
    tendril::{StrTendril, TendrilSink},
    tokenizer::{BufferQueue, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer},
};
use ratatui::{
    style::Style,
    text::{Line, Span},
};

use super::{Cell, Context, Disclosure, InlineState, LinkedLine, THEME, css, html};

#[cfg(test)]
mod tests;

pub(crate) struct Block {
    pub bytes: usize,
    pub complete: bool,
    pub lines: Vec<LinkedLine>,
}

#[derive(Default)]
struct Boundary {
    root: Option<String>,
    depth: usize,
    complete: bool,
    rejected: bool,
}

struct Sink(RefCell<Boundary>);

impl TokenSink for Sink {
    type Handle = ();
    fn process_token(&self, token: Token, _: u64) -> TokenSinkResult<()> {
        let mut state = self.0.borrow_mut();
        if state.complete || state.rejected {
            return TokenSinkResult::Continue;
        }
        match token {
            Token::TagToken(tag) => {
                let name = tag.name.as_ref();
                if state.root.is_none() {
                    if tag.kind != TagKind::StartTag
                        || !matches!(
                            name,
                            "details" | "blockquote" | "table" | "div" | "p" | "hr" | "img"
                        )
                    {
                        state.rejected = true;
                    } else {
                        state.root = Some(name.to_owned());
                        state.depth = 1;
                        state.complete = matches!(name, "hr" | "img");
                    }
                } else if state.root.as_deref() == Some(name) {
                    if tag.kind == TagKind::EndTag {
                        state.depth = state.depth.saturating_sub(1);
                        state.complete = state.depth == 0;
                    } else {
                        state.depth += 1;
                    }
                }
            }

            Token::CharacterTokens(text) if state.root.is_none() && !text.trim().is_empty() => {
                state.rejected = true
            }
            _ => {}
        }

        TokenSinkResult::Continue
    }
}

/// Bound an HTML block without interpreting tags ourselves. Incomplete blocks stay
/// provisional during streaming; Markdown fences inside details remain literal.
pub(crate) fn render(source: &str, width: u16, path: &str, depth: usize) -> Option<Block> {
    let first = source.split('\n').next()?;
    if !first.trim_start().starts_with('<')
        || first.starts_with("    ")
        || first.starts_with('\t')
        || depth >= 32
    {
        return None;
    }

    let tokenizer = Tokenizer::new(Sink(RefCell::new(Boundary::default())), Default::default());
    let input = BufferQueue::default();
    let mut bytes = 0;
    let mut fence = None;
    let context = Context::default();
    for part in source.split_inclusive('\n') {
        bytes += part.len();
        let was_fenced = fence.is_some();
        let _ = context.code_line(part.trim_end_matches(['\r', '\n']), &mut fence);
        if !was_fenced && fence.is_none() {
            input.push_back(StrTendril::from(protect_inline_code(part)));
            let _ = tokenizer.feed(&input);
        }

        let state = tokenizer.sink.0.borrow();
        if state.rejected {
            return None;
        }
        if state.complete {
            break;
        }
    }

    let boundary = tokenizer.sink.0.borrow();
    boundary.root.as_ref()?;
    let complete = boundary.complete;
    drop(boundary);
    // Protect literal Markdown code before passing the fragment through HTML's tree builder.
    let mut protected = String::new();
    let mut fence = None;
    for line in source[..bytes].split_inclusive('\n') {
        let was_fenced = fence.is_some();
        let _ = context.code_line(line.trim_end_matches(['\r', '\n']), &mut fence);
        if was_fenced || fence.is_some() {
            protected.push_str(&line.replace('&', "&amp;").replace('<', "&lt;"));
        } else {
            protected.push_str(&protect_inline_code(line));
        }
    }

    let dom = html5ever::parse_fragment(
        RcDom::default(),
        Default::default(),
        QualName::new(None, ns!(html), local_name!("div")),
        Vec::new(),
        false,
    )
    .one(protected);
    let mut lines = Vec::new();
    for (index, node) in dom.document.children.borrow().iter().enumerate() {
        lines.extend(node_lines(
            node,
            width,
            &format!("{path}/{index}"),
            depth + 1,
        ));
    }

    Some(Block {
        bytes,
        complete,
        lines,
    })
}

fn protect_inline_code(source: &str) -> String {
    let ranges = pulldown_cmark::Parser::new(source)
        .into_offset_iter()
        .filter_map(|(event, range)| {
            matches!(event, pulldown_cmark::Event::Code(_)).then_some(range)
        });
    let mut out = String::new();
    let mut offset = 0;
    for range in ranges {
        out.push_str(&source[offset..range.start]);
        out.push_str(
            &source[range.clone()]
                .replace('&', "&amp;")
                .replace('<', "&lt;"),
        );
        offset = range.end;
    }

    out.push_str(&source[offset..]);
    out
}

fn attribute(node: &Handle, key: &str) -> String {
    if let Element { attrs, .. } = &node.data {
        attrs
            .borrow()
            .iter()
            .find(|attr| attr.name.local.as_ref() == key)
            .map(|attr| attr.value.to_string())
            .unwrap_or_default()
    } else {
        String::new()
    }
}

fn has_attribute(node: &Handle, key: &str) -> bool {
    matches!(&node.data, Element { attrs, .. } if attrs.borrow().iter().any(|attr| attr.name.local.as_ref() == key))
}

fn source(node: &Handle) -> String {
    let mut bytes = Vec::new();
    if node.serialize(&mut bytes).is_err() {
        return String::new();
    }
    let source = String::from_utf8(bytes).unwrap_or_default();
    if node.element_name().is_some() {
        return source;
    }
    // Text nodes are serialized with escaped entities; restore their Markdown source.
    html::events(&source)
        .map(|events| {
            events
                .into_iter()
                .filter_map(|event| match event {
                    html::Event::Text(text) => Some(text),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or(source)
}

fn children_source(node: &Handle, exclude: Option<&str>) -> String {
    node.children
        .borrow()
        .iter()
        .filter(|child| child.element_name().as_deref() != exclude || exclude.is_none())
        .map(source)
        .collect()
}

fn node_lines(node: &Handle, width: u16, path: &str, depth: usize) -> Vec<LinkedLine> {
    let Some(name) = node.element_name() else {
        return super::document_at(source(node).trim(), width, path, depth);
    };
    if name == "html" {
        return node
            .children
            .borrow()
            .iter()
            .enumerate()
            .flat_map(|(index, child)| node_lines(child, width, &format!("{path}/{index}"), depth))
            .collect();
    }

    let presentation = css::parse(&attribute(node, "style"));
    let body = children_source(node, None);
    let outer_margin = if matches!(name.as_str(), "div" | "p") {
        super::BODY_MARGIN.min(width / 2)
    } else {
        0
    };
    let block_width = width.saturating_sub(outer_margin * 2);
    let mut lines = match name.as_str() {
        "details" => {
            let fold = Disclosure {
                id: path.to_owned(),
                open: has_attribute(node, "open"),
            };
            let summary = node
                .children
                .borrow()
                .iter()
                .find(|child| child.element_name().as_deref() == Some("summary"))
                .cloned();
            let summary = summary
                .map(|node| children_source(&node, None))
                .unwrap_or_else(|| "详细内容".into());
            let mut lines = super::document_at(
                summary.trim(),
                width.saturating_sub(2),
                &format!("{path}/summary"),
                depth,
            );
            if lines.is_empty() {
                lines.push(LinkedLine::plain(Line::from("详细内容")));
            }
            for line in &mut lines {
                line.strip_body_margin();
                line.disclosure = Some(fold.clone());
                *line = line.clone().framed(
                    vec![
                        Span::raw("  "),
                        Span::styled(
                            format!("{} ", super::icons::CHEVRON_RIGHT),
                            Style::default().fg(THEME.secondary),
                        ),
                    ],
                    Vec::new(),
                );
            }

            let mut content = super::document_at(
                children_source(node, Some("summary")).trim(),
                width.saturating_sub(2),
                &format!("{path}/body"),
                depth,
            );

            for line in &mut content {
                line.folds.push(fold.clone());
                *line = line.clone().framed(vec![Span::raw("  ")], Vec::new());
            }

            lines.extend(content);
            lines
        }

        "table" => table(node, width, path, depth),
        "hr" => vec![LinkedLine::chrome(super::rule_line(width))],
        "blockquote" => super::document_at(body.trim(), super::body_width(width), path, depth)
            .into_iter()
            .map(|mut line| {
                line.strip_body_margin();
                for span in &mut line.line.spans {
                    if span.style.fg.is_none() {
                        span.style = span.style.fg(THEME.md_quote_text);
                    }
                }

                line.framed(
                    vec![
                        Span::raw("  "),
                        Span::styled("▎ ", Style::default().fg(THEME.md_quote_rail)),
                    ],
                    Vec::new(),
                )
            })
            .collect(),

        "div" | "p" => {
            let inset = usize::from(presentation.rail.is_some()) * 2
                + usize::from(presentation.padding) * 2;
            let inner = block_width.saturating_sub(inset as u16).max(1);
            let mut content = super::document_at(body.trim(), inner.saturating_add(2), path, depth);
            for line in &mut content {
                line.strip_body_margin();
            }
            if presentation.padding {
                content.insert(0, LinkedLine::chrome(Line::default()));
                content.push(LinkedLine::chrome(Line::default()));
            }

            let mut out = Vec::new();
            if presentation.margin {
                out.push(LinkedLine::plain(Line::default()));
            }
            for line in content {
                for mut line in line.wrap(inner) {
                    let mut prefix = Vec::new();
                    if let Some(color) = presentation.rail {
                        prefix.push(Span::styled("▎ ", Style::default().fg(color)));
                    }
                    if presentation.padding {
                        prefix.push(Span::raw(" "));
                    }
                    let trailing = if presentation.style.bg.is_some() || presentation.padding {
                        vec![Span::raw(" ".repeat(
                            usize::from(inner).saturating_sub(line.line.width())
                                + usize::from(presentation.padding),
                        ))]
                    } else {
                        Vec::new()
                    };

                    line = line.framed(prefix, trailing);
                    line.line.alignment = presentation.alignment;
                    out.push(line);
                }
            }

            if presentation.margin {
                out.push(LinkedLine::plain(Line::default()));
            }
            out
        }

        _ => {
            // Inline tags, images and unsupported tags all go through the shared inline projection.
            let mut fence = None;
            let mut state = InlineState::default();
            Context::default().linked_lines(&source(node), 0, &mut fence, &mut state)
        }
    };

    for line in &mut lines {
        if let Some(table) = &mut line.table {
            table.style = presentation.style.patch(table.style);
        }
        for span in &mut line.line.spans {
            span.style = presentation.style.patch(span.style);
        }
        if presentation.alignment.is_some() {
            line.line.alignment = presentation.alignment;
        }
        *line = line.clone().align_to(block_width);
        if outer_margin > 0 {
            let trailing = if presentation.style.bg.is_some()
                || presentation.padding
                || presentation.rail.is_some()
            {
                vec![Span::raw(" ".repeat(usize::from(outer_margin)))]
            } else {
                Vec::new()
            };

            *line = line.clone().framed(
                vec![Span::raw(" ".repeat(usize::from(outer_margin)))],
                trailing,
            );
        }
    }

    lines
}

fn table(node: &Handle, width: u16, path: &str, depth: usize) -> Vec<LinkedLine> {
    fn rows(node: &Handle, out: &mut Vec<Handle>) {
        for child in node.children.borrow().iter() {
            match child.element_name().as_deref() {
                Some("tr") => out.push(child.clone()),
                Some("thead" | "tbody" | "tfoot") => rows(child, out),
                _ => {}
            }
        }
    }
    let mut elements = Vec::new();
    rows(node, &mut elements);
    let cells: Vec<Vec<Cell>> = elements
        .iter()
        .enumerate()
        .map(|(r, row)| {
            row.children
                .borrow()
                .iter()
                .filter(|cell| matches!(cell.element_name().as_deref(), Some("td" | "th")))
                .enumerate()
                .map(|(c, cell)| {
                    let mut lines = super::document_at(
                        children_source(cell, None).trim(),
                        width,
                        &format!("{path}/{r}/{c}"),
                        depth,
                    );

                    for line in &mut lines {
                        if line
                            .line
                            .spans
                            .first()
                            .is_some_and(|span| span.content == "  ")
                        {
                            line.line.spans.remove(0);
                            line.shift_offsets(-2);
                        }
                    }

                    Cell {
                        lines,
                        colspan: attribute(cell, "colspan")
                            .parse::<usize>()
                            .unwrap_or(1)
                            .clamp(1, 64),
                        alignment: css::parse(&attribute(cell, "style"))
                            .alignment
                            .or_else(|| css::alignment(&attribute(cell, "align"))),
                        header: cell.element_name().as_deref() == Some("th"),
                    }
                })
                .collect()
        })
        .collect();
    let cols = cells
        .iter()
        .map(|row| row.iter().map(|cell| cell.colspan).sum())
        .max()
        .unwrap_or(0);
    if cols == 0 {
        return Vec::new();
    }
    super::render_table(&cells, &vec![pulldown_cmark::Alignment::None; cols], width)
}
