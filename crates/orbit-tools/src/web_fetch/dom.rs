//! DOM primitives shared by search providers; provider selection stays separate.
use html2text::{Element, Handle};

const TEXT_PROJECTION_WIDTH: usize = 4_096;

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
