#[test]
fn footnote_targets_follow_streaming_definitions_wrapping_and_message_scope() {
    let source = "较长的正文引用命名脚注[^note]，以及数字脚注[^1]。\n\n[^note]: **第一段**\n\n    第二段\n[^1]: 数字内容\n";
    let mut layout = TranscriptLayout::default();
    for end in source
        .char_indices()
        .map(|(index, ch)| index + ch.len_utf8())
    {
        let items = vec![TranscriptItem::Assistant(source[..end].into())];
        layout.invalidate_from(0);
        layout.update(&items, None, 24);
        let mut fresh = TranscriptLayout::default();
        fresh.update(&items, None, 24);
        assert_eq!(layout.height(), fresh.height(), "{end}");
        let rows = |layout: &TranscriptLayout| {
            layout.entries[0]
                .rows
                .iter()
                .flat_map(|row| row.wrapped.iter().map(ToString::to_string))
                .collect::<Vec<_>>()
        };

        assert_eq!(rows(&layout), rows(&fresh), "{end}");
    }

    let items = vec![
        TranscriptItem::Assistant(source.into()),
        TranscriptItem::Assistant("另一条引用[^note]。\n\n[^note]: 不同会话消息的内容\n".into()),
    ];

    for width in [12, 24, 60] {
        layout.update(&items, None, width);
        let area = Rect::new(3, 2, width, layout.height() as u16);
        for entry in &layout.entries {
            let expected = entry
                .rows
                .iter()
                .find(|row| {
                    row.anchors
                        .iter()
                        .any(|anchor| anchor == "orbit://footnote/note")
                })
                .unwrap();
            let destination = entry.end - entry.height + expected.start;
            let point = (area.y..area.bottom())
                .find_map(|y| {
                    (area.x..area.right()).find_map(|x| {
                        let (hit_entry, _, _) = layout.row_at(area, 0, (x, y))?;
                        if !std::ptr::eq(hit_entry, entry) {
                            return None;
                        }
                        (layout.link_at(area, 0, (x, y)) == Some("orbit://footnote/note"))
                            .then_some((x, y))
                    })
                })
                .unwrap();

            assert_eq!(layout.footnote_at(area, 0, point), Some(destination));
        }
    }
}

#[test]
fn html_details_toggle_independently_and_survive_streaming_and_resize() {
    let source = "<details>\n<summary>折叠内容</summary>\n\n**正文** [链接](https://orbit.example)\n<details open><summary>子块</summary>嵌套内容</details>\n</details>\n后文";
    let mut layout = TranscriptLayout::default();
    let items = [TranscriptItem::Assistant(source.to_owned())];
    layout.update(&items, None, 80);
    let visible = |layout: &TranscriptLayout| {
        layout.entries[0]
            .rows
            .iter()
            .map(|row| row.line.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    };

    assert!(!visible(&layout).contains("正文"));
    let area = Rect::new(0, 0, 80, 40);
    let (index, outer) = layout.html_disclosure_at(area, 0, (3, 0)).unwrap();
    layout.toggle_html(index, outer.clone());
    layout.update(&items, None, 80);
    assert!(visible(&layout).contains("正文"));
    assert!(visible(&layout).contains("嵌套内容"));
    let inner = layout.entries[0]
        .rows
        .iter()
        .find_map(|row| row.html_disclosure.as_ref().filter(|id| **id != outer))
        .unwrap()
        .clone();

    layout.toggle_html(index, inner);
    layout.update(&items, None, 14);
    assert!(!visible(&layout).contains("嵌套内容"));
    assert!(visible(&layout).contains("正文"));
    // Streaming must not freeze provisional open blocks or change their IDs.
    let mut streamed = TranscriptLayout::default();
    for (end, ch) in source.char_indices() {
        streamed.invalidate_from(0);
        streamed.update(
            &[TranscriptItem::Assistant(
                source[..end + ch.len_utf8()].to_owned(),
            )],
            None,
            80,
        );
    }

    let streamed_outer = streamed.entries[0]
        .rows
        .iter()
        .find_map(|row| row.html_disclosure.as_ref())
        .unwrap()
        .clone();

    assert_eq!(streamed_outer, outer);
    assert_eq!(
        visible(&streamed),
        visible(&{
            let mut fresh = TranscriptLayout::default();
            fresh.update(&items, None, 80);
            fresh
        })
    );
}

#[test]
fn markdown_rule_keeps_symmetric_body_margins_after_resize() {
    let items = vec![super::TranscriptItem::Assistant(
        "before\n---\nafter\n".into(),
    )];
    let mut layout = super::TranscriptLayout::default();
    for width in [80, 180, 40] {
        layout.update(&items, None, width);
        let entry = &layout.entries[0];
        let rule = &entry.rows[1];
        assert_eq!(
            rule.line.to_string(),
            format!("{}  ", "─".repeat(usize::from(width) - 4))
        );
        assert_eq!(rule.height, 1);
        assert_eq!(rule.gutter.as_ref().unwrap().width, 2);
        assert_eq!(rule.wrapped[0].width(), usize::from(width) - 2);
    }
}

use ratatui::{Terminal, backend::TestBackend};

use super::*;

#[test]
fn nested_thinking_code_gutter_is_recognized() {
    assert!(is_rail_gutter("  ┊ │ "));
    assert!(is_rail_gutter("    │ "));
    assert!(is_rail_gutter("  ┊ "));
    assert!(!is_rail_gutter("text │ "));
}

#[test]
fn collapsed_disclosures_use_text_width_and_expanded_thinking_uses_the_full_area() {
    let mut row =
        super::super::app::ThinkingRow::from("some thinking content\n\nlast line".to_owned());
    row.finished = true;
    row.elapsed = Some(std::time::Duration::from_secs(1));
    let thinking = TranscriptItem::Thinking(row);
    let tool = TranscriptItem::Tool(super::super::app::ToolRow {
        id: "tool_1".into(),
        name: "read_file".into(),
        arguments: serde_json::json!({ "path": "src/main.rs" }).to_string(),
        status: super::super::app::ToolStatus::Finished,
        output: "done".into(),
    });
    let items = vec![thinking, tool];
    let mut layout = TranscriptLayout::default();
    let area = Rect::new(0, 0, 80, 40);

    layout.update(&items, None, 80);

    assert_eq!(layout.disclosure_at(&items, area, 0, (2, 0)), Some(0));
    assert_eq!(layout.disclosure_at(&items, area, 0, (50, 0)), None);

    let tool_y = layout.entries[0].height as u16;
    assert_eq!(layout.disclosure_at(&items, area, 0, (2, tool_y)), Some(1));
    assert_eq!(layout.disclosure_at(&items, area, 0, (50, tool_y)), None);

    for width in [24, 80] {
        layout.update(&items, Some(0), width);
        let area = Rect::new(3, 2, width, 40);
        let thinking = &layout.entries[0];
        let end = thinking.end - usize::from(thinking.spacing);
        for y in 0..end {
            for x in [area.x, area.x + width / 2, area.right() - 1] {
                assert_eq!(
                    layout.disclosure_at(&items, area, 0, (x, area.y + y as u16)),
                    Some(0)
                );
            }
        }
        assert_eq!(
            layout.disclosure_at(&items, area, 0, (area.right(), area.y)),
            None
        );
        assert_eq!(
            layout.disclosure_at(&items, area, 0, (area.x - 1, area.y)),
            None
        );
        assert_eq!(
            layout.disclosure_at(&items, area, 0, (area.x, area.y + end as u16)),
            None
        );

        let offset = 2;
        assert_eq!(
            layout.disclosure_at(&items, area, offset, (area.right() - 1, area.y)),
            Some(0)
        );
    }
}

#[test]
fn tool_title_and_output_are_clickable() {
    let items = vec![TranscriptItem::Tool(super::super::app::ToolRow {
        id: "web".into(),
        name: "web_search".into(),
        arguments: serde_json::json!({ "query": "papers" }).to_string(),
        status: super::super::app::ToolStatus::Failed,
        output: "first\nsecond\nthird\nfourth".into(),
    })];
    let mut layout = TranscriptLayout::default();
    let area = Rect::new(0, 0, 80, 40);

    layout.update(&items, None, 80);
    assert_eq!(layout.disclosure_at(&items, area, 0, (0, 0)), Some(0));
    for y in 1..layout.height() - 1 {
        assert_eq!(
            layout.disclosure_at(&items, area, 0, (0, y as u16)),
            Some(0)
        );
    }

    let collapsed = layout.height();
    layout.update(&items, Some(0), 80);

    assert!(layout.height() > collapsed);
    assert_eq!(layout.disclosure_at(&items, area, 0, (80, 2)), None);
}

#[test]
fn wrapped_thinking_and_code_keep_gutters_even_when_scrolled_mid_line() {
    let text = "中文 words 👋 words ".repeat(80);

    for (item, marker) in [
        (TranscriptItem::Thinking(text.clone().into()), "┊"),
        (
            TranscriptItem::Assistant(format!("```rust\n// {text}\n```")),
            "│",
        ),
    ] {
        let mut layout = TranscriptLayout::default();
        layout.update(&[item], None, 24);
        let mut terminal = Terminal::new(TestBackend::new(24, 6)).unwrap();
        terminal
            .draw(|frame| layout.draw(frame, frame.area(), 3))
            .unwrap();
        let buffer = terminal.backend().buffer();

        for y in 0..6 {
            let column = 2;
            assert_eq!(buffer[(column, y)].symbol(), marker);
            assert_eq!(buffer[(column + 1, y)].symbol(), " ");
        }
    }
}

#[test]
fn paragraph_continuations_keep_their_left_margin() {
    let text = "### 标题\n\n这是一段中文说明，换行以后仍然需要与段落开头对齐。\n  缩进段落也要保持原有的额外缩进，而不是向左跳。";
    let mut layout = TranscriptLayout::default();
    layout.update(&[TranscriptItem::Assistant(text.into())], None, 24);

    for row in layout.entries[0].rows.iter().filter(|row| row.height > 1) {
        let gutter = row.gutter.as_ref().expect("wrapped prose retains a margin");
        assert!(gutter.width >= 2);
        assert_eq!(gutter.first.width(), gutter.continuation.width());
    }

    assert!(layout.entries[0].rows.iter().any(|row| row.height > 1));
}

#[test]
fn wrapped_lists_keep_markers_with_content_and_use_hanging_indents() {
    let answer = "1. **第一项很长很长**：后面的说明继续延伸到下一行。\n2. **第二项也很长**：后面的说明同样继续延伸。\n- 无序项也应保留悬挂缩进。";
    let mut layout = TranscriptLayout::default();
    layout.update(&[TranscriptItem::Assistant(answer.into())], None, 24);
    let rows = &layout.entries[0].rows;

    for (index, marker) in ["1. ", "2. ", "• "].into_iter().enumerate() {
        let gutter = rows[index].gutter.as_ref().unwrap();

        assert!(gutter.first.to_string().contains(marker));
        assert_eq!(gutter.first.width(), gutter.continuation.width());
        assert!(!rows[index].line.to_string().is_empty());
    }

    let mut terminal = Terminal::new(TestBackend::new(24, 8)).unwrap();
    terminal
        .draw(|frame| layout.draw(frame, frame.area(), 0))
        .unwrap();
    let buffer = terminal.backend().buffer();

    assert_eq!(buffer[(2, 0)].symbol(), "1");
    assert_ne!(buffer[(5, 0)].symbol(), " ");
    assert_eq!(buffer[(2, 1)].symbol(), " ");
    assert_eq!(buffer[(3, 1)].symbol(), " ");
    assert_eq!(buffer[(4, 1)].symbol(), " ");
}

#[test]
fn task_list_checkbox_keeps_a_hanging_indent() {
    let mut layout = TranscriptLayout::default();
    layout.update(
        &[TranscriptItem::Assistant(
            "- [ ] 这是一个会换行的待完成任务，用于验证复选框不会再和项目符号重复显示。".into(),
        )],
        None,
        20,
    );
    let row = &layout.entries[0].rows[0];

    assert_eq!(row.gutter.as_ref().unwrap().first.to_string(), "  󰄱 ");
    assert_eq!(
        row.gutter.as_ref().unwrap().first.width(),
        row.gutter.as_ref().unwrap().continuation.width()
    );
}

#[test]
fn ctrl_click_targets_link_labels_and_urls_in_code_source() {
    let mut layout = TranscriptLayout::default();
    let area = Rect::new(0, 0, 18, 8);
    layout.update(
        &[TranscriptItem::Assistant(
            "阅读 [Orbit 文档](https://example.com/orbit) 获取详情。".into(),
        )],
        None,
        area.width,
    );
    let target = (area.y..area.bottom()).find_map(|y| {
        (area.x..area.right()).find_map(|x| {
            layout
                .link_at(area, 0, (x, y))
                .map(|url| (x, y, url.to_owned()))
        })
    });

    assert_eq!(
        target.as_ref().map(|(_, _, url)| url.as_str()),
        Some("https://example.com/orbit")
    );

    let mut code_layout = TranscriptLayout::default();
    code_layout.update(
        &[TranscriptItem::Assistant(
            "```markdown\n[不是链接](https://example.com/no)\n```".into(),
        )],
        None,
        area.width,
    );

    let targets: HashSet<_> = (0..code_layout.height())
        .flat_map(|offset| {
            let layout = &code_layout;
            (area.y..area.bottom()).flat_map(move |y| {
                (area.x..area.right()).filter_map(move |x| layout.link_at(area, offset, (x, y)))
            })
        })
        .collect();

    assert_eq!(targets, HashSet::from(["https://example.com/no"]));
}

#[test]
fn plain_output_and_wrapped_table_cells_keep_their_own_link_targets() {
    use super::super::app::{ToolRow, ToolStatus};
    let items = [
            TranscriptItem::User("中文 https://example.com/user".into()),
            TranscriptItem::CommandResult("结果 https://example.com/command".into()),
            TranscriptItem::Tool(ToolRow {
                id: "tool".into(), name: "run_command".into(), arguments: "{}".into(),
                status: ToolStatus::Finished, output: "地址 https://example.com/tool".into(),
            }),
            TranscriptItem::Assistant("| 名称 | 地址 |\n| --- | --- |\n| [中文链接](https://example.com/one) | [中文链接](https://example.com/two) |\n".into()),
        ];

    for width in [10, 26, 80] {
        let mut layout = TranscriptLayout::default();
        layout.update(&items, Some(2), width);
        let area = Rect::new(3, 2, width, layout.height() as u16);
        let mut targets = HashSet::new();
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                if let Some(url) = layout.link_at(area, 0, (x, y)) {
                    targets.insert(url);
                }
            }
        }

        for suffix in ["user", "command", "tool", "one", "two"] {
            assert!(
                targets.contains(format!("https://example.com/{suffix}").as_str()),
                "width {width}, missing {suffix}: {targets:?}"
            );
        }

        for row in &layout.entries.last().unwrap().rows {
            for link in &row.links {
                let text = row.line.to_string();
                assert!(text.get(link.start..link.end).is_some());
                assert!(link.url.ends_with("/one") || link.url.ends_with("/two"));
            }
        }
    }
}

#[test]
fn markdown_file_links_remain_actionable_after_resize_and_scroll() {
    use super::super::app::{ToolRow, ToolStatus};
    let items = [TranscriptItem::Tool(ToolRow {
            id: "orbit-doc".into(),
            name: "read_file".into(),
            arguments: r#"{"path":"docs/Orbit Guide.md"}"#.into(),
            status: ToolStatus::Finished,
            output: serde_json::json!({
                "path": "docs/Orbit Guide.md",
                "content": "1: # Orbit\n2: \n3: | 名称 | 链接 |\n4: | --- | --- |\n5: | [Orbit](https://orbit.example/first) | [Orbit](https://orbit.example/second) |\n6: \n7: ```markdown\n8: [Orbit](https://orbit.example/code)\n9: ```\n"
            }).to_string(),
        })];
    let mut layout = TranscriptLayout::default();
    for width in [80, 16, 32, 10, 80] {
        layout.update(&items, Some(0), width);
        let area = Rect::new(3, 2, width, 7);
        let mut targets = HashSet::new();
        for offset in 0..layout.height() {
            for y in area.y..area.bottom() {
                for x in area.x..area.right() {
                    if let Some(url) = layout.link_at(area, offset, (x, y)) {
                        targets.insert(url.to_owned());
                    }
                }
            }
        }

        assert_eq!(
            targets,
            if width == 10 {
                HashSet::new()
            } else {
                HashSet::from([
                    "https://orbit.example/first".to_owned(),
                    "https://orbit.example/second".to_owned(),
                    "https://orbit.example/code".to_owned(),
                ])
            },
            "width {width}"
        );
    }
}

#[test]
fn streamed_reference_definitions_update_prior_labels_without_reparsing_on_scroll() {
    let mut layout = TranscriptLayout::default();
    let initial = "阅读 [Orbit 文档][DOCS]\n\n";
    let text = format!("{initial}[docs]: https://orbit.example/docs/ \"Orbit\"\n");
    let mut items = [TranscriptItem::Assistant(initial.into())];
    layout.update(&items, None, 24);
    assert!(layout.entries[0].rows[0].links.is_empty());
    for end in initial.len()..=text.len() {
        if !text.is_char_boundary(end) {
            continue;
        }
        items[0] = TranscriptItem::Assistant(text[..end].into());
        layout.invalidate_from(0);
        layout.update(&items, None, 24);
    }

    let label = &layout.entries[0].rows[0];
    assert_eq!(label.line.to_string(), "阅读 Orbit 文档");
    assert_eq!(label.links.len(), 1);
    assert_eq!(label.links[0].url, "https://orbit.example/docs/");
    let stable = layout.entries[0].parsed_bytes;
    layout.update(&items, None, 24);
    assert_eq!(layout.entries[0].parsed_bytes, stable);
    let mut fresh = TranscriptLayout::default();
    fresh.update(&items, None, 24);
    assert_eq!(layout.height(), fresh.height());
    for (cached, rebuilt) in layout.entries[0].rows.iter().zip(&fresh.entries[0].rows) {
        assert_eq!(cached.line, rebuilt.line);
        assert_eq!(cached.links, rebuilt.links);
    }
}

#[test]
fn command_results_remain_plain_beneath_the_command() {
    let mut layout = TranscriptLayout::default();
    layout.update(
        &[TranscriptItem::CommandResult("状态\n第二行细节".into())],
        None,
        32,
    );
    let rows = &layout.entries[0].rows;

    assert_eq!(rows[0].line.to_string(), "  状态");
    assert_eq!(rows[1].line.to_string(), "  第二行细节");
}

#[test]
fn user_turn_marker_is_joined_to_the_top_border() {
    let mut layout = TranscriptLayout::default();
    layout.update(&[TranscriptItem::User("问题".into())], None, 32);

    assert!(
        layout.entries[0].rows[0]
            .line
            .to_string()
            .starts_with(&format!("╭─{}─", super::super::icons::PROMPT))
    );
}

#[test]
fn thinking_keeps_one_row_before_the_following_answer() {
    let mut layout = TranscriptLayout::default();
    layout.update(
        &[
            TranscriptItem::Thinking("inspect the request".to_owned().into()),
            TranscriptItem::Assistant("final answer".into()),
        ],
        None,
        40,
    );
    let thinking = &layout.entries[0];

    assert!(thinking.rows.last().unwrap().line.to_string().is_empty());
    assert_eq!(layout.entries[1].rows[0].line.to_string(), "final answer");
    assert_eq!(layout.entries[1].rows[0].gutter.as_ref().unwrap().width, 2);
}

#[test]
fn thinking_code_keeps_both_rails_when_wrapped() {
    let mut layout = TranscriptLayout::default();
    layout.update(
        &[TranscriptItem::Thinking(
            format!("```markdown\n{}\n```", "long Markdown source ".repeat(4)).into(),
        )],
        None,
        22,
    );
    let row = &layout.entries[0].rows[1];
    let gutter = row.gutter.as_ref().unwrap();

    assert_eq!(gutter.first.to_string(), "  ┊ │ ");
    assert_eq!(gutter.continuation.to_string(), "  ┊ │ ");
    assert!(row.height > 1);

    let mut terminal = Terminal::new(TestBackend::new(22, 5)).unwrap();
    terminal
        .draw(|frame| layout.draw(frame, frame.area(), 0))
        .unwrap();

    for y in 1..5 {
        assert_eq!(terminal.backend().buffer()[(2, y)].symbol(), "┊");
        assert_eq!(terminal.backend().buffer()[(4, y)].symbol(), "│");
    }
}

#[test]
fn unicode_line_breaks_fill_cjk_rows_and_quotes_keep_a_continuous_rail() {
    let source = "前缀 + 沙箱工具测试 后文";
    let ranges = wrap_ranges_from(source, 10, 10);

    assert!(source[ranges[0].clone()].width() >= 9);
    assert!(ranges.len() >= 2);

    let mut layout = TranscriptLayout::default();
    layout.update(
        &[TranscriptItem::Assistant(format!(
            "> {}",
            "这是一段需要换行的引用内容".repeat(4)
        ))],
        None,
        20,
    );
    let row = &layout.entries[0].rows[0];
    let gutter = row.gutter.as_ref().unwrap();

    assert_eq!(gutter.first.to_string(), "  ▎ ");
    assert_eq!(gutter.continuation.to_string(), "  ▎ ");
    assert!(row.height > 1);

    let mut terminal = Terminal::new(TestBackend::new(20, 5)).unwrap();
    terminal
        .draw(|frame| layout.draw(frame, frame.area(), 0))
        .unwrap();

    for y in 0..5 {
        assert_eq!(terminal.backend().buffer()[(2, y)].symbol(), "▎");
    }
}

#[tokio::test]
#[ignore = "requires a disposable database copy and ORBIT_PROFILE_SESSION"]
async fn profile_saved_session_layout() {
    let path = std::env::var("ORBIT_PROFILE_DATABASE").unwrap();
    let id = std::env::var("ORBIT_PROFILE_SESSION").unwrap();
    let start = std::time::Instant::now();
    let session = crate::session::SessionState::open(path.into(), Some(id), "profile".into())
        .await
        .unwrap();
    eprintln!("load: {:?}", start.elapsed());
    let start = std::time::Instant::now();
    let mut state = super::super::app::UiState::new("profile");
    session.restore_ui(&mut state);
    eprintln!(
        "replay: {:?}, items: {}",
        start.elapsed(),
        state.transcript.len()
    );
    let mut layout = TranscriptLayout::default();
    let start = std::time::Instant::now();
    layout.update(&state.transcript, None, 100);
    eprintln!("layout: {:?}, rows: {}", start.elapsed(), layout.height());
}

#[test]
fn table_streaming_one_character_at_a_time_matches_complete_render() {
    let source = "| # | 语言 | 特性 |\n|---|---|---|\n| 1 | **Python** | `complex` |\n| 2 | C | restrict |\n| 3 | C++ | 模板 |\n\n后文\n";
    let mut text = String::new();
    let mut layout = TranscriptLayout::default();

    for ch in source.chars() {
        text.push(ch);
        let items = [TranscriptItem::Assistant(text.clone())];
        layout.invalidate_from(0);
        layout.update(&items, None, 80);
        let mut fresh = TranscriptLayout::default();
        fresh.update(&items, None, 80);
        let lines = |l: &TranscriptLayout| {
            l.entries[0]
                .rows
                .iter()
                .map(|r| r.line.clone())
                .collect::<Vec<_>>()
        };

        assert_eq!(lines(&layout), lines(&fresh), "input: {text:?}");
    }
}

#[test]
fn streamed_tables_match_fresh_layout_through_split_delimiters_and_resize() {
    let mut text = String::new();
    let mut layout = TranscriptLayout::default();

    for chunk in [
        "intro\n| 名称 | 数量 |\n",
        "| ---",
        " | ---: |\n",
        "| 中文 | 12",
        "3 |\n",
        "| **test** | 4 |\n",
        "\n```rust\n/* hi\n",
        "comment */\nlet x = 1;\n```\nafter\n",
    ] {
        text.push_str(chunk);
        let items = vec![TranscriptItem::Assistant(text.clone())];
        layout.invalidate_from(0);
        layout.update(&items, None, 35);
        let mut fresh = TranscriptLayout::default();
        fresh.update(&items, None, 35);

        assert_eq!(layout.height(), fresh.height());

        let actual: Vec<_> = layout.entries[0].rows.iter().map(|r| &r.line).collect();
        let expected: Vec<_> = fresh.entries[0].rows.iter().map(|r| &r.line).collect();

        assert_eq!(actual, expected, "source: {text}");
    }

    for width in [12, 60] {
        layout.update(&[TranscriptItem::Assistant(text.clone())], None, width);

        assert!(layout.height() > 0);
    }
}

#[test]
fn batched_assistant_completion_matches_fresh_layout_after_trimming() {
    use super::super::app::UiState;
    use orbit_agent::events::AgentEvent;
    use orbit_model::types::{FunctionCall, StreamEvent, ToolCall};

    for tool_started in [true, false] {
        for (initial, delta) in [
            ("\n\n", "好的，正在检查 Orbit。\n"),
            ("  \n", "Orbit\n"),
            ("  \n", "好\n"),
            ("\n\nOrbit\n", "你好，Orbit。\n"),
            ("好的，Orbit。\n \t\n", ""),
        ] {
            let mut state = UiState::new("orbit-test");
            state.submit("检查 Orbit".into());
            state.apply(AgentEvent::Model(StreamEvent::TextDelta(initial.into())));
            state.transcript_layout.update(&state.transcript, None, 24);
            if !delta.is_empty() {
                state.apply(AgentEvent::Model(StreamEvent::TextDelta(delta.into())));
            }

            state.apply(if tool_started {
                AgentEvent::ToolStarted {
                    call: ToolCall {
                        metadata: None,
                        id: "orbit-check".into(),
                        kind: "function".into(),
                        function: FunctionCall {
                            name: "run_command".into(),
                            arguments: r#"{"command":"orbit --version"}"#.into(),
                        },
                    },
                }
            } else {
                AgentEvent::Completed {
                    turns: 1,
                    tool_calls: 0,
                }
            });

            state.transcript_layout.update(&state.transcript, None, 24);
            let mut fresh = TranscriptLayout::default();
            fresh.update(&state.transcript, None, 24);

            assert_eq!(state.transcript_layout.height(), fresh.height());

            for offset in 0..fresh.height() {
                let mut actual = Terminal::new(TestBackend::new(24, 5)).unwrap();
                actual
                    .draw(|frame| state.transcript_layout.draw(frame, frame.area(), offset))
                    .unwrap();
                let mut expected = Terminal::new(TestBackend::new(24, 5)).unwrap();
                expected
                    .draw(|frame| fresh.draw(frame, frame.area(), offset))
                    .unwrap();

                assert_eq!(
                    actual.backend().buffer(),
                    expected.backend().buffer(),
                    "tool_started {tool_started}, initial {initial:?}, delta {delta:?}, offset {offset}"
                );
            }
        }
    }
}

#[test]
fn removed_empty_assistant_is_replaced_before_the_next_frame() {
    use super::super::app::UiState;
    use orbit_agent::events::AgentEvent;
    use orbit_model::types::StreamEvent;

    for event in [
        StreamEvent::TextDelta("好的，检查 Orbit。\n".into()),
        StreamEvent::ReasoningDelta("好的，分析 Orbit。\n".into()),
    ] {
        let mut state = UiState::new("orbit-test");
        state.submit("检查 Orbit".into());
        state.apply(AgentEvent::Model(StreamEvent::TextDelta("\n\n".into())));
        state.transcript_layout.update(&state.transcript, None, 24);
        state.apply(AgentEvent::Completed {
            turns: 1,
            tool_calls: 0,
        });
        state.apply(AgentEvent::Model(event));
        state.transcript_layout.update(&state.transcript, None, 24);
        let mut fresh = TranscriptLayout::default();
        fresh.update(&state.transcript, None, 24);
        let lines = |layout: &TranscriptLayout| {
            layout
                .entries
                .iter()
                .flat_map(|entry| entry.rows.iter().map(|row| row.line.clone()))
                .collect::<Vec<_>>()
        };

        assert_eq!(state.transcript_layout.height(), fresh.height());
        assert_eq!(lines(&state.transcript_layout), lines(&fresh));
    }
}

#[test]
fn streamed_math_matches_fresh_parser_at_every_character_boundary() {
    let source = "先看 $x^2$\n\n$$\n\\begin{pmatrix}a&b\\\\c&d\\end{pmatrix}\n$$\n后文 $\\frac{a}{b}$\n```latex\n$x$\n```\n";
    let mut incremental = TranscriptLayout::default();
    let mut items = vec![TranscriptItem::Assistant(String::new())];
    for ch in source.chars() {
        let TranscriptItem::Assistant(text) = &mut items[0] else {
            unreachable!()
        };
        text.push(ch);
        let observed = text.clone();
        incremental.invalidate_from(0);
        incremental.update(&items, None, 36);
        let mut fresh = TranscriptLayout::default();
        fresh.update(&items, None, 36);
        assert_eq!(
            incremental.height(),
            fresh.height(),
            "{observed:?}; stable={} actual={:?}; fresh={:?}",
            incremental.entries[0].parsed_bytes,
            incremental.entries[0]
                .rows
                .iter()
                .map(|r| r.line.to_string())
                .collect::<Vec<_>>(),
            fresh.entries[0]
                .rows
                .iter()
                .map(|r| r.line.to_string())
                .collect::<Vec<_>>()
        );
        let actual = incremental.entries[0]
            .rows
            .iter()
            .map(|r| (&r.line, &r.media))
            .collect::<Vec<_>>();
        let expected = fresh.entries[0]
            .rows
            .iter()
            .map(|r| (&r.line, &r.media))
            .collect::<Vec<_>>();

        assert_eq!(actual, expected, "{observed:?}");
    }
}

#[test]
fn incremental_layout_matches_fresh_layout_for_unicode_wrapping_and_split_fences() {
    let mut layout = TranscriptLayout::default();
    let mut items = vec![TranscriptItem::Assistant(String::new())];

    for chunk in [
        "你好 e\u{301} words words",
        " **加",
        "粗** 与 `代码`",
        "\n- 项目 [链接](https://example.com)\n> 引用",
        "\n# Heading\n`",
        "``rust\n",
        "let x = 1;\n```",
        "\nlast 👨‍👩‍👧‍👦 line\n",
    ] {
        let TranscriptItem::Assistant(text) = &mut items[0] else {
            unreachable!()
        };

        text.push_str(chunk);
        layout.invalidate_from(0);

        for width in [17] {
            layout.update(&items, None, width);
            let mut reference = TranscriptLayout::default();
            reference.update(&items, None, width);

            assert_eq!(layout.height(), reference.height());

            for offset in 0..layout.height() {
                let mut actual = Terminal::new(TestBackend::new(width, 5)).unwrap();
                actual.draw(|f| layout.draw(f, f.area(), offset)).unwrap();
                let mut expected = Terminal::new(TestBackend::new(width, 5)).unwrap();
                expected
                    .draw(|f| reference.draw(f, f.area(), offset))
                    .unwrap();

                assert_eq!(
                    actual.backend().buffer(),
                    expected.backend().buffer(),
                    "width {width} offset {offset}"
                );
            }
        }
    }
}

#[test]
fn streamed_html_preserves_open_frames_breaks_and_click_targets_after_resize() {
    let source = "<a href='https://orbit.example'><b>\n中文 H<sub>2</sub>O<br/>x<sup>2</sup>\n最后一行</b></a> 正常\n";
    let mut entry = Entry::default();
    for end in source
        .char_indices()
        .map(|(index, ch)| index + ch.len_utf8())
    {
        entry.stream(&source[..end], false, 18);
        let mut fresh = Entry::default();
        fresh.stream(&source[..end], false, 18);
        assert_eq!(entry.height, fresh.height, "{end}");
        assert_eq!(entry.inline, fresh.inline, "{end}");
        for (actual, expected) in entry.rows.iter().zip(&fresh.rows) {
            assert_eq!(actual.line, expected.line, "{end}");
            assert_eq!(actual.links, expected.links, "{end}");
            assert_eq!(actual.wrapped, expected.wrapped, "{end}");
        }
    }

    let items = [TranscriptItem::Assistant(source.to_owned())];
    let mut layout = TranscriptLayout::default();
    for width in [18, 80, 14] {
        layout.update(&items, None, width);
        let mut fresh = TranscriptLayout::default();
        fresh.update(&items, None, width);
        let area = Rect::new(0, 0, width, layout.height() as u16);
        for y in 0..area.height {
            for x in 0..width {
                assert_eq!(
                    layout.link_at(area, 0, (x, y)),
                    fresh.link_at(area, 0, (x, y))
                );
            }
        }
    }
}

#[test]
fn width_and_expansion_invalidate_cached_heights() {
    let items = vec![
        TranscriptItem::Thinking("thinking words ".repeat(80).into()),
        TranscriptItem::Assistant("answer words ".repeat(80)),
    ];
    let mut layout = TranscriptLayout::default();

    for (width, expanded) in [
        (80, None),
        (20, None),
        (20, Some(1)),
        (80, Some(1)),
        (80, None),
    ] {
        layout.update(&items, expanded, width);
        let mut expected = TranscriptLayout::default();
        expected.update(&items, expanded, width);

        assert_eq!(layout.height(), expected.height());

        let offset = layout.height().saturating_sub(6);
        let mut actual_terminal = Terminal::new(TestBackend::new(width, 6)).unwrap();
        actual_terminal
            .draw(|f| layout.draw(f, f.area(), offset))
            .unwrap();
        let mut expected_terminal = Terminal::new(TestBackend::new(width, 6)).unwrap();
        expected_terminal
            .draw(|f| expected.draw(f, f.area(), offset))
            .unwrap();

        assert_eq!(
            actual_terminal.backend().buffer(),
            expected_terminal.backend().buffer()
        );
    }
}

#[test]
fn user_turns_use_a_framed_bubble_and_wrap_inside_its_side_rails() {
    let mut layout = TranscriptLayout::default();
    layout.update(
        &[TranscriptItem::User(
            "一段很长的用户输入，需要在边框内换行。".repeat(4),
        )],
        None,
        24,
    );
    let rows = &layout.entries[0].rows;

    assert_eq!(
        rows.first().unwrap().line.to_string().chars().next(),
        Some('╭')
    );
    assert_eq!(rows.last().unwrap().line.to_string(), "");
    assert!(rows.iter().any(|row| {
        row.gutter
            .as_ref()
            .is_some_and(|gutter| gutter.first.to_string() == "│ ")
    }));
    assert!(rows.iter().any(|row| row.line.to_string().starts_with('╰')));
    assert!(rows.iter().all(|row| row.line.width() <= 24));
}

#[test]
fn user_wrap_consumes_overflowing_spaces_and_keeps_fitting_whitespace() {
    let first = "你好，请你在当前目录中将所有你能使用的工具依次使用一遍";
    let second = "如果有对仓库造成影响的，则新建一个demo/，在内部执行。";
    let source = format!("{first}{}{second}", " ".repeat(85));
    let items = [TranscriptItem::User(source.clone())];
    let mut layout = TranscriptLayout::default();
    let body = |layout: &TranscriptLayout| {
        layout.entries[0]
            .rows
            .iter()
            .filter(|row| {
                row.gutter
                    .as_ref()
                    .is_some_and(|gutter| gutter.first.to_string() == "│ ")
            })
            .map(|row| {
                row.line
                    .to_string()
                    .strip_suffix(" │")
                    .unwrap()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
    };

    for width in [24, 48, 100, first.width() as u16 + 85 + 12 + 4] {
        layout.update(&items, None, width);
        let lines = body(&layout);
        assert!(
            lines.iter().any(|line| line.starts_with("如果")),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .all(|line| !line.is_empty() && !line.contains("  "))
        );
        assert_eq!(lines.concat(), format!("{first}{second}"));
        assert!(
            layout.entries[0]
                .rows
                .iter()
                .all(|row| row.line.width() <= usize::from(width))
        );
    }

    // A line that fits still shows every space; the source passed to the
    // model and stored in history is never rewritten by the layout.
    layout.update(&items, None, source.width() as u16 + 4);
    assert_eq!(body(&layout).as_slice(), std::slice::from_ref(&source));
    assert!(matches!(&items[0], TranscriptItem::User(text) if text == &source));

    let verbatim = wrap_line(&Line::from(source.clone()), 40).0;
    assert_eq!(
        verbatim.iter().map(Line::to_string).collect::<String>(),
        source
    );
    let source = "    缩进  与列对齐\n\nword\u{a0}word";
    layout.update(&[TranscriptItem::User(source.into())], None, 80);
    assert_eq!(body(&layout).join("\n"), source);
}

#[test]
fn user_links_remain_clickable_after_consuming_a_wrap_separator() {
    let url = "https://orbit.example/demo";
    let items = [TranscriptItem::User(format!(
        "第一句{}{url}",
        " ".repeat(85)
    ))];

    for width in [12, 40, 100] {
        let mut layout = TranscriptLayout::default();
        layout.update(&items, None, width);
        let area = Rect::new(3, 2, width, layout.height() as u16);
        let mut found = false;
        for y in area.y..area.bottom() {
            assert!(layout.link_at(area, 0, (area.x, y)).is_none());
            for x in area.x..area.right() {
                if let Some(target) = layout.link_at(area, 0, (x, y)) {
                    assert_eq!(target, url);
                    found = true;
                }
            }
        }

        assert!(found, "width {width}");
    }
}

#[test]
fn streaming_keeps_completed_lines_and_scrolling_reuses_layout() {
    let mut items = vec![TranscriptItem::Assistant("history line\n".repeat(10000))];
    let mut layout = TranscriptLayout::default();
    layout.update(&items, None, 100);
    let stable = layout.entries[0].rows[500].line.spans[0].content.as_ptr();

    for _ in 0..30 {
        let TranscriptItem::Assistant(text) = &mut items[0] else {
            unreachable!()
        };

        text.push_str("stream delta ");
        layout.invalidate_from(0);
        layout.update(&items, None, 100);

        assert_eq!(
            stable,
            layout.entries[0].rows[500].line.spans[0].content.as_ptr()
        );
    }

    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    let started = std::time::Instant::now();

    for offset in (0..10000).step_by(100) {
        layout.update(&items, None, 100);
        terminal.draw(|f| layout.draw(f, f.area(), offset)).unwrap();
    }

    eprintln!(
        "100 scroll frames over 10000 lines: {:?}",
        started.elapsed()
    );

    assert_eq!(
        stable,
        layout.entries[0].rows[500].line.spans[0].content.as_ptr()
    );
}

#[test]
fn wrapped_indented_code_blocks_keep_rails_on_continuation_lines() {
    let text = "代码片段:\n  ```rust\n  const FRAME_DEADLINE: std::time::Duration = std::time::Duration::from_secs(10);\n  ```";
    let mut layout = TranscriptLayout::default();
    layout.update(&[TranscriptItem::Assistant(text.into())], None, 45);

    let code_row = layout.entries[0]
        .rows
        .iter()
        .find(|row| row.height > 1 && row.line.to_string().contains("FRAME_DEADLINE"))
        .expect("code line should wrap into multiple rows");

    let gutter = code_row
        .gutter
        .as_ref()
        .expect("indented code block retains gutter");
    assert!(gutter.width >= 4);
    assert_eq!(gutter.first.to_string(), gutter.continuation.to_string());
    assert!(gutter.continuation.to_string().contains('│'));

    let mut terminal = Terminal::new(TestBackend::new(45, 6)).unwrap();
    terminal
        .draw(|frame| layout.draw(frame, frame.area(), 0))
        .unwrap();
    let buffer = terminal.backend().buffer();

    let rail_col = gutter.continuation.to_string().find('│').unwrap();
    assert_eq!(buffer[(rail_col as u16, 2)].symbol(), "│");
    assert_eq!(buffer[(rail_col as u16, 3)].symbol(), "│");
}
