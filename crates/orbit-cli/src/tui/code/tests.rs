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
