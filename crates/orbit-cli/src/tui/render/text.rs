//! Safe terminal text, ANSI styles and clipped previews.
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::tui::{
    markdown::LinkedLine,
    theme::{ansi_color, desaturate_ansi_color},
};

pub(in crate::tui) fn compact_preview(text: &str, limit: usize) -> String {
    let text = safe_output(text);
    let mut chars = text.chars();
    let mut result: String = chars.by_ref().take(limit).collect();

    if chars.next().is_some() {
        result.push('…');
    }

    result
}

pub(in crate::tui) fn parse_ansi_to_spans(text: &str) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut current_style = Style::default();
    let mut chars = text.chars().peekable();
    let mut current_chunk = String::new();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            match chars.peek().copied() {
                Some('[') => {
                    chars.next();
                    let mut seq = String::new();
                    while let Some(&c) = chars.peek() {
                        chars.next();
                        if c.is_ascii_alphabetic() {
                            seq.push(c);
                            break;
                        }
                        seq.push(c);
                    }
                    if seq.ends_with('m') {
                        if !current_chunk.is_empty() {
                            spans.push(Span::styled(current_chunk.clone(), current_style));
                            current_chunk.clear();
                        }
                        let code_part = &seq[..seq.len() - 1];
                        if code_part.is_empty() || code_part == "0" {
                            current_style = Style::default();
                        } else {
                            for code in code_part.split(';') {
                                match code.trim().parse::<u16>() {
                                    Ok(0) => current_style = Style::default(),
                                    Ok(1) => {
                                        current_style = current_style.add_modifier(Modifier::BOLD)
                                    }
                                    Ok(2) => {
                                        current_style = current_style.add_modifier(Modifier::DIM)
                                    }
                                    Ok(3) => {
                                        current_style = current_style.add_modifier(Modifier::ITALIC)
                                    }
                                    Ok(4) => {
                                        current_style =
                                            current_style.add_modifier(Modifier::UNDERLINED)
                                    }
                                    Ok(39) => current_style = current_style.fg(Color::Reset),
                                    Ok(c) => {
                                        if let Some(color) = ansi_color(c) {
                                            current_style =
                                                current_style.fg(desaturate_ansi_color(color));
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                    continue;
                }
                Some('(' | ')' | '*' | '+') => {
                    chars.next();
                    chars.next();
                    continue;
                }
                Some(']') => {
                    chars.next();
                    while let Some(&c) = chars.peek() {
                        chars.next();
                        if c == '\x07' || c == '\x1b' {
                            if c == '\x1b' && chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                    continue;
                }
                Some(_) => {
                    chars.next();
                    continue;
                }
                None => continue,
            }
        }

        if ch == '\t' {
            current_chunk.push_str("    ");
        } else if ch != '\r' {
            current_chunk.push(ch);
        }
    }

    if !current_chunk.is_empty() {
        spans.push(Span::styled(current_chunk, current_style));
    }

    if spans.is_empty() {
        vec![Span::raw("")]
    } else {
        spans
    }
}

pub(in crate::tui) fn sanitize_line(line: &mut Line<'_>) {
    for span in &mut line.spans {
        span.content = safe_output(&span.content).into();
    }
}

pub(in crate::tui) fn sanitize_linked_line(linked: &mut LinkedLine) {
    let original = linked.text();
    if !original.chars().any(|c| c.is_control() && c != '\n') {
        return;
    }
    // Semantic display math can span several lines. Layout splits these into
    // physical rows; retain breaks while escaping other terminal controls.
    let offset = |byte: usize| safe_multiline_output(&original[..byte]).len();
    for link in &mut linked.links {
        link.start = offset(link.start);
        link.end = offset(link.end);
    }
    for range in linked
        .decorations
        .iter_mut()
        .chain(linked.media.iter_mut().map(|t| &mut t.range))
    {
        range.start = offset(range.start);
        range.end = offset(range.end);
    }
    for span in &mut linked.line.spans {
        span.content = safe_multiline_output(&span.content).into();
    }
}

pub(in crate::tui) fn safe_multiline_output(text: &str) -> String {
    text.split('\n')
        .map(safe_output)
        .collect::<Vec<_>>()
        .join("\n")
}

pub(in crate::tui) fn safe_output(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c == '\t' {
                "    ".into()
            } else if c.is_control() {
                c.escape_default().to_string()
            } else {
                c.to_string()
            }
        })
        .collect()
}
