use super::{Identifier, Source, language};
use regex::Regex;
use std::{path::Path, sync::LazyLock};
use tree_sitter::{Node, Parser};

pub(super) fn analyze(path: &Path, source: &str) -> Source {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    let language = match ext {
        "rs" => Some(tree_sitter_rust::LANGUAGE.into()),
        "py" => Some(tree_sitter_python::LANGUAGE.into()),
        "ts" => Some(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
        "tsx" => Some(tree_sitter_typescript::LANGUAGE_TSX.into()),
        "js" | "jsx" | "mjs" | "cjs" => Some(tree_sitter_javascript::LANGUAGE.into()),
        "go" => Some(tree_sitter_go::LANGUAGE.into()),
        "c" | "h" => Some(tree_sitter_c::LANGUAGE.into()),
        "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => Some(tree_sitter_cpp::LANGUAGE.into()),
        _ => None,
    };
    let mut parser = Parser::new();
    let tree = language
        .and_then(|language| parser.set_language(&language).ok())
        .and_then(|()| parser.parse(source, None));
    let mut symbols = match tree.as_ref() {
        Some(tree) => match ext {
            "rs" => extract_rust_symbols(tree, source),
            "py" => extract_python_symbols(tree, source),
            "go" => extract_go_symbols(tree, source),
            "c" | "h" | "cc" | "cpp" | "cxx" | "hh" | "hpp" | "hxx" => {
                extract_c_symbols(tree, source)
            }
            _ => extract_ts_js_symbols(tree, source),
        },
        None => extract_fallback_symbols(path, source),
    };
    let mut identifiers = Vec::new();
    let mut truncated = symbols.len() > 1_024;
    symbols.truncate(1_024);
    if let Some(tree) = &tree {
        let mut pending = vec![tree.root_node()];
        let mut seen = std::collections::BTreeSet::new();
        while let Some(node) = pending.pop() {
            let kind = node.kind();
            if kind.contains("comment")
                || matches!(
                    kind,
                    "string"
                        | "string_literal"
                        | "interpreted_string_literal"
                        | "raw_string_literal"
                        | "template_string"
                )
            {
                continue;
            }
            if kind.ends_with("identifier") {
                let mut declaration = node;
                // Follow only the named declarator chain, never a function's
                // body or parameter list, to distinguish C/C++ declarations.
                while let Some(parent) = declaration.parent().filter(|parent| {
                    parent.kind() == "qualified_identifier"
                        && parent.child_by_field_name("name") == Some(declaration)
                }) {
                    declaration = parent;
                }
                let declaration_name = declaration
                    .parent()
                    .is_some_and(|parent| {
                        let kind = parent.kind();
                        ((kind.ends_with("_item")
                            || kind.ends_with("_definition")
                            || kind.ends_with("_declaration")
                            || matches!(
                                kind,
                                "variable_declarator"
                                    | "type_spec"
                                    | "method_signature"
                                    | "struct_specifier"
                                    | "class_specifier"
                                    | "enum_specifier"
                                    | "union_specifier"
                            ))
                            && parent.child_by_field_name("name") == Some(declaration))
                            || (matches!(kind, "function_declarator" | "type_definition")
                                && parent.child_by_field_name("declarator") == Some(declaration))
                    });
                if declaration_name {
                    continue;
                }
                let name = node
                    .utf8_text(source.as_bytes())
                    .unwrap_or_default();
                let line = node.start_position().row + 1;
                if name.len() <= 128 && seen.insert((name.to_owned(), line)) {
                    identifiers.push(Identifier {
                        name: name.to_owned(),
                        line,
                    });
                    if identifiers.len() >= 8_192 {
                        truncated = true;
                        break;
                    }
                }
            }
            let mut cursor = node.walk();
            pending.extend(node.named_children(&mut cursor));
        }
    }
    Source {
        symbols,
        identifiers,
        parsed: tree.is_some(),
        truncated,
    }
}

#[cfg(test)]
pub(crate) fn extract_symbols(path: &Path, source: &str) -> Vec<AstSymbol> {
    analyze(path, source).symbols
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AstSymbol {
    pub name: String,
    pub kind: String,
    pub line: usize,
    pub signature: String,
}

fn clean_signature(sig: &str) -> String {
    sig.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(512)
        .collect()
}

fn extract_c_symbols(tree: &tree_sitter::Tree, source: &str) -> Vec<AstSymbol> {
    let mut symbols = Vec::new();
    let mut pending = vec![tree.root_node()];
    while let Some(node) = pending.pop() {
        let (name, kind) = match node.kind() {
            "function_declarator" => (
                node.child_by_field_name("declarator")
                    .filter(|n| {
                        matches!(
                            n.kind(),
                            "identifier"
                                | "field_identifier"
                                | "qualified_identifier"
                                | "destructor_name"
                                | "operator_name"
                        )
                    }),
                "function",
            ),
            "struct_specifier" => (node.child_by_field_name("name"), "struct"),
            "class_specifier" => (node.child_by_field_name("name"), "class"),
            "union_specifier" => (node.child_by_field_name("name"), "union"),
            "enum_specifier" => (node.child_by_field_name("name"), "enum"),
            "type_definition" => (node.child_by_field_name("declarator"), "type"),
            _ => (None, ""),
        };
        if let Some(mut name) = name {
            // A qualified method uses its terminal name for cross-file hints.
            while let Some(inner) = name
                .child_by_field_name("name")
                .or_else(|| name.child_by_field_name("declarator"))
            {
                name = inner;
            }
            let owner = if kind == "function" {
                let mut owner = node;
                while let Some(parent) = owner.parent().filter(|p| {
                    !matches!(
                        p.kind(),
                        "translation_unit"
                            | "field_declaration_list"
                            | "declaration_list"
                            | "namespace_definition"
                            | "template_declaration"
                    )
                }) {
                    owner = parent;
                    if matches!(
                        owner.kind(),
                        "declaration" | "field_declaration" | "function_definition"
                    ) {
                        break;
                    }
                }
                owner
            } else {
                node
            };
            let end = owner
                .child_by_field_name("body")
                .map_or(owner.end_byte(), |body| body.start_byte());
            symbols.push(AstSymbol {
                name: name
                    .utf8_text(source.as_bytes())
                    .unwrap_or_default()
                    .into(),
                kind: kind.into(),
                line: name.start_position().row + 1,
                signature: clean_signature(&source[owner.start_byte()..end]),
            });
            if symbols.len() > 1_024 {
                break;
            }
        }
        if node.kind() != "compound_statement" {
            let mut cursor = node.walk();
            pending.extend(node.named_children(&mut cursor));
        }
    }
    symbols.sort_by_key(|s| s.line);
    symbols
}

fn extract_rust_symbols(tree: &tree_sitter::Tree, source: &str) -> Vec<AstSymbol> {
    let root = tree.root_node();
    let bytes = source.as_bytes();
    let mut symbols = Vec::new();

    let mut cursor = root.walk();
    for node in root.children(&mut cursor) {
        match node.kind() {
            "function_item" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = name_node
                        .utf8_text(bytes)
                        .unwrap_or_default();
                    let sig_end = node
                        .child_by_field_name("body")
                        .map(|b| b.start_byte())
                        .unwrap_or_else(|| node.end_byte());
                    let mut start = node.start_byte();
                    let mut item_cursor = node.walk();
                    for child in node.children(&mut item_cursor) {
                        if child.kind() == "attribute_item" {
                            start = child.end_byte();
                        } else {
                            break;
                        }
                    }

                    let raw_sig = source
                        .get(start..sig_end)
                        .unwrap_or("")
                        .trim_end_matches(';')
                        .trim();
                    let signature = clean_signature(raw_sig);
                    symbols.push(AstSymbol {
                        name: name.to_string(),
                        kind: "function".into(),
                        line: node.start_position().row + 1,
                        signature,
                    });
                }
            }

            "struct_item" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = name_node
                        .utf8_text(bytes)
                        .unwrap_or_default();
                    let sig_end = node
                        .child_by_field_name("body")
                        .map(|b| b.start_byte())
                        .unwrap_or_else(|| node.end_byte());
                    let mut start = node.start_byte();
                    let mut item_cursor = node.walk();
                    for child in node.children(&mut item_cursor) {
                        if child.kind() == "attribute_item" {
                            start = child.end_byte();
                        } else {
                            break;
                        }
                    }

                    let raw_sig = source
                        .get(start..sig_end)
                        .unwrap_or("")
                        .trim_end_matches(';')
                        .trim();
                    let signature = clean_signature(raw_sig);
                    symbols.push(AstSymbol {
                        name: name.to_string(),
                        kind: "struct".into(),
                        line: node.start_position().row + 1,
                        signature,
                    });
                }
            }

            "enum_item" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = name_node
                        .utf8_text(bytes)
                        .unwrap_or_default();
                    let sig_end = node
                        .child_by_field_name("body")
                        .map(|b| b.start_byte())
                        .unwrap_or_else(|| node.end_byte());
                    let mut start = node.start_byte();
                    let mut item_cursor = node.walk();
                    for child in node.children(&mut item_cursor) {
                        if child.kind() == "attribute_item" {
                            start = child.end_byte();
                        } else {
                            break;
                        }
                    }

                    let raw_sig = source
                        .get(start..sig_end)
                        .unwrap_or("")
                        .trim_end_matches(';')
                        .trim();
                    let signature = clean_signature(raw_sig);
                    symbols.push(AstSymbol {
                        name: name.to_string(),
                        kind: "enum".into(),
                        line: node.start_position().row + 1,
                        signature,
                    });
                }
            }

            "trait_item" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = name_node
                        .utf8_text(bytes)
                        .unwrap_or_default();
                    let sig_end = node
                        .child_by_field_name("body")
                        .map(|b| b.start_byte())
                        .unwrap_or_else(|| node.end_byte());
                    let mut start = node.start_byte();
                    let mut item_cursor = node.walk();
                    for child in node.children(&mut item_cursor) {
                        if child.kind() == "attribute_item" {
                            start = child.end_byte();
                        } else {
                            break;
                        }
                    }

                    let raw_sig = source
                        .get(start..sig_end)
                        .unwrap_or("")
                        .trim_end_matches(';')
                        .trim();
                    let signature = clean_signature(raw_sig);
                    symbols.push(AstSymbol {
                        name: name.to_string(),
                        kind: "trait".into(),
                        line: node.start_position().row + 1,
                        signature,
                    });
                }
            }

            "type_item" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = name_node
                        .utf8_text(bytes)
                        .unwrap_or_default();
                    let raw_sig = source
                        .get(node.start_byte()..node.end_byte())
                        .unwrap_or("")
                        .trim_end_matches(';')
                        .trim();
                    let signature = clean_signature(raw_sig);
                    symbols.push(AstSymbol {
                        name: name.to_string(),
                        kind: "type".into(),
                        line: node.start_position().row + 1,
                        signature,
                    });
                }
            }

            "impl_item" => {
                let body_node = node
                    .child_by_field_name("body")
                    .or_else(|| {
                        let mut c = node.walk();
                        node.children(&mut c)
                            .find(|n| n.kind() == "declaration_list")
                    });

                if let Some(body) = body_node {
                    let mut body_cursor = body.walk();
                    for child in body.children(&mut body_cursor) {
                        if child.kind() == "function_item"
                            && let Some(name_node) = child.child_by_field_name("name")
                        {
                            let name = name_node
                                .utf8_text(bytes)
                                .unwrap_or_default();
                            let sig_end = child
                                .child_by_field_name("body")
                                .map(|b| b.start_byte())
                                .unwrap_or_else(|| child.end_byte());
                            let mut start = child.start_byte();
                            let mut item_cursor = child.walk();
                            for c in child.children(&mut item_cursor) {
                                if c.kind() == "attribute_item" {
                                    start = c.end_byte();
                                } else {
                                    break;
                                }
                            }

                            let raw_sig = source
                                .get(start..sig_end)
                                .unwrap_or("")
                                .trim_end_matches(';')
                                .trim();
                            let signature = clean_signature(raw_sig);
                            symbols.push(AstSymbol {
                                name: name.to_string(),
                                kind: "method".into(),
                                line: child.start_position().row + 1,
                                signature,
                            });
                        }
                    }
                }
            }

            _ => {}
        }
    }

    symbols
}

fn extract_python_symbols(tree: &tree_sitter::Tree, source: &str) -> Vec<AstSymbol> {
    let root = tree.root_node();
    let bytes = source.as_bytes();
    let mut symbols = Vec::new();

    fn process_py_node(
        node: Node,
        bytes: &[u8],
        source: &str,
        is_method: bool,
        symbols: &mut Vec<AstSymbol>,
    ) {
        match node.kind() {
            "decorated_definition" => {
                let def_node = node
                    .child_by_field_name("definition")
                    .or_else(|| {
                        let mut c = node.walk();
                        node.children(&mut c).find(|n| {
                            n.kind() == "function_definition" || n.kind() == "class_definition"
                        })
                    });
                if let Some(def) = def_node {
                    process_py_node(def, bytes, source, is_method, symbols);
                }
            }

            "function_definition" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = name_node
                        .utf8_text(bytes)
                        .unwrap_or_default();
                    let sig_end = node
                        .child_by_field_name("body")
                        .map(|b| b.start_byte())
                        .unwrap_or_else(|| node.end_byte());
                    let raw_sig = source
                        .get(node.start_byte()..sig_end)
                        .unwrap_or("")
                        .trim()
                        .trim_end_matches(':')
                        .trim();

                    symbols.push(AstSymbol {
                        name: name.to_string(),
                        kind: if is_method {
                            "method".into()
                        } else {
                            "function".into()
                        },
                        line: node.start_position().row + 1,
                        signature: clean_signature(raw_sig),
                    });
                }
            }

            "class_definition" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = name_node
                        .utf8_text(bytes)
                        .unwrap_or_default();
                    let sig_end = node
                        .child_by_field_name("body")
                        .map(|b| b.start_byte())
                        .unwrap_or_else(|| node.end_byte());
                    let raw_sig = source
                        .get(node.start_byte()..sig_end)
                        .unwrap_or("")
                        .trim()
                        .trim_end_matches(':')
                        .trim();

                    symbols.push(AstSymbol {
                        name: name.to_string(),
                        kind: "class".into(),
                        line: node.start_position().row + 1,
                        signature: clean_signature(raw_sig),
                    });
                    if let Some(body) = node.child_by_field_name("body") {
                        let mut body_cursor = body.walk();
                        for child in body.children(&mut body_cursor) {
                            process_py_node(child, bytes, source, true, symbols);
                        }
                    }
                }
            }

            _ => {}
        }
    }

    let mut cursor = root.walk();
    for node in root.children(&mut cursor) {
        process_py_node(node, bytes, source, false, &mut symbols);
    }
    symbols
}

fn extract_ts_js_symbols(tree: &tree_sitter::Tree, source: &str) -> Vec<AstSymbol> {
    let root = tree.root_node();
    let bytes = source.as_bytes();
    let mut symbols = Vec::new();

    fn process_js_node(
        node: Node,
        bytes: &[u8],
        source: &str,
        is_export: bool,
        symbols: &mut Vec<AstSymbol>,
    ) {
        match node.kind() {
            "export_statement" => {
                let decl = node
                    .child_by_field_name("declaration")
                    .or_else(|| {
                        let mut c = node.walk();
                        node.children(&mut c).find(|n| {
                            matches!(
                                n.kind(),
                                "function_declaration"
                                    | "class_declaration"
                                    | "interface_declaration"
                                    | "type_alias_declaration"
                                    | "lexical_declaration"
                            )
                        })
                    });
                if let Some(decl) = decl {
                    process_js_node(decl, bytes, source, true, symbols);
                }
            }

            "function_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = name_node
                        .utf8_text(bytes)
                        .unwrap_or_default();
                    let sig_end = node
                        .child_by_field_name("body")
                        .map(|b| b.start_byte())
                        .unwrap_or_else(|| node.end_byte());
                    let raw_sig = source
                        .get(node.start_byte()..sig_end)
                        .unwrap_or("")
                        .trim();
                    let sig = clean_signature(raw_sig);
                    let signature = if is_export && !sig.starts_with("export") {
                        format!("export {sig}")
                    } else {
                        sig
                    };

                    symbols.push(AstSymbol {
                        name: name.to_string(),
                        kind: "function".into(),
                        line: node.start_position().row + 1,
                        signature,
                    });
                }
            }

            "class_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = name_node
                        .utf8_text(bytes)
                        .unwrap_or_default();
                    let sig_end = node
                        .child_by_field_name("body")
                        .map(|b| b.start_byte())
                        .unwrap_or_else(|| node.end_byte());
                    let raw_sig = source
                        .get(node.start_byte()..sig_end)
                        .unwrap_or("")
                        .trim();
                    let sig = clean_signature(raw_sig);
                    let signature = if is_export && !sig.starts_with("export") {
                        format!("export {sig}")
                    } else {
                        sig
                    };

                    symbols.push(AstSymbol {
                        name: name.to_string(),
                        kind: "class".into(),
                        line: node.start_position().row + 1,
                        signature,
                    });
                    if let Some(body) = node.child_by_field_name("body") {
                        let mut body_cursor = body.walk();
                        for child in body.children(&mut body_cursor) {
                            if child.kind() == "method_definition"
                                && let Some(m_name) = child.child_by_field_name("name")
                            {
                                let m_sig_end = child
                                    .child_by_field_name("body")
                                    .map(|b| b.start_byte())
                                    .unwrap_or_else(|| child.end_byte());
                                let m_raw = source
                                    .get(child.start_byte()..m_sig_end)
                                    .unwrap_or("")
                                    .trim();
                                symbols.push(AstSymbol {
                                    name: m_name
                                        .utf8_text(bytes)
                                        .unwrap_or_default()
                                        .to_string(),
                                    kind: "method".into(),
                                    line: child.start_position().row + 1,
                                    signature: clean_signature(m_raw),
                                });
                            }
                        }
                    }
                }
            }

            "interface_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = name_node
                        .utf8_text(bytes)
                        .unwrap_or_default();
                    let sig_end = node
                        .child_by_field_name("body")
                        .map(|b| b.start_byte())
                        .unwrap_or_else(|| node.end_byte());
                    let raw_sig = source
                        .get(node.start_byte()..sig_end)
                        .unwrap_or("")
                        .trim();
                    let sig = clean_signature(raw_sig);
                    let signature = if is_export && !sig.starts_with("export") {
                        format!("export {sig}")
                    } else {
                        sig
                    };

                    symbols.push(AstSymbol {
                        name: name.to_string(),
                        kind: "interface".into(),
                        line: node.start_position().row + 1,
                        signature,
                    });
                }
            }

            "type_alias_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = name_node
                        .utf8_text(bytes)
                        .unwrap_or_default();
                    let raw_sig = source
                        .get(node.start_byte()..node.end_byte())
                        .unwrap_or("")
                        .trim()
                        .trim_end_matches(';')
                        .trim();
                    let sig = clean_signature(raw_sig);
                    let signature = if is_export && !sig.starts_with("export") {
                        format!("export {sig}")
                    } else {
                        sig
                    };

                    symbols.push(AstSymbol {
                        name: name.to_string(),
                        kind: "type".into(),
                        line: node.start_position().row + 1,
                        signature,
                    });
                }
            }

            "lexical_declaration" => {
                let mut decl_cursor = node.walk();
                for child in node.children(&mut decl_cursor) {
                    if child.kind() == "variable_declarator"
                        && let Some(name_node) = child.child_by_field_name("name")
                    {
                        let name = name_node
                            .utf8_text(bytes)
                            .unwrap_or_default();
                        let val_start = child
                            .child_by_field_name("value")
                            .map(|v| v.start_byte())
                            .unwrap_or_else(|| child.end_byte());
                        let raw_sig = source
                            .get(node.start_byte()..val_start)
                            .unwrap_or("")
                            .trim()
                            .trim_end_matches('=')
                            .trim();
                        let sig = clean_signature(raw_sig);
                        let signature = if is_export && !sig.starts_with("export") {
                            format!("export {sig}")
                        } else {
                            sig
                        };

                        symbols.push(AstSymbol {
                            name: name.to_string(),
                            kind: "variable".into(),
                            line: child.start_position().row + 1,
                            signature,
                        });
                    }
                }
            }

            _ => {}
        }
    }

    let mut cursor = root.walk();
    for node in root.children(&mut cursor) {
        process_js_node(node, bytes, source, false, &mut symbols);
    }
    symbols
}

fn extract_go_symbols(tree: &tree_sitter::Tree, source: &str) -> Vec<AstSymbol> {
    let root = tree.root_node();
    let bytes = source.as_bytes();
    let mut symbols = Vec::new();

    let mut cursor = root.walk();
    for node in root.children(&mut cursor) {
        match node.kind() {
            "function_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = name_node
                        .utf8_text(bytes)
                        .unwrap_or_default();
                    let sig_end = node
                        .child_by_field_name("body")
                        .map(|b| b.start_byte())
                        .unwrap_or_else(|| node.end_byte());
                    let raw_sig = source
                        .get(node.start_byte()..sig_end)
                        .unwrap_or("")
                        .trim();
                    symbols.push(AstSymbol {
                        name: name.to_string(),
                        kind: "function".into(),
                        line: node.start_position().row + 1,
                        signature: clean_signature(raw_sig),
                    });
                }
            }

            "method_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = name_node
                        .utf8_text(bytes)
                        .unwrap_or_default();
                    let sig_end = node
                        .child_by_field_name("body")
                        .map(|b| b.start_byte())
                        .unwrap_or_else(|| node.end_byte());
                    let raw_sig = source
                        .get(node.start_byte()..sig_end)
                        .unwrap_or("")
                        .trim();
                    symbols.push(AstSymbol {
                        name: name.to_string(),
                        kind: "method".into(),
                        line: node.start_position().row + 1,
                        signature: clean_signature(raw_sig),
                    });
                }
            }

            "type_declaration" => {
                let mut type_cursor = node.walk();
                for child in node.children(&mut type_cursor) {
                    if child.kind() == "type_spec"
                        && let Some(name_node) = child.child_by_field_name("name")
                    {
                        let name = name_node
                            .utf8_text(bytes)
                            .unwrap_or_default();
                        let type_node = child.child_by_field_name("type");
                        let kind = match type_node.map(|t| t.kind()) {
                            Some("struct_type") => "struct",
                            Some("interface_type") => "interface",
                            _ => "type",
                        };
                        let sig_end = type_node
                            .and_then(|t| {
                                let mut c = t.walk();
                                t.children(&mut c).find(|n| {
                                    n.kind() == "field_declaration_list"
                                        || n.kind() == "method_spec_list"
                                        || n.kind() == "block"
                                })
                            })
                            .map(|b| b.start_byte())
                            .unwrap_or_else(|| child.end_byte());
                        let raw_sig = source
                            .get(node.start_byte()..sig_end)
                            .unwrap_or("")
                            .trim();
                        symbols.push(AstSymbol {
                            name: name.to_string(),
                            kind: kind.into(),
                            line: child.start_position().row + 1,
                            signature: clean_signature(raw_sig),
                        });
                    }
                }
            }

            _ => {}
        }
    }

    symbols
}

fn extract_fallback_symbols(path: &Path, text: &str) -> Vec<AstSymbol> {
    static FALLBACK: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:export\s+)?(?:default\s+)?(?:async\s+)?(?:unsafe\s+)?(?:fn|func|def|class|struct|enum|trait|interface|type)\s+([A-Za-z_$][A-Za-z0-9_$]*)").unwrap()
    });

    if language(path).is_none() {
        return Vec::new();
    }

    text.lines()
        .enumerate()
        .filter_map(|(index, line)| {
            FALLBACK
                .captures(line)
                .and_then(|c| c.get(1))
                .map(|m| {
                    let name = m.as_str().to_string();
                    let sig = clean_signature(
                        line.trim_end_matches('{')
                            .trim_end_matches(':')
                            .trim(),
                    );
                    AstSymbol {
                        name,
                        kind: "symbol".into(),
                        line: index + 1,
                        signature: sig,
                    }
                })
        })
        .take(128)
        .collect()
}
