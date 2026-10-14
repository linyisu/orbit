use ratatui::style::Modifier;

use super::*;

fn text(lines: &[LinkedLine]) -> String {
    lines
        .iter()
        .map(|line| line.line.to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn inline_example_preserves_semantics_and_portable_css() {
    let source = r#"<em>斜体</em>、<strong>粗体</strong>、<u>下划线</u>、<mark>高亮</mark>、<sup>2</sup>、<sub>下标</sub>、<small>小字</small>、<del>删除线</del>、<q>引用</q>，<kbd>Ctrl</kbd> + <kbd>Enter</kbd>，<span style="color:#e74c3c; font-weight:bold">彩色</span>，<abbr title="HyperText Markup Language">HTML</abbr>。"#;
    let lines = super::super::document(source, 100);
    let visible = text(&lines);
    assert_eq!(
        visible,
        r#"  斜体、粗体、下划线、高亮、<sup>2</sup>、<sub>下标</sub>、<small>小字</small>、删除线、<q>引用</q>，<kbd>Ctrl</kbd> + <kbd>Enter</kbd>，彩色，<abbr title="HyperText Markup Language">HTML</abbr>。"#
    );
    let spans = &lines[0].line.spans;
    let find = |value: &str| {
        spans
            .iter()
            .find(|span| span.content == value)
            .unwrap()
            .style
    };
    for value in ["2", "下标", "小字", "引用", "Ctrl", "Enter", "HTML"] {
        assert_eq!(find(value), ratatui::style::Style::default());
    }
    assert!(
        find("删除线")
            .add_modifier
            .contains(Modifier::CROSSED_OUT)
    );
    assert_eq!(
        find("彩色").fg,
        Some(ratatui::style::Color::Rgb(231, 76, 60))
    );
    assert!(
        find("彩色")
            .add_modifier
            .contains(Modifier::BOLD)
    );
}

#[test]
fn html_table_spans_share_native_grid_and_keep_click_targets() {
    let source = "<table>\n  <thead>\n    <tr><th>表头 A</th><th>表头 B</th></tr>\n  </thead>\n  \
        <tbody>\n    <tr><td><a href='https://orbit.example'>中文</a></td><td>单元格 2</td></tr>\n    \
        <tr><td colspan='2' align='center'>跨列单元格</td></tr>\n  </tbody>\n</table>";
    for width in [12, 20, 80] {
        let lines = super::super::document(source, width);
        let visible = text(&lines);
        assert!(!visible.contains("<table>"));
        assert!(visible.contains("跨列单元格"));
        let targets = lines
            .iter()
            .flat_map(|line| {
                line.links
                    .iter()
                    .map(|link| (&line.line, link))
            })
            .collect::<Vec<_>>();
        assert!(!targets.is_empty());
        assert_eq!(
            targets
                .iter()
                .map(|(line, link)| line.to_string()[link.start..link.end].to_owned())
                .collect::<String>(),
            "中文"
        );
        if width >= 20 {
            let span = lines
                .iter()
                .find(|line| {
                    line.line
                        .to_string()
                        .contains("跨列单元格")
                })
                .unwrap();
            assert_eq!(
                span.line
                    .to_string()
                    .matches('│')
                    .count(),
                2
            );
            assert!(
                lines
                    .iter()
                    .all(|line| line.line.width() <= usize::from(width))
            );
            assert!(
                !lines
                    .last()
                    .unwrap()
                    .line
                    .to_string()
                    .contains('┴')
            );
        }
    }
}

#[test]
fn headerless_and_ragged_tables_never_drop_cells() {
    let source = "<table><tr><td>第一行</td><td>第二列</td></tr><tr><td>短行</td></tr></table>";
    for width in [12, 80] {
        let lines = super::super::document(source, width);
        let visible = text(&lines);
        assert!(visible.contains("第一行"));
        assert!(visible.contains("第二列"));
        assert!(visible.contains("短行"));
        if width == 80 {
            let widths = lines
                .iter()
                .map(|line| line.line.width())
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(widths.len(), 1);
        }
    }
}

#[test]
fn html_blocks_preserve_surrounding_blank_lines_and_indented_source_code() {
    let lines = super::super::document("前文\n\n<p>段落</p>\n\n后文", 80);
    assert_eq!(text(&lines), "  前文\n  \n  段落\n  \n  后文");
    let lines = super::super::document("    <div>源码</div>", 80);
    assert!(text(&lines).contains("<div>源码</div>"));
    assert!(render("\n<p>段落</p>", 80, "body/0", 0).is_none());
}

#[test]
fn block_styles_links_and_generated_rails_share_copy_metadata() {
    let source = "<blockquote>引用 <code>&lt;blockquote&gt;</code></blockquote>\n\n<div \
        style='background:#f0f4f8; border-left:4px solid #3498db; padding:12px; margin:12px \
        0;'>提示 **加粗**</div>\n\n<hr>\n\n<img src='https://orbit.example/image.png' alt='HTML 图片' \
        width='400'>\n\n<p style='text-align:center; color:#888888;'>文档结束</p>";
    let lines = super::super::document(source, 60);
    let visible = text(&lines);
    assert!(visible.contains("引用 <blockquote>"));
    assert!(visible.contains("提示 加粗"));
    let quote = lines
        .iter()
        .find(|line| line.line.to_string().contains("引用"))
        .unwrap();
    assert!(!quote.decorations.is_empty());
    let callout = lines
        .iter()
        .find(|line| line.line.to_string().contains("提示"))
        .unwrap();
    assert!(!callout.decorations.is_empty());
    assert!(
        callout
            .line
            .spans
            .iter()
            .any(|span| span.content.contains("提示")
                && span.style.bg == Some(ratatui::style::Color::Rgb(240, 244, 248)))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.chrome && line.line.to_string() == format!("  {}  ", "─".repeat(56)))
    );
    let image = lines
        .iter()
        .find(|line| {
            line.line
                .to_string()
                .contains("HTML 图片")
        })
        .unwrap();
    assert_eq!(image.links[0].url, "https://orbit.example/image.png");
    assert_eq!(
        image.media[0].source,
        crate::tui::media::Source::Image("https://orbit.example/image.png".into())
    );
    let end = lines
        .iter()
        .find(|line| {
            line.line
                .to_string()
                .contains("文档结束")
        })
        .unwrap();
    assert!(
        end.line
            .to_string()
            .starts_with(&" ".repeat(20))
    );
    assert!(
        end.line
            .spans
            .iter()
            .any(|span| span.style.fg == Some(ratatui::style::Color::Rgb(136, 136, 136)))
    );
}

#[test]
fn details_keep_markdown_and_literal_code_with_independent_nested_ids() {
    let source = "<details>\n<summary>折叠</summary>\n\n- **加粗项目**\n- `fib(n)`\n- `</details>` \
        字面标签\n\n```html\n</details><b>源码</b>\n```\n\n<details \
        open><summary>子块</summary>子块内容</details>\n</details>\n后文";
    let block = render(source, 80, "body/0", 0).unwrap();
    assert!(block.complete);
    assert!(!source[..block.bytes].contains("后文"));
    let visible = text(&block.lines);
    assert!(visible.contains("加粗项目"));
    assert!(visible.contains("fib(n)"));
    assert!(visible.contains("</details> 字面标签"));
    assert!(visible.contains("</details><b>源码</b>"));
    let headers = block
        .lines
        .iter()
        .filter_map(|line| line.disclosure.as_ref())
        .collect::<Vec<_>>();

    assert_eq!(headers.len(), 2);
    assert_ne!(headers[0].id, headers[1].id);
    assert!(!headers[0].open);
    assert!(headers[1].open);
    let nested = block
        .lines
        .iter()
        .find(|line| {
            line.line
                .to_string()
                .contains("子块内容")
        })
        .unwrap();

    assert_eq!(nested.folds.len(), 2);
}

#[test]
fn html_callouts_and_rules_share_body_edges_and_keep_the_outer_margin_unpainted() {
    let source = "前文\n\n<div style='background:#f0f4f8;border-left:4px solid \
        #3498db;padding:12px'>提示 **文字**</div>\n\n<hr>\n\n---";
    for width in [24, 80, 140] {
        let lines = super::super::document(source, width);
        let callout = lines
            .iter()
            .find(|line| line.text().contains("提示"))
            .unwrap();
        assert_eq!(callout.line.width(), usize::from(width));
        assert_eq!(callout.line.spans[0].content, "  ");
        assert_eq!(callout.line.spans[0].style.bg, None);
        assert_eq!(
            callout
                .line
                .spans
                .last()
                .unwrap()
                .style
                .bg,
            None
        );
        assert!(
            callout
                .line
                .spans
                .iter()
                .any(|s| s.content == "▎ ")
        );
        let html = lines
            .iter()
            .find(|line| line.chrome && line.text().contains('─'))
            .unwrap();
        assert_eq!(
            html.text(),
            format!("  {}  ", "─".repeat(usize::from(width) - 4))
        );
        let mut markdown = lines.last().unwrap().line.clone();
        assert!(super::super::expand_rule(&mut markdown, width));
        assert_eq!(markdown, html.line);
    }
}

#[test]
fn details_use_a_body_aligned_indicator_and_preserve_summary_semantics() {
    let source = "<details><summary>普通 **强调** 标题</summary>正文</details>";
    let lines = super::super::document(source, 80);
    let header = &lines[0];
    assert!(
        header
            .text()
            .starts_with(&format!("  {} 普通", super::super::icons::CHEVRON_RIGHT))
    );
    assert!(header.line.spans.iter().any(|s| {
        s.content == "强调"
            && s.style
                .add_modifier
                .contains(Modifier::BOLD)
    }));
    assert!(header.line.spans.iter().any(|s| {
        s.content.contains("普通")
            && !s
                .style
                .add_modifier
                .contains(Modifier::BOLD)
            && s.style.fg != Some(THEME.accent)
    }));
    assert!(
        lines
            .iter()
            .find(|line| line.text().contains("正文"))
            .unwrap()
            .text()
            .starts_with("    正文")
    );
}
