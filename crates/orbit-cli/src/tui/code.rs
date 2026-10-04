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
        Style::default().fg(foreground).bg(background)
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
        let ops = match self.state.1.parse_line(text, &languages::SYNTAXES) {
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
    let prefix = options.gutter.span(false, options.change);
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
    let prefix = options.gutter.span(continuation, options.change);
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
        let style = options.change.map_or(span.style, |change| {
            span.style.bg(change.style().bg.unwrap())
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
mod tests {
    use ratatui::style::Modifier;

    use super::*;

    #[test]
    fn source_url_targets_survive_wrapping_clipping_and_numbered_gutters() {
        let url = "https://orbit.example/docs/rendering/";
        let source = format!("中文 [Orbit]({url})");
        for width in [16, 24, 70] {
            for clip in [false, true] {
                let lines = Renderer::new("markdown").render_linked(
                    &source,
                    Options {
                        gutter: Gutter::Number {
                            number: 42,
                            digits: 4,
                            indent: 4,
                        },
                        width: Some(width),
                        overflow: if clip { Overflow::Clip } else { Overflow::Wrap },
                        highlights: if clip {
                            let start = source.find(url).unwrap();
                            std::iter::once(start..start + url.len()).collect()
                        } else {
                            Vec::new()
                        },
                        ..Default::default()
                    },
                );
                assert!(
                    lines
                        .iter()
                        .all(|line| line.line.width() <= usize::from(width))
                );
                let links: Vec<_> = lines.iter().flat_map(|line| &line.links).collect();
                assert!(!links.is_empty(), "width {width}, clip {clip}");
                assert!(links.iter().all(|link| link.url == url));
                for line in lines {
                    let text = line.line.to_string();
                    for link in line.links {
                        let label = text.get(link.start..link.end).unwrap();
                        assert!(!label.is_empty() && !label.contains('│') && !label.contains('…'));
                    }
                }
            }
        }
    }

    fn style_at(line: &Line<'_>, token: &str) -> Style {
        let position = line.to_string().find(token).unwrap();
        let mut offset = 0;
        for span in &line.spans {
            offset += span.content.len();
            if offset > position {
                return span.style;
            }
        }
        panic!("missing token {token}");
    }

    fn highlight(renderer: &mut Renderer, source: &str) -> Line<'static> {
        let line = renderer.render(source, Options::default()).remove(0);
        assert_eq!(line.to_string(), source);
        line
    }

    #[test]
    fn shared_layout_preserves_unicode_colors_and_only_overlays_search_hits() {
        let source = "let orbit = \"中文👩‍💻e\u{301}\"; // Orbit 注释";
        let plain = highlight(&mut Renderer::new("rust"), source);
        let hit = source.find("中文").unwrap()..source.find(';').unwrap();
        for width in [0, 1, 7, 12, 24, 50] {
            for gutter in [
                Gutter::None,
                Gutter::Rail {
                    indent: 2,
                    color: THEME.accent,
                },
                Gutter::Number {
                    number: 1,
                    digits: 4,
                    indent: 4,
                },
                Gutter::Diff {
                    number: 1,
                    digits: 4,
                    indent: 0,
                },
            ] {
                let options = Options {
                    gutter,
                    width: Some(width),
                    ..Default::default()
                };
                let prepared = render_line(&plain, options);
                assert!(prepared.iter().all(|row| row.width() <= usize::from(width)));
            }
        }
        let options = || Options {
            gutter: Gutter::Number {
                number: 1,
                digits: 4,
                indent: 4,
            },
            width: Some(24),
            highlights: vec![hit.clone()],
            ..Default::default()
        };
        let searched = Renderer::new("rust").render(source, options());
        let prepared = render_line(&plain, options());
        assert_eq!(searched, prepared);
        let collected: String = searched
            .iter()
            .flat_map(|row| &row.spans[1..])
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(collected, source);
        let marked: String = searched
            .iter()
            .flat_map(|row| &row.spans)
            .filter(|span| {
                span.style
                    .add_modifier
                    .contains(ratatui::style::Modifier::UNDERLINED)
            })
            .map(|span| span.content.as_ref())
            .collect();
        assert_eq!(marked, &source[hit]);

        let mut renderer = Renderer::new("rust");
        renderer.render(
            "/*",
            Options {
                width: Some(0),
                ..Default::default()
            },
        );
        let comment = highlight(&mut renderer, "fn orbit() {}");
        assert!(
            comment
                .spans
                .iter()
                .all(|span| span.style.fg == Some(Color::Rgb(163, 173, 190)))
        );
    }

    #[test]
    fn diff_gutters_align_context_and_continuations_without_repeating_the_marker() {
        let source = "let orbit = \"中文 ABCDEFGHIJKLMNOP\";";
        let line = highlight(&mut Renderer::new("rust"), source);
        for change in [None, Some(Change::Added), Some(Change::Removed)] {
            let rows = render_line(
                &line,
                Options {
                    gutter: Gutter::Diff {
                        number: 12,
                        digits: 4,
                        indent: 2,
                    },
                    width: Some(23),
                    change,
                    ..Default::default()
                },
            );
            assert!(rows.len() > 1);
            assert!(rows.iter().all(|row| row.width() <= 23));
            assert_eq!(rows[0].spans[0].width(), rows[1].spans[0].width());
            assert_eq!(
                rows[0].spans[0].content.chars().nth(2).unwrap(),
                change.map_or(' ', Change::marker)
            );
            assert_eq!(rows[1].spans[0].content, "        │ ");
            let text: String = rows
                .iter()
                .flat_map(|row| &row.spans[1..])
                .map(|span| span.content.as_ref())
                .collect();
            assert_eq!(text, source);
            for span in rows.iter().flat_map(|row| &row.spans[1..]) {
                assert_eq!(span.style.bg, change.and_then(|change| change.style().bg));
            }
        }
    }

    #[test]
    fn markdown_source_colors_syntax_without_rendering_it() {
        let cases = [
            ("## Orbit 标题", "##", Color::Rgb(198, 120, 221)),
            ("### 数学公式", "数学", Color::Rgb(229, 192, 123)),
            ("---", "---", Color::Rgb(198, 120, 221)),
            ("***", "***", Color::Rgb(198, 120, 221)),
            ("___", "___", Color::Rgb(198, 120, 221)),
            ("**重点**", "重点", Color::Rgb(229, 192, 123)),
            ("*斜体*", "斜体", Color::Rgb(198, 120, 221)),
            ("`orbit`", "orbit", Color::Rgb(126, 205, 208)),
            (
                "[Orbit](https://orbit.test)",
                "https://orbit.test",
                Color::Rgb(97, 175, 239),
            ),
        ];
        let renderers = ["md", "markdown", "gfm", "commonmark", "language-MD"]
            .map(Renderer::new)
            .into_iter()
            .chain(["README.md", "docs/Orbit.MARKDOWN"].map(Renderer::for_path));
        for mut renderer in renderers {
            for (source, token, foreground) in cases {
                highlight(&mut renderer, "");
                let line = highlight(&mut renderer, source);
                assert_eq!(style_at(&line, token).fg, Some(foreground), "{source}");
            }
        }
    }

    #[test]
    fn markdown_source_keeps_symbols_and_composes_nested_effects_in_all_views() {
        let cases = [
            ("## Orbit 标题", "标题", Modifier::BOLD),
            ("**粗体**", "粗体", Modifier::BOLD),
            ("*斜体*", "斜体", Modifier::ITALIC),
            ("***粗斜体***", "粗斜体", Modifier::BOLD | Modifier::ITALIC),
            ("___粗斜体___", "粗斜体", Modifier::BOLD | Modifier::ITALIC),
            ("~~删除线~~", "删除线", Modifier::CROSSED_OUT),
            ("==高亮==", "高亮", Modifier::UNDERLINED),
            (
                "**粗体包含*斜体*部分**",
                "斜体",
                Modifier::BOLD | Modifier::ITALIC,
            ),
            (
                "~~**加粗的删除线**~~",
                "加粗的删除线",
                Modifier::BOLD | Modifier::CROSSED_OUT,
            ),
        ];
        for label in ["md", "markdown", "gfm", "commonmark", "language-MD"] {
            for (source, token, effects) in cases {
                let line = highlight(&mut Renderer::new(label), source);
                assert_eq!(style_at(&line, token).add_modifier, effects, "{source}");
                assert_eq!(file_lines("README.md", source), vec![line.clone()]);

                let mut fence = None;
                super::super::markdown::line("````markdown", 0, &mut fence);
                let fenced = super::super::markdown::line(source, 1, &mut fence);
                assert_eq!(fenced.to_string(), format!("  │ {source}"));
                assert_eq!(&fenced.spans[1..], line.spans.as_slice());
                assert!(fenced.spans[0].style.add_modifier.is_empty());
            }
        }
        let nested = highlight(&mut Renderer::new("md"), "**外部*内部*恢复** 结束");
        assert_eq!(style_at(&nested, "内部").fg, Some(THEME.md_bold_italic));
        assert_eq!(style_at(&nested, "恢复").add_modifier, Modifier::BOLD);
        assert!(style_at(&nested, "结束").add_modifier.is_empty());
        let strike = highlight(&mut Renderer::new("md"), "~~删除线~~");
        assert_eq!(style_at(&strike, "删除线").fg, Some(THEME.md_strikethrough));
    }

    #[test]
    fn source_effects_keep_literals_and_embedded_code_literal() {
        for source in [
            "`**字面量** *星号* ~~波浪线~~`",
            "`==字面量==`",
            r"\*字面量\* \~\~波浪线\~\~",
            r"\==字面量==",
            "== 未闭合",
            "== 两边有空格 ==",
            "===字面量===",
            "    **缩进代码**",
        ] {
            let line = highlight(&mut Renderer::new("md"), source);
            assert!(
                line.spans
                    .iter()
                    .all(|span| span.style.add_modifier.is_empty())
            );
        }
        let mut renderer = Renderer::new("md");
        highlight(&mut renderer, "```rust");
        for source in ["// **注释** ~~原样~~", "let text = \"***原样***\";"] {
            let line = highlight(&mut renderer, source);
            assert_eq!(line, highlight(&mut Renderer::new("rust"), source));
        }
        highlight(&mut renderer, "```");
        let emphasis = highlight(&mut renderer, "***恢复***");
        assert_eq!(
            style_at(&emphasis, "恢复").add_modifier,
            Modifier::BOLD | Modifier::ITALIC
        );
    }

    #[test]
    fn source_highlight_extension_reuses_inline_scopes_and_ends_at_its_delimiter() {
        let source = "==**重点** 与 `*代码*`== 普通";
        let line = highlight(&mut Renderer::new("md"), source);
        assert_eq!(
            style_at(&line, "重点").add_modifier,
            Modifier::BOLD | Modifier::UNDERLINED
        );
        assert_eq!(style_at(&line, "*代码*").add_modifier, Modifier::UNDERLINED);
        assert_eq!(style_at(&line, " 与 ").fg, Some(THEME.md_mark));
        assert!(style_at(&line, "普通").add_modifier.is_empty());
        let line = highlight(&mut Renderer::new("md"), "==表达式 a = b==");
        assert_eq!(style_at(&line, "a = b").add_modifier, Modifier::UNDERLINED);
    }

    #[test]
    fn source_effects_survive_wrapping_search_and_diff_overlays() {
        let source = "***中文强调片段*** 和 ~~删除片段~~";
        let start = source.find("强调").unwrap();
        let rows = Renderer::new("md").render(
            source,
            Options {
                gutter: Gutter::Number {
                    number: 7,
                    digits: 1,
                    indent: 0,
                },
                width: Some(18),
                change: Some(Change::Added),
                highlights: std::iter::once(start..start + "强调".len()).collect(),
                ..Default::default()
            },
        );
        let text: String = rows
            .iter()
            .flat_map(|row| &row.spans[1..])
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(text, source);
        assert!(rows.len() > 1);
        let emphasis = rows
            .iter()
            .flat_map(|row| &row.spans)
            .find(|span| span.content.contains("强调"))
            .unwrap();
        assert!(
            emphasis
                .style
                .add_modifier
                .contains(Modifier::BOLD | Modifier::ITALIC)
        );
        assert!(
            rows.iter()
                .all(|row| row.spans[0].style.add_modifier.is_empty())
        );
        assert!(
            rows.iter()
                .flat_map(|row| &row.spans[1..])
                .any(|span| span.style.add_modifier.contains(Modifier::CROSSED_OUT))
        );
    }

    #[test]
    fn markdown_math_reuses_latex_tokens_and_preserves_multiline_state() {
        let mut markdown = Renderer::new("md");
        let inline = highlight(&mut markdown, r"公式 $E = mc^2$；$e^{i\pi} + 1 = 0$。");
        assert_eq!(style_at(&inline, "$").fg, Some(Color::Rgb(198, 120, 221)));
        assert_eq!(style_at(&inline, "E").fg, Some(Color::Rgb(126, 205, 208)));
        assert_eq!(style_at(&inline, "2").fg, Some(Color::Rgb(209, 154, 102)));
        assert_eq!(
            style_at(&inline, r"\pi").fg,
            Some(Color::Rgb(198, 120, 221))
        );

        highlight(&mut markdown, "");
        let opening = highlight(&mut markdown, "$$");
        assert_eq!(style_at(&opening, "$$").fg, Some(Color::Rgb(198, 120, 221)));
        let mut replay = markdown.clone();
        let source = r"\int_{-\infty}^{\infty} e^{-x^2}\, dx = \sqrt{\pi}";
        let block = highlight(&mut markdown, source);
        assert_eq!(block, highlight(&mut replay, source));
        let mut latex = Renderer::new("latex");
        highlight(&mut latex, "$$");
        let latex = highlight(&mut latex, source);
        for token in [r"\int", r"\sqrt", r"\pi", "2"] {
            assert_eq!(style_at(&block, token), style_at(&latex, token), "{token}");
        }
        let closing = highlight(&mut markdown, "$$");
        assert_eq!(closing, highlight(&mut replay, "$$"));
        assert_eq!(
            highlight(&mut markdown, "Orbit 正文"),
            highlight(&mut Renderer::new("md"), "Orbit 正文")
        );

        let mut fence = None;
        let mut markdown = Renderer::new("md");
        super::super::markdown::line("````markdown", 0, &mut fence);
        for source in [
            "## Orbit 公式",
            r"公式 $x = \frac{1}{2}$",
            "$$",
            r"\begin{pmatrix} a & b \\ c & d \end{pmatrix}",
            "$$",
            "```rust",
            "// Orbit 注释",
            "let orbit = 42;",
            "```",
            "结束正文",
        ] {
            let fenced = super::super::markdown::line(source, 1, &mut fence);
            let raw = highlight(&mut markdown, source);
            assert_eq!(fenced.to_string(), format!("  │ {source}"));
            assert_eq!(&fenced.spans[1..], raw.spans.as_slice(), "{source}");
        }
        super::super::markdown::line("````", 0, &mut fence);
        assert!(fence.is_none());
    }

    #[test]
    fn markdown_math_preserves_literals_currency_and_embedded_code() {
        let foreground = Color::Rgb(225, 230, 240);
        for source in [
            r"字面量 \$x\$",
            "$5 and $10",
            "$unclosed",
            "$ spaced $",
            "    $x$",
        ] {
            let line = highlight(&mut Renderer::new("md"), source);
            assert_eq!(style_at(&line, "$").fg, Some(foreground), "{source}");
        }
        let raw = highlight(&mut Renderer::new("md"), "`$x$`");
        assert_eq!(style_at(&raw, "$").fg, Some(Color::Rgb(126, 205, 208)));

        let mut markdown = Renderer::new("md");
        highlight(&mut markdown, "```python");
        let literal = highlight(&mut markdown, "value = '$x$'");
        assert_eq!(style_at(&literal, "$").fg, Some(Color::Rgb(152, 195, 121)));
        let comment = highlight(&mut markdown, "# $$ comment");
        assert_eq!(style_at(&comment, "$$").fg, Some(Color::Rgb(163, 173, 190)));
        highlight(&mut markdown, "```");
        let formula = highlight(&mut markdown, "$x$");
        assert_eq!(style_at(&formula, "$").fg, Some(Color::Rgb(198, 120, 221)));
        highlight(&mut markdown, "");
        highlight(&mut markdown, "```text");
        let literal = highlight(&mut markdown, "$x$");
        assert_eq!(style_at(&literal, "$").fg, Some(foreground));
    }

    #[test]
    fn code_comments_share_a_readable_foreground_across_languages_and_previews() {
        for (label, path, source, token) in [
            ("rust", "src/orbit.rs", "// Orbit 注释", "//"),
            ("python", "orbit.py", "# Orbit 注释", "#"),
            ("bash", "orbit.sh", "# Orbit 注释", "#"),
            ("toml", "orbit.toml", "# Orbit 注释", "#"),
            ("javascript", "orbit.js", "// Orbit 注释", "//"),
            ("html", "orbit.html", "<!-- Orbit 注释 -->", "<!--"),
            ("sql", "orbit.sql", "-- Orbit 注释", "--"),
            ("vbnet", "orbit.vb", "' Orbit 注释", "'"),
            ("latex", "orbit.tex", "% Orbit 注释", "%"),
        ] {
            let line = highlight(&mut Renderer::new(label), source);
            assert_eq!(
                style_at(&line, token).fg,
                Some(Color::Rgb(163, 173, 190)),
                "{label}"
            );
            assert_eq!(file_lines(path, source), vec![line.clone()], "{path}");
            let mut fence = None;
            super::super::markdown::line(&format!("```{label}"), 0, &mut fence);
            let fenced = super::super::markdown::line(source, 1, &mut fence);
            assert_eq!(style_at(&fenced, token), style_at(&line, token), "{label}");
        }
        let mut rust = Renderer::new("rust");
        highlight(&mut rust, "/* Orbit 注释");
        let continuation = highlight(&mut rust, "中文内容 */");
        assert_eq!(
            style_at(&continuation, "中文").fg,
            Some(Color::Rgb(163, 173, 190))
        );
    }
}
