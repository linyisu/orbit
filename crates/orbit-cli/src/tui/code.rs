use ratatui::{
    style::{Color, Style},
    text::{Line, Span},
};
use syntect::{easy::HighlightLines, highlighting::HighlightState, parsing::ParseState};

use super::{languages, render::safe_output, theme::CODE_THEME};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Highlighter {
    state: (HighlightState, ParseState),
}

impl Highlighter {
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

    /// Keep parser state across contiguous source lines, independently for each file or diff side.
    pub fn line(&mut self, source: &str) -> Line<'static> {
        let text = safe_output(source);
        let mut highlighter =
            HighlightLines::from_state(&CODE_THEME, self.state.0.clone(), self.state.1.clone());
        let line = match highlighter.highlight_line(&text, &languages::SYNTAXES) {
            Ok(tokens) => Line::from(
                tokens
                    .into_iter()
                    .map(|(style, token)| {
                        let color = style.foreground;
                        Span::styled(
                            token.to_owned(),
                            Style::default().fg(Color::Rgb(color.r, color.g, color.b)),
                        )
                    })
                    .collect::<Vec<_>>(),
            ),
            Err(_) => Line::from(text),
        };
        self.state = highlighter.state();
        line
    }
}

pub(super) fn file_lines(path: &str, content: &str) -> Vec<Line<'static>> {
    let mut highlighter = Highlighter::for_path(path);
    content.lines().map(|line| highlighter.line(line)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn highlight(renderer: &mut Highlighter, source: &str) -> Line<'static> {
        let line = renderer.line(source);
        assert_eq!(line.to_string(), source);
        line
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
            .map(Highlighter::new)
            .into_iter()
            .chain(["README.md", "docs/Orbit.MARKDOWN"].map(Highlighter::for_path));
        for mut renderer in renderers {
            for (source, token, foreground) in cases {
                highlight(&mut renderer, "");
                let line = highlight(&mut renderer, source);
                assert_eq!(style_at(&line, token).fg, Some(foreground), "{source}");
            }
        }
    }

    #[test]
    fn markdown_math_reuses_latex_tokens_and_preserves_multiline_state() {
        let mut markdown = Highlighter::new("md");
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
        let mut latex = Highlighter::new("latex");
        highlight(&mut latex, "$$");
        let latex = highlight(&mut latex, source);
        for token in [r"\int", r"\sqrt", r"\pi", "2"] {
            assert_eq!(style_at(&block, token), style_at(&latex, token), "{token}");
        }
        let closing = highlight(&mut markdown, "$$");
        assert_eq!(closing, highlight(&mut replay, "$$"));
        assert_eq!(
            highlight(&mut markdown, "Orbit 正文"),
            highlight(&mut Highlighter::new("md"), "Orbit 正文")
        );

        let mut fence = None;
        let mut markdown = Highlighter::new("md");
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
            let line = highlight(&mut Highlighter::new("md"), source);
            assert_eq!(style_at(&line, "$").fg, Some(foreground), "{source}");
        }
        let raw = highlight(&mut Highlighter::new("md"), "`$x$`");
        assert_eq!(style_at(&raw, "$").fg, Some(Color::Rgb(126, 205, 208)));

        let mut markdown = Highlighter::new("md");
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
            let line = highlight(&mut Highlighter::new(label), source);
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
        let mut rust = Highlighter::new("rust");
        highlight(&mut rust, "/* Orbit 注释");
        let continuation = highlight(&mut rust, "中文内容 */");
        assert_eq!(
            style_at(&continuation, "中文").fg,
            Some(Color::Rgb(163, 173, 190))
        );
    }
}
