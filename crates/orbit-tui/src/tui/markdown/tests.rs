use super::table::labelled_table_rows;
use super::*;
use crate::tui::languages::syntax_for_label;

#[test]
fn basic_html_and_markdown_share_nested_semantic_styles() {
    for (html, markdown) in [
        ("<b>粗体</b>", "**粗体**"),
        ("<strong>粗体</strong>", "**粗体**"),
        ("<em>斜体</em>", "*斜体*"),
        ("<i>斜体</i>", "*斜体*"),
        ("<del>删除</del>", "~~删除~~"),
        ("<s>删除</s>", "~~删除~~"),
        ("<mark>高亮</mark>", "==高亮=="),
        ("<code>代码</code>", "`代码`"),
        ("<strong><em>粗斜体</em></strong>", "***粗斜体***"),
        ("<b>*粗斜体*</b>", "***粗斜体***"),
    ] {
        let html = document(html, 80);
        let markdown = document(markdown, 80);
        assert_eq!(html[0].line, markdown[0].line);
    }

    let value = render("<u>下划线</u> 正常");
    assert!(value.spans.iter().any(
            |span| span.content == "下划线" && span.style.add_modifier == Modifier::UNDERLINED
        ));
    assert!(
        value
            .spans
            .iter()
            .any(|span| span.content.contains("正常") && span.style.add_modifier.is_empty())
    );
}

#[test]
fn html_tokenizer_handles_case_attributes_entities_and_retains_unsupported_source() {
    let value = render("<STRONG title='a > b' style='color:red'>甲 &amp; 乙</STRONG> 正常");
    assert_eq!(value.to_string(), "  甲 & 乙 正常");
    assert!(
        value
            .spans
            .iter()
            .filter(|span| span.content.contains("甲"))
            .all(|span| span.style.fg == Some(Color::Rgb(255, 0, 0)))
    );
    assert_eq!(render("<span>原文</span>").to_string(), "  原文");
    for source in [
        "<div>原文</div>",
        "<!-- 注释 -->",
        "<script>alert('x')</script>",
        "<style>b{color:red}</style>",
    ] {
        assert_eq!(render(source).to_string(), format!("  {source}"));
    }

    assert_eq!(
        render("`<strong>原文</strong>`").to_string(),
        "  <strong>原文</strong>"
    );
    for label in ["html", "markdown", "text"] {
        let mut fence = None;
        line(&format!("```{label}"), 0, &mut fence);
        assert_eq!(
            line("<b>原文</b><br>下一行", 1, &mut fence).to_string(),
            "  │ <b>原文</b><br>下一行"
        );
    }
}

#[test]
fn html_breaks_preserve_styles_links_and_physical_line_offsets() {
    let url = "https://orbit.example/?a=1&b=2";
    let source = "- <a href='https://orbit.example/?a=1&amp;b=2'><b>中文<br />下一行</b></a> 正常";
    let rows = document(source, 80);
    assert_eq!(
        rows.iter()
            .map(|row| row.line.to_string())
            .collect::<Vec<_>>(),
        ["  • 中文", "  下一行 正常"]
    );
    for (row, label) in rows.iter().zip(["中文", "下一行"]) {
        assert_eq!(row.links.len(), 1);
        assert_eq!(row.links[0].url, url);
        assert_eq!(
            &row.line.to_string()[row.links[0].start..row.links[0].end],
            label
        );
        assert!(row.line.spans.iter().any(|span| {
            span.content == label
                && span
                    .style
                    .add_modifier
                    .contains(Modifier::BOLD | Modifier::UNDERLINED)
        }));
    }

    assert_eq!(
        document("甲<BR>乙<br/>丙", 80)
            .iter()
            .map(|row| row.line.to_string())
            .collect::<Vec<_>>(),
        ["  甲", "  乙", "  丙"]
    );
    let rows = document("\t<a href='https://orbit.example'>链接</a>", 80);
    let link = &rows[0].links[0];
    assert_eq!(&rows[0].line.to_string()[link.start..link.end], "链接");
    assert!(!rows[0].line.to_string().contains('\t'));
}

#[test]
fn unsupported_html_tags_keep_source_attributes_and_link_targets() {
    let source = "H<sub>2</sub>O x<sup>2</sup> y<sup>任意文字</sup> z<sub>ABC</sub>";
    assert_eq!(render(source).to_string(), format!("  {source}"));
    for (tag, value) in [
        ("sup", "上标12"),
        ("sub", "下标ab"),
        ("small", "小字"),
        ("abbr", "HTML"),
        ("q", "行内引用"),
        ("kbd", "Ctrl👩‍💻"),
    ] {
        let source = format!("<{tag} title='说明'>{value}</{tag}> 正常");
        let rendered = render(&source);
        assert_eq!(rendered.to_string(), format!("  {source}"));
        assert!(
            rendered
                .spans
                .iter()
                .all(|span| span.style == Style::default())
        );
    }

    let rows = document(
        "<a href='https://orbit.example'>x<sup><b>12</b></sup>水<sub>AB</sub></a>",
        80,
    );
    assert_eq!(rows[0].line.to_string(), "  x<sup>12</sup>水<sub>AB</sub>");
    assert_eq!(rows[0].links.len(), 1);
    let link = &rows[0].links[0];
    assert_eq!(
        &rows[0].line.to_string()[link.start..link.end],
        "x<sup>12</sup>水<sub>AB</sub>"
    );
    assert_eq!(link.url, "https://orbit.example");
    assert!(rows[0].line.spans.iter().any(|span| {
        span.content == "12"
            && span
                .style
                .add_modifier
                .contains(Modifier::BOLD | Modifier::UNDERLINED)
    }));
}

#[test]
fn html_styles_and_links_survive_source_lines_and_close_before_following_text() {
    let rows = document(
        "<a href='https://orbit.example'><strong>\n第一行\n第二行</strong></a> 正常",
        80,
    );

    for row in &rows[1..] {
        assert!(row.line.spans.iter().any(|span| {
            span.content.starts_with('第')
                && span
                    .style
                    .add_modifier
                    .contains(Modifier::BOLD | Modifier::UNDERLINED)
        }));
        assert_eq!(row.links.len(), 1);
    }

    assert!(
        rows[2]
            .line
            .spans
            .iter()
            .any(|span| span.content.contains("正常") && span.style.add_modifier.is_empty())
    );
}

#[test]
fn html_table_cells_break_into_rows_and_keep_links_on_narrow_and_wide_layouts() {
    let source = "| 名称 | 内容 |\n| --- | --- |\n| 示例 | <a href='https://orbit.example'>第一<br>第二</a> |";
    for width in [14, 16, 80] {
        let rows = document(source, width);
        let labels: String = rows
            .iter()
            .flat_map(|row| {
                row.links.iter().map(|link| {
                    assert_eq!(link.url, "https://orbit.example");
                    row.line.to_string()[link.start..link.end].to_owned()
                })
            })
            .collect();

        assert_eq!(labels, "第一第二");
        assert!(rows.iter().filter(|row| !row.links.is_empty()).count() >= 2);
        assert!(!rows.iter().any(|row| row.line.to_string().contains("<br>")));
    }
}

#[test]
fn indented_code_preserves_unicode_and_shorter_indentation() {
    let mut fence = None;
    line("    ```rust", 0, &mut fence);
    assert_eq!(line("中文", 1, &mut fence).to_string(), "      │ 中文");
    assert_eq!(
        line("  orbit", 2, &mut fence).to_string(),
        "      │   orbit"
    );
    assert_eq!(
        line("        orbit", 3, &mut fence).to_string(),
        "      │     orbit"
    );
}

#[test]
fn resolves_real_model_labels_aliases_attributes_and_filename_extensions() {
    for label in [
        "csharp",
        "C#",
        "cs",
        "C Sharp",
        "{.csharp}",
        "language-csharp",
        "Program.cs",
        "csharp title=demo",
    ] {
        assert_eq!(syntax_for_label(label).unwrap().name, "C#", "{label}");
    }

    for label in [
        "vb",
        "VB.NET",
        "vbnet",
        "Visual Basic",
        "Visual Basic .NET",
        "Module.vb",
    ] {
        assert_eq!(
            syntax_for_label(label).unwrap().name,
            "Visual Basic .NET",
            "{label}"
        );
    }

    for label in ["typescript", "ts", "tsx", "toml", "Dockerfile", "python3"] {
        assert!(syntax_for_label(label).is_some(), "{label}");
    }

    assert!(syntax_for_label(".NET").is_none());
    assert!(syntax_for_label("invented-language").is_none());

    for (label, sample) in [
        (
            "csharp",
            "public static int Fact(int n) { return 42; } // note",
        ),
        ("vb", "Public Function Fact(n As Integer) As Long ' note"),
    ] {
        let mut fence = None;
        line(&format!("```{label}"), 0, &mut fence);
        let rendered = line(sample, 1, &mut fence);
        let mut colors = Vec::new();

        for span in rendered.spans.iter().skip(1) {
            if !colors.contains(&span.style.fg) {
                colors.push(span.style.fg);
            }
        }

        assert!(colors.len() >= 3, "{label}: {rendered:?}");
        assert_eq!(rendered.to_string(), format!("  │ {sample}"));
    }
}

#[test]
fn indented_code_fence_inside_list_aligns_border_and_strips_leading_indent() {
    let mut fence = None;
    let open = line("    ```rust", 0, &mut fence);
    assert_eq!(open.to_string(), "      ╭─ rust ");

    let code = line("    let x = 42;", 1, &mut fence);
    assert_eq!(code.to_string(), "      │ let x = 42;");

    let nested_code = line("        let y = x + 1;", 2, &mut fence);
    assert_eq!(nested_code.to_string(), "      │     let y = x + 1;");

    let close = line("    ```", 3, &mut fence);
    assert_eq!(close.to_string(), "      ╰─");
}

#[test]
fn code_badges_share_alias_colors_and_keep_unsupported_code_unchanged() {
    let mut colors = Vec::new();
    for (label, alias) in [
        ("rust", "RS"),
        ("r", "R"),
        ("powershell", "pwsh"),
        ("python", "py"),
    ] {
        let header = line(&format!("```{label}"), 0, &mut None);
        let badge = &header.spans[1];
        let alias_header = line(&format!("```{alias}"), 0, &mut None);
        assert_eq!(badge.style.bg, alias_header.spans[1].style.bg);
        assert_eq!(header.spans[0].style.fg, badge.style.bg);
        assert_eq!(badge.style.fg, Some(THEME.on_accent));
        assert!(badge.style.add_modifier.contains(Modifier::BOLD));
        assert!(!colors.contains(&badge.style.bg), "{label}");
        colors.push(badge.style.bg);
    }

    for (label, title) in [
        ("powershell", "powershell"),
        ("orbit-script", "orbit-script"),
        ("", "text"),
    ] {
        let mut fence = None;
        let header = line(&format!("```{label}"), 0, &mut fence);
        assert_eq!(header.to_string(), format!("  ╭─ {title} "));
        let color = header.spans[1].style.bg;
        let source = r".\orbit\Scripts\orbit.exe --version # 中文";
        let body = line(source, 1, &mut fence);
        assert_eq!(body.to_string(), format!("  │ {source}"));
        assert_eq!(body.spans[0].style.fg, color);
        let closing = line("```", 2, &mut fence);
        assert_eq!(closing.style.fg, color);
        assert!(fence.is_none());
    }
}

#[test]
fn tables_align_unicode_wrap_cells_and_preserve_inline_content() {
    let source = "| 名称 | 值 |\n| :--- | ---: |\n| **中文** | `a\\|b` |\n| 长文本测试长文本 | 123 |\n\n后文";
    let (end, lines) = table(source, 28).unwrap();
    let rendered = lines
        .iter()
        .map(|line| line.line.to_string())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(rendered.contains("中文"), "{rendered}");
    assert!(rendered.contains("a|b"), "{rendered}");
    assert!(!rendered.contains("**") && !rendered.contains('`'));
    assert!(source[end..].contains("后文"));
    assert!(lines.iter().all(|line| line.line.width() <= 28));

    let (_, narrow) = table(source, 10).unwrap();

    assert!(narrow.iter().any(|l| l.line.to_string().contains("名称:")));
    assert!(table("normal | text\nnot a separator", 80).is_none());
}

#[test]
fn narrow_table_tolerates_a_body_wider_than_its_header() {
    let cell = |text: &'static str| Cell {
        lines: vec![LinkedLine::plain(Line::from(text))],
        colspan: 1,
        alignment: None,
        header: true,
    };
    let rows = vec![vec![cell("名称")], vec![cell("一"), cell("二")]];

    let lines = labelled_table_rows(&rows);

    assert!(
        lines
            .iter()
            .any(|line| line.line.to_string().contains("第 2 列: 二"))
    );
}

#[test]
fn code_highlighting_keeps_multiline_state_and_unknown_languages_readable() {
    let mut fence = None;
    line("```rust", 0, &mut fence);
    let code = line("let answer = 42; // comment", 1, &mut fence);

    assert_eq!(code.to_string(), "  │ let answer = 42; // comment");
    assert!(
        code.spans
            .windows(2)
            .any(|s| s[0].style.fg != s[1].style.fg)
    );

    line("/* comment", 2, &mut fence);
    let middle = line("still a comment", 3, &mut fence);
    let mut fresh = None;
    line("```rust", 0, &mut fresh);

    assert_ne!(middle, line("still a comment", 1, &mut fresh));

    line("```", 4, &mut fence);
    line("```unknown_language", 5, &mut fence);

    assert_eq!(
        line("中文 **raw**", 6, &mut fence).to_string(),
        "  │ 中文 **raw**"
    );
}

fn render(source: &str) -> Line<'static> {
    line(source, 1, &mut None)
}

#[test]
fn formats_inline_styles_without_modifying_code_or_urls() {
    let value = render("**重点**，*斜体*，~~删除~~，`**原样**` [文档](https://example.com)");

    assert_eq!(value.to_string(), "  重点，斜体，删除，**原样** 文档");
    assert!(
        value
            .spans
            .iter()
            .any(|s| s.content == "重点" && s.style.add_modifier.contains(Modifier::BOLD))
    );
    assert!(
        value
            .spans
            .iter()
            .any(|s| s.content == "斜体" && s.style.add_modifier.contains(Modifier::ITALIC))
    );
    assert!(value.spans.iter().any(|span| {
        span.content == "删除"
            && span.style.add_modifier.contains(Modifier::CROSSED_OUT)
            && span.style.fg == Some(THEME.md_strikethrough)
    }));
    assert_eq!(
        render(r"\*literal\* and snake_case_name").to_string(),
        "  *literal* and snake_case_name"
    );
    assert!(
        render("[1]: https://example.com")
            .to_string()
            .contains("https://example.com")
    );
}

#[test]
fn renders_mark_extension_without_touching_code_or_escaped_source() {
    let value = render("正常 ==重点== 与 `==代码==`");

    assert_eq!(value.to_string(), "  正常 重点 与 ==代码==");
    assert!(value.spans.iter().any(|span| {
        span.content == "重点"
            && span.style.fg == Some(THEME.md_mark)
            && span.style.add_modifier.contains(Modifier::UNDERLINED)
    }));
    assert!(value.spans.iter().any(|span| {
        span.content == "==代码=="
            && span.style.fg == Some(THEME.md_inline_code)
            && span.style.add_modifier.contains(Modifier::BOLD)
    }));
    assert_eq!(render(r"保留 \==字面量==").to_string(), "  保留 ==字面量==");
}

#[test]
fn recognizes_and_styles_bare_safe_web_links() {
    let source = "访问 https://example.com/docs?from=orbit，或 www.rust-lang.org/learn。`https://code.invalid`";
    let rendered = render(source);

    assert_eq!(
        rendered
            .spans
            .iter()
            .filter(|span| span.style.fg == Some(crate::tui::theme::THEME.md_link))
            .count(),
        2
    );
    assert_eq!(
        links(source, &rendered)
            .into_iter()
            .map(|target| target.url)
            .collect::<Vec<_>>(),
        vec![
            "https://example.com/docs?from=orbit".to_owned(),
            "https://www.rust-lang.org/learn".to_owned(),
            "https://code.invalid".to_owned(),
        ]
    );
}

#[test]
fn repeated_prose_does_not_steal_link_label_hits_when_destinations_are_hidden() {
    let source = "文档之前的文档 [文档](https://orbit.example/first) 与 [**文档**](https://orbit.example/second)";
    let rendered = render(source);
    let text = rendered.to_string();
    let targets = links(source, &rendered);
    assert_eq!(text, "  文档之前的文档 文档 与 文档");
    assert_eq!(targets.len(), 2);
    for (link, target) in targets.iter().zip(["first", "second"]) {
        assert_eq!(&text[link.start..link.end], "文档");
        assert_eq!(link.url, format!("https://orbit.example/{target}"));
        assert!(link.start >= "  文档之前的文档 ".len());
    }
}

#[test]
fn reference_links_and_remote_images_keep_targets_and_sources() {
    let source = "[Orbit 文档][docs] 与 ![Orbit 图片](https://orbit.example/image)\n\n| 项 | 地址 |\n|---|---|\n| 文档 | [Orbit][DOCS] |\n\n[docs]: https://orbit.example/docs/ \"Orbit Guide\"\n";
    let lines = document(source, 80);
    assert_eq!(lines[0].line.to_string(), "  Orbit 文档 与 Orbit 图片");
    let targets: Vec<_> = lines
        .iter()
        .flat_map(|line| {
            line.links.iter().map(move |link| {
                let text = line.line.to_string();
                (text[link.start..link.end].to_owned(), link.url.as_str())
            })
        })
        .collect();

    for (label, target) in [
        ("Orbit 文档", "https://orbit.example/docs/"),
        ("Orbit 图片", "https://orbit.example/image"),
        ("Orbit", "https://orbit.example/docs/"),
        ("https://orbit.example/docs/", "https://orbit.example/docs/"),
    ] {
        assert!(targets.contains(&(label.to_owned(), target)), "{targets:?}");
    }

    assert_eq!(
        lines[0].media[0].source,
        crate::tui::media::Source::Image("https://orbit.example/image".into())
    );
    let definition = lines
        .iter()
        .find(|line| line.line.to_string().contains("[docs]:"))
        .unwrap();
    assert!(
        definition
            .line
            .spans
            .iter()
            .any(|span| span.style.fg == Some(THEME.md_link))
    );
}

#[test]
fn task_list_replaces_the_list_bullet_with_its_checkbox() {
    assert_eq!(render("- [x] 已完成任务").to_string(), "  󰄵 已完成任务");
    assert_eq!(render("- [ ] 未完成任务").to_string(), "  󰄱 未完成任务");
}

#[test]
fn recovers_model_style_strong_markers_next_to_cjk_text() {
    let value =
        render(r#"4. **Orbit 的测试数据需要"可重复"**：这里使用**"虚构的示例"**验证文本渲染。"#);

    assert!(!value.to_string().contains("**"), "{value:?}");
    assert!(value.spans.iter().any(|span| {
        span.content == "\"虚构的示例\"" && span.style.add_modifier.contains(Modifier::BOLD)
    }));

    let escaped = render(r"保留 \*\*字面量\*\*");

    assert_eq!(escaped.to_string(), "  保留 **字面量**");
    assert!(
        escaped
            .spans
            .iter()
            .all(|span| !span.style.add_modifier.contains(Modifier::BOLD))
    );
}

#[test]
fn formats_blocks_and_preserves_indentation() {
    assert_eq!(line("普通段落", 0, &mut None).to_string(), "  普通段落");
    assert_eq!(render("## 标题").to_string(), "  标题");
    assert_eq!(render("  - **项目**").to_string(), "    • 项目");
    assert_eq!(render("3. 第三项").to_string(), "  3. 第三项");
    assert_eq!(render("> 引用").to_string(), "  ▎ 引用");
    assert!(render("- [x] 完成").to_string().contains("󰄵 完成"));
    assert!(render("#hashtag").to_string().contains("#hashtag"));
}

#[test]
fn code_fences_require_matching_delimiter_and_length() {
    let mut fence = None;

    assert_eq!(line("````rust", 0, &mut fence).to_string(), "  ╭─ rust ");
    assert_eq!(line("```", 1, &mut fence).to_string(), "  │ ```");
    assert_eq!(line("**code**", 2, &mut fence).to_string(), "  │ **code**");

    line("````", 3, &mut fence);

    assert_eq!(fence, None);

    line("~~~python", 4, &mut fence);

    assert_eq!(
        fence.as_ref().map(|f| (f.delimiter, f.length)),
        Some(('~', 3))
    );

    line("~~~~", 5, &mut fence);

    assert_eq!(fence, None);
}

#[test]
fn outer_fence_keeps_nested_markdown_as_source() {
    let mut fence = None;

    assert_eq!(
        line("````markdown", 0, &mut fence).to_string(),
        "  ╭─ markdown "
    );
    assert_eq!(line("# 标题", 1, &mut fence).to_string(), "  │ # 标题");
    assert_eq!(
        line("```python", 2, &mut fence).to_string(),
        "  │ ```python"
    );
    assert_eq!(
        line("**不应加粗**", 3, &mut fence).to_string(),
        "  │ **不应加粗**"
    );
    assert_eq!(line("```", 4, &mut fence).to_string(), "  │ ```");
    assert!(fence.is_some());
    assert_eq!(line("````", 5, &mut fence).to_string(), "  ╰─");
    assert!(fence.is_none());
}

#[test]
fn markdown_examples_recover_nested_triple_fences() {
    let mut fence = None;
    line("```markdown", 0, &mut fence);

    assert_eq!(
        line("```python", 1, &mut fence).to_string(),
        "  │ ```python"
    );
    assert_eq!(
        line("print('raw')", 2, &mut fence).to_string(),
        "  │ print('raw')"
    );
    assert_eq!(line("```", 3, &mut fence).to_string(), "  │ ```");
    assert!(
        fence.is_some(),
        "inner close must not end outer Markdown example"
    );
    assert_eq!(
        line("# still source", 4, &mut fence).to_string(),
        "  │ # still source"
    );
    assert_eq!(line("```", 5, &mut fence).to_string(), "  ╰─");
    assert!(fence.is_none());
}

#[test]
fn document_fragment_links_are_plain_text_in_markdown_html_and_tables() {
    for source in [
        "文档内锚点：[跳转至第一节标题测试](#section-one)",
        r##"<a href="#section-one">跳转至第一节标题测试</a>"##,
        "| 标题 |\n| --- |\n| [跳转至第一节标题测试](#section-one) |",
    ] {
        let lines = document(source, 80);
        assert!(
            lines
                .iter()
                .any(|line| line.text().contains("跳转至第一节标题测试"))
        );
        assert!(lines.iter().all(|line| line.links.is_empty()));
        assert!(
            lines
                .iter()
                .flat_map(|line| &line.line.spans)
                .filter(|span| span.content.contains("跳转"))
                .all(|span| !span.style.add_modifier.contains(Modifier::UNDERLINED))
        );
    }
}

#[test]
fn unpaired_word_asterisks_preserve_text_and_surrounding_emphasis() {
    for source in ["**Label (a***b)**：说明", "**Label (a***b c***d)**：说明"] {
        let rendered = document(source, 100);
        assert_eq!(
            rendered[0].line.to_string().trim(),
            source.trim_start_matches("**").replace("**：", "：")
        );
        for span in &rendered[0].line.spans {
            if span.content.contains("Label") || span.content.contains("b)") {
                assert!(span.style.add_modifier.contains(Modifier::BOLD));
            }
            assert!(!span.style.add_modifier.contains(Modifier::ITALIC));
        }
    }
    let paired = render("a***bold italic***b");
    assert_eq!(paired.to_string().trim(), "abold italicb");
    assert!(paired.spans.iter().any(|span| {
        span.content == "bold italic"
            && span
                .style
                .add_modifier
                .contains(Modifier::BOLD | Modifier::ITALIC)
    }));
    let nested = render("a***bold** italic*b");
    assert_eq!(nested.to_string().trim(), "abold italicb");
    assert!(nested.spans.iter().any(|span| {
        span.content == "bold"
            && span
                .style
                .add_modifier
                .contains(Modifier::BOLD | Modifier::ITALIC)
    }));
    for (source, expected) in [
        ("`a***b`", "a***b"),
        (r"a\*\*\*b", "a***b"),
        ("$a***b$", "$a***b$"),
    ] {
        assert_eq!(render(source).to_string().trim(), expected);
    }
    let linked = document("[link](https://example.test/a***b)", 100);
    assert_eq!(linked[0].links[0].url, "https://example.test/a***b");
}
