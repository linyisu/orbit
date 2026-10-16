//! DOM primitives shared by search providers; provider selection stays separate.
use html2text::{Element, Handle};

const TEXT_PROJECTION_WIDTH: usize = 4_096;

/// Prefer a unique semantic content root; ambiguous documents keep their full content.
pub(super) fn readable_html(source: &str) -> Result<String, super::ToolError> {
    let dom = html2text::config::plain()
        .parse_html(source.as_bytes())
        .map_err(super::failed)?;
    let mut pending = vec![dom.document.clone()];
    let mut main = Vec::new();
    let mut articles = Vec::new();
    while let Some(node) = pending.pop() {
        match node.element_name().as_deref() {
            Some("main") => main.push(node.clone()),
            Some("article") => articles.push(node.clone()),
            _ => {}
        }
        pending.extend(node.children.borrow().iter().cloned());
    }
    let selected = if main.len() == 1 {
        main.pop()
    } else if main.is_empty() && articles.len() == 1 {
        articles.pop()
    } else {
        None
    };
    if let Some(node) = selected {
        let mut html = Vec::new();
        node.serialize(&mut html)
            .map_err(super::failed)?;
        let content = html2text::from_read(html.as_slice(), 120).map_err(super::failed)?;
        if !content.trim().is_empty() {
            return Ok(content);
        }
    }
    html2text::from_read(source.as_bytes(), 120).map_err(super::failed)
}

pub(super) fn attribute(node: &Handle, key: &str) -> Option<String> {
    let Element { attrs, .. } = &node.data else {
        return None;
    };
    attrs
        .borrow()
        .iter()
        .find(|a| a.name.local.as_ref() == key)
        .map(|a| a.value.to_string())
}

pub(super) fn text(node: &Handle) -> String {
    let mut html = Vec::new();
    for child in node.children.borrow().iter() {
        if child.serialize(&mut html).is_err() {
            return String::new();
        }
    }

    html2text::config::plain_no_decorate()
        .string_from_read(html.as_slice(), TEXT_PROJECTION_WIDTH)
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
