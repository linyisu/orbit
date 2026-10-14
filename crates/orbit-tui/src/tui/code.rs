use std::ops::Range;

use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use syntect::{
    easy::{HighlightLines, ScopeRegionIterator},
    highlighting::{HighlightIterator, HighlightState, Highlighter},
    parsing::ParseState,
};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use super::{
    languages,
    render::safe_output,
    theme::{CODE_THEME, THEME, syntax_modifiers},
};

#[derive(Clone, Copy, Debug)]
pub(super) enum Change {
    Added,
    Removed,
}

impl Change {
    fn style(self) -> Style {
        let (foreground, background) = match self {
            Self::Added => (THEME.diff_added, THEME.diff_added_bg),
            Self::Removed => (THEME.diff_removed, THEME.diff_removed_bg),
        };
        Style::default()
            .fg(foreground)
            .bg(background)
    }

    fn marker(self) -> char {
        match self {
            Self::Added => '+',
            Self::Removed => '-',
        }
    }
}

#[derive(Default)]
pub(super) enum Gutter {
    #[default]
    None,
    Indent(usize),
    Rail {
        indent: usize,
        color: Color,
    },
    Number {
        number: u64,
        digits: usize,
        indent: usize,
    },
    Diff {
        number: u64,
        digits: usize,
        indent: usize,
    },
}

impl Gutter {
    fn span(&self, continuation: bool, change: Option<Change>) -> Span<'static> {
        match self {
            Self::None => Span::raw(""),
            Self::Indent(indent) => Span::raw(" ".repeat(*indent)),
            Self::Rail { indent, color } => Span::styled(
                format!("{}│ ", " ".repeat(*indent)),
                Style::default().fg(*color),
            ),

            Self::Number {
                number,
                digits,
                indent,
            }
            | Self::Diff {
                number,
                digits,
                indent,
            } => {
                let sign = if continuation {
                    ' '
                } else {
                    change.map_or(' ', Change::marker)
                };
                let sign = if matches!(self, Self::Diff { .. }) {
                    sign.to_string()
                } else {
                    String::new()
                };
                let number = if continuation {
                    " ".repeat(*digits)
                } else {
                    format!("{number:>digits$}")
                };
                Span::styled(
                    format!("{}{sign}{number} │ ", " ".repeat(*indent)),
                    change.map_or_else(|| Style::default().fg(THEME.line_number), Change::style),
                )
            }
        }
    }
}

#[derive(Default)]
pub(super) enum Overflow {
    #[default]
    Wrap,
    Clip,
}

#[derive(Default)]
pub(super) struct Options {
    pub gutter: Gutter,
    pub width: Option<u16>,
    pub overflow: Overflow,
    pub change: Option<Change>,
    /// UTF-8 byte ranges in the original source, before escaping terminal controls.
    pub highlights: Vec<Range<usize>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Renderer {
    state: (HighlightState, ParseState),
}

impl Renderer {
    pub fn new(label: &str) -> Self {
        Self::with_syntax(languages::syntax_for_label(label))
    }

    pub fn for_path(path: &str) -> Self {
        Self::with_syntax(languages::syntax_for_path(path))
    }

    fn with_syntax(syntax: Option<&syntect::parsing::SyntaxReference>) -> Self {
        let syntax = syntax.unwrap_or_else(|| languages::SYNTAXES.find_syntax_plain_text());
        Self {
            state: HighlightLines::new(syntax, &CODE_THEME).state(),
        }
    }

    /// Parse contiguous lines once, then apply the same layout and overlays in every code view.
    pub fn render(&mut self, source: &str, options: Options) -> Vec<Line<'static>> {
        self.render_linked(source, options)
            .into_iter()
            .map(Line::from)
            .collect()
    }

    pub fn render_linked(
        &mut self,
        source: &str,
        mut options: Options,
    ) -> Vec<super::markdown::LinkedLine> {
        let (text, highlights) = marked_source(source, &options.highlights);
        options.highlights = highlights;
        let line = self.line(&text);
        render_line_linked(&line, options)
    }

    fn line(&mut self, text: &str) -> Line<'static> {
        let ops = match self
            .state
            .1
            .parse_line(text, &languages::SYNTAXES)
        {
            Ok(ops) => ops,
            Err(_) => return Line::from(text.to_owned()),
        };
        // Reuse the parser's scope operations for terminal effects. Syntect's
        // font flags lack strikethrough; scope effects compose nested emphasis.
        let mut scopes = self.state.0.path.clone();
        let mut modifiers = Vec::new();
        for (token, op) in ScopeRegionIterator::new(&ops, text) {
            if scopes.apply(op).is_err() {
                return Line::from(text.to_owned());
            }
            if !token.is_empty() {
                modifiers.push(syntax_modifiers(scopes.as_slice()));
            }
        }

        let highlighter = Highlighter::new(&CODE_THEME);
        Line::from(
            HighlightIterator::new(&mut self.state.0, &ops, text, &highlighter)
                .zip(modifiers)
                .map(|((style, token), modifiers)| {
                    let color = style.foreground;
                    Span::styled(
                        token.to_owned(),
                        Style::default()
                            .fg(Color::Rgb(color.r, color.g, color.b))
                            .add_modifier(modifiers),
                    )
                })
                .collect::<Vec<_>>(),
        )
    }
}

pub(super) fn file_lines(path: &str, content: &str) -> Vec<Line<'static>> {
    let mut renderer = Renderer::for_path(path);
    content
        .lines()
        .flat_map(|line| renderer.render(line, Options::default()))
        .collect()
}

/// Format already parsed, safely escaped source from a cached file or diff side.
pub(super) fn render_line(line: &Line<'static>, options: Options) -> Vec<Line<'static>> {
    render_line_linked(line, options)
        .into_iter()
        .map(Line::from)
        .collect()
}

fn render_line_linked(line: &Line<'static>, options: Options) -> Vec<super::markdown::LinkedLine> {
    let text = line.to_string();
    let links = super::markdown::plain_links(&text);
    let prefix = options
        .gutter
        .span(false, options.change);
    let Some(width) = options.width.map(usize::from) else {
        return vec![styled_row(
            line,
            &text,
            &options,
            &links,
            0..text.len(),
            false,
            false,
            false,
        )];
    };
    let available = width.saturating_sub(prefix.width());
    if available == 0 {
        let mut spans = vec![prefix];
        spans.extend(line.spans.iter().cloned());
        return vec![super::markdown::LinkedLine::chrome(clip_spans(
            &Line::from(spans),
            width,
        ))];
    }

    if matches!(options.overflow, Overflow::Clip) {
        let mut start = 0;
        if let Some(first) = options.highlights.first()
            && text
                .get(..first.end)
                .is_some_and(|text| text.width() >= available)
        {
            let before: Vec<_> = text
                .grapheme_indices(true)
                .take_while(|(offset, _)| *offset <= first.start)
                .collect();
            let mut context_width = 0;
            for (offset, glyph) in before.into_iter().rev() {
                if offset < first.start {
                    context_width += glyph.width();
                }
                if context_width > available / 4 {
                    break;
                }
                start = offset;
            }
        }

        let budget = available.saturating_sub(usize::from(start > 0));
        let (length, truncated) = clipped_length(&text[start..], budget);
        return vec![styled_row(
            line,
            &text,
            &options,
            &links,
            start..start + length,
            false,
            start > 0,
            truncated,
        )];
    }

    let (_, ranges) = super::transcript::wrap_line(line, available as u16);
    ranges
        .into_iter()
        .enumerate()
        .map(|(index, range)| {
            let oversized = text[range.clone()].width() > available;
            let range = if oversized {
                range.start..range.start
            } else {
                range
            };
            styled_row(
                line,
                &text,
                &options,
                &links,
                range,
                index > 0,
                false,
                oversized,
            )
        })
        .collect()
}

fn marked_source(source: &str, highlights: &[Range<usize>]) -> (String, Vec<Range<usize>>) {
    let mut text = String::new();
    let mut ranges = Vec::new();
    let mut offset = 0;
    for range in highlights {
        if range.start < offset || range.start >= range.end {
            continue;
        }
        let (Some(before), Some(hit)) =
            (source.get(offset..range.start), source.get(range.clone()))
        else {
            continue;
        };

        text.push_str(&safe_output(before));
        let start = text.len();
        text.push_str(&safe_output(hit));
        ranges.push(start..text.len());
        offset = range.end;
    }

    text.push_str(&safe_output(&source[offset..]));
    (text, ranges)
}

#[allow(clippy::too_many_arguments)]
fn styled_row(
    line: &Line<'static>,
    text: &str,
    options: &Options,
    links: &[super::markdown::LinkTarget],
    range: Range<usize>,
    continuation: bool,
    leading: bool,
    trailing: bool,
) -> super::markdown::LinkedLine {
    let prefix = options
        .gutter
        .span(continuation, options.change);
    let prefix_length = prefix.content.len();
    let offset = prefix_length + usize::from(leading) * "…".len();
    let links = links
        .iter()
        .filter_map(|link| link.slice(&range, offset))
        .collect();
    let mut spans = if prefix.content.is_empty() {
        Vec::new()
    } else {
        vec![prefix]
    };
    if leading {
        spans.push(Span::raw("…"));
    }
    let mut offset = 0;
    for span in &line.spans {
        let start = offset.max(range.start);
        offset += span.content.len();
        let end = offset.min(range.end);
        if start >= end {
            continue;
        }
        let style = options
            .change
            .map_or(span.style, |change| {
                span.style
                    .bg(change.style().bg.unwrap())
            });
        let mut position = start;
        for mark in &options.highlights {
            let mark_start = mark.start.max(position);
            let mark_end = mark.end.min(end);
            if mark_start >= mark_end {
                continue;
            }
            if position < mark_start {
                spans.push(Span::styled(text[position..mark_start].to_owned(), style));
            }
            spans.push(Span::styled(
                text[mark_start..mark_end].to_owned(),
                super::markdown::mark_style(style),
            ));
            position = mark_end;
        }

        if position < end {
            spans.push(Span::styled(text[position..end].to_owned(), style));
        }
    }

    if trailing {
        spans.push(Span::raw("…"));
    }
    super::markdown::LinkedLine {
        line: Line::from(spans),
        links,
        chrome: false,
        decorations: if prefix_length == 0 {
            Vec::new()
        } else {
            std::iter::once(0..prefix_length).collect()
        },
        ..Default::default()
    }
}

fn clipped_length(text: &str, width: usize) -> (usize, bool) {
    if text.width() <= width {
        return (text.len(), false);
    }
    let mut length = 0;
    let mut used = 0;
    for glyph in text.graphemes(true) {
        if used + glyph.width() > width.saturating_sub(1) {
            break;
        }
        length += glyph.len();
        used += glyph.width();
    }

    (length, width > 0)
}

fn clip_spans(line: &Line<'static>, width: usize) -> Line<'static> {
    let text = line.to_string();
    let (length, truncated) = clipped_length(&text, width);
    styled_row(
        line,
        &text,
        &Options::default(),
        &[],
        0..length,
        false,
        false,
        truncated,
    )
    .line
}

#[cfg(test)]
mod tests;
