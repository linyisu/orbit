use crossterm::event::MouseEvent;
use ratatui::{Terminal, backend::TestBackend};

use super::*;
use crate::tui::{
    app::{TranscriptItem, UiState},
    render,
};

fn mouse(kind: MouseEventKind, x: u16, y: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    })
}

fn draw(state: &mut UiState, terminal: &mut Terminal<TestBackend>) {
    terminal
        .draw(|frame| {
            render::draw(frame, state);
            state.selection.paint(frame);
        })
        .unwrap();
}

fn setup() -> (UiState, Terminal<TestBackend>) {
    let mut state = UiState::new("footer-model");
    state
        .transcript
        .push(TranscriptItem::Assistant(
            (0..40)
                .map(|i| format!("line{i:02}\n"))
                .collect(),
        ));
    state.input.set("你好 draft".into());
    state.follow_bottom = false;
    let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
    draw(&mut state, &mut terminal);
    state.scroll = state.max_scroll;
    draw(&mut state, &mut terminal);

    (state, terminal)
}

fn select_transcript(items: &[TranscriptItem], width: u16, expanded: &[usize]) -> Selection {
    let mut layout = super::super::transcript::TranscriptLayout::default();
    layout.update_expanded(
        items,
        &expanded.iter().copied().collect(),
        &Default::default(),
        width,
    );
    let mut selection = Selection {
        viewport: Rect::new(3, 2, width, layout.height() as u16),
        region: Some(Region::Transcript),
        anchor: Some((0, 0)),
        end: Some((width - 1, layout.height() - 1)),
        moved: true,
        ..Default::default()
    };

    selection.transcript_view(0, &layout);
    selection
}

#[test]
fn copying_code_omits_generated_frame_but_keeps_source_indentation_and_blank_lines() {
    let source = "fn main() {\n    let glyphs = \"│─╭╮┊\";\n\n    println!(\"你好 👩‍💻\");\n}";
    let mut selection = select_transcript(
        &[TranscriptItem::Assistant(format!("```rust\n{source}\n```"))],
        80,
        &[],
    );

    assert_eq!(selection.text().trim_end_matches('\n'), source);

    selection.end = Some((79, 0));
    assert_eq!(selection.text(), "");
}

#[test]
fn copying_thinking_and_nested_code_omits_both_rails_even_after_wrapping() {
    let source = "先检查你好 👩‍💻\n\n```text\n    保留缩进与正文 │─\n\n最后一行\n```\n检查完成";
    for width in [16, 80] {
        let selection = select_transcript(
            &[TranscriptItem::Thinking(source.to_owned().into())],
            width,
            &[],
        );
        let copied = selection.text();
        assert_eq!(
            copied.replace(['\n', ' '], ""),
            "先检查你好👩‍💻保留缩进与正文│─最后一行检查完成"
        );
        if width == 80 {
            assert_eq!(
                copied.trim_end_matches('\n'),
                "先检查你好 👩‍💻\n\n    保留缩进与正文 │─\n\n最后一行\n检查完成"
            );
        }

        assert!(copied.contains("    保留"));
        assert!(!copied.contains(['┊', '╭', '╰']));
        assert!(copied.contains("👩‍💻\n\n"));
        assert!(copied.contains("\n\n最后一行"));
    }
}

#[test]
fn copying_html_keeps_unsupported_tags_and_omits_supported_tags_and_thinking_rails() {
    let source = "<b>中文 H<sub>2</sub>O<br>x<sup>2</sup></b>";
    for (item, expected) in [
        (
            TranscriptItem::Assistant(source.into()),
            "  中文 H<sub>2</sub>O\n  x<sup>2</sup>",
        ),
        (
            TranscriptItem::Thinking(source.to_owned().into()),
            "中文 H<sub>2</sub>O\nx<sup>2</sup>",
        ),
    ] {
        let selection = select_transcript(&[item], 80, &[]);
        assert_eq!(selection.text().trim_end_matches('\n'), expected);
    }
}

#[test]
fn copying_html_blocks_omits_generated_spacing_and_rails_and_keeps_literal_glyphs() {
    let source = "<details open><summary>摘要</summary>\n<blockquote>引用 │原文</blockquote>\n<div \
        style='border-left:4px solid #3498db; padding:12px;'>提示</div>\n</details>\n<p \
        style='text-align:center;'>结束</p>";
    let selection = select_transcript(&[TranscriptItem::Assistant(source.into())], 80, &[]);
    let copied = selection.text();
    assert!(copied.contains("摘要"));
    assert!(copied.contains("引用 │原文"));
    assert!(copied.contains("提示"));
    assert!(copied.contains("结束"));
    assert!(!copied.contains('▎'));
    assert!(!copied.contains(super::super::icons::ARROW_DOWN));
    assert!(!copied.contains(&" ".repeat(20)));
}

#[test]
fn partial_and_reversed_copy_include_whole_wide_glyphs_without_the_code_rail() {
    let mut selection = select_transcript(
        &[TranscriptItem::Assistant("```text\n你好 👩‍💻 │─\n```".into())],
        40,
        &[],
    );

    for (anchor, end) in [((5, 1), (7, 1)), ((7, 1), (5, 1))] {
        selection.anchor = Some(anchor);
        selection.end = Some(end);
        assert_eq!(selection.text(), "你好");
    }

    selection.anchor = Some((0, 1));
    selection.end = Some((7, 1));
    assert_eq!(selection.text(), "你好");
}

#[test]
fn copying_wrapped_user_input_omits_consumed_spaces_and_frame() {
    let source = format!("第一句{}https://orbit.example/demo", " ".repeat(85));
    let user = select_transcript(&[TranscriptItem::User(source)], 46, &[]);
    assert_eq!(
        user.text().trim_end_matches('\n'),
        "第一句\nhttps://orbit.example/demo"
    );
}

#[test]
fn copying_message_frames_keeps_literal_borders_and_body_blank_lines() {
    let source = "    first │─╭╮\n\n    second 你好";
    let user = select_transcript(&[TranscriptItem::User(source.into())], 60, &[]);
    assert_eq!(user.text().trim_end_matches('\n'), source);
}

#[test]
fn copying_tables_omits_grid_rows_and_keeps_columns_separate() {
    let selection = select_transcript(
        &[TranscriptItem::Assistant(
            "| 名称 | 值 |\n|---|---|\n| Orbit | 你好 |".into(),
        )],
        60,
        &[],
    );

    assert_eq!(
        selection
            .text()
            .trim_end_matches('\n')
            .lines()
            .map(|line| line
                .split_whitespace()
                .collect::<Vec<_>>())
            .collect::<Vec<_>>(),
        [vec!["名称", "值"], vec!["Orbit", "你好"]]
    );
}

#[test]
fn selected_markdown_and_code_keep_colors_and_wide_glyphs_through_redraws() {
    use ratatui::style::{Color, Modifier};

    let mut state = UiState::new("orbit-model");
    state
        .transcript
        .push(TranscriptItem::Assistant(
            "Orbit **加粗** _斜体_ [文档](https://orbit.example/docs) 👩‍💻\n\n```python\n# \
                Orbit\nprint('Orbit')\n```"
                .into(),
        ));
    state.follow_bottom = false;
    let mut terminal = Terminal::new(TestBackend::new(70, 24)).unwrap();
    draw(&mut state, &mut terminal);
    let before = terminal.backend().buffer().clone();
    let area = state.selection.viewport;
    state
        .selection
        .handle(&mouse(
            MouseEventKind::Down(MouseButton::Left),
            area.x,
            area.y,
        ))
        .unwrap();
    state
        .selection
        .handle(&mouse(
            MouseEventKind::Drag(MouseButton::Left),
            area.right() - 1,
            area.bottom() - 1,
        ))
        .unwrap();
    let copied = state.selection.text();
    assert!(copied.contains("加粗"));
    assert!(copied.contains("👩‍💻"));
    assert!(copied.contains("print('Orbit')"));

    for _ in 0..3 {
        let mut painted = None;
        terminal
            .draw(|frame| {
                assert!(state.selection.draw_frozen(frame));
                painted = Some(frame.buffer_mut().clone());
            })
            .unwrap();
        let selected = painted.unwrap();
        let mut colored = 0;
        let mut wide = 0;

        for y in area.y..area.bottom() {
            let mut x = area.x;
            while x < area.right() {
                let original = &before[(x, y)];
                let cell = &selected[(x, y)];
                let width = original.symbol().width().max(1) as u16;
                assert_eq!(cell.symbol(), original.symbol());
                assert_eq!(terminal.backend().buffer()[(x, y)].bg, cell.bg);
                if cell.bg == super::super::theme::THEME.selection {
                    assert_eq!(
                        cell.modifier,
                        original.modifier - (Modifier::REVERSED | Modifier::DIM)
                    );
                    if let Color::Rgb(red, green, blue) = original.fg
                        && let Color::Rgb(selected_red, selected_green, selected_blue) = cell.fg
                        && red.max(green).max(blue) - red.min(green).min(blue) > 8
                    {
                        assert_eq!(red.cmp(&green), selected_red.cmp(&selected_green));
                        assert_eq!(red.cmp(&blue), selected_red.cmp(&selected_blue));
                        assert_eq!(green.cmp(&blue), selected_green.cmp(&selected_blue));
                        colored += 1;
                    }

                    for column in x + 1..(x + width).min(area.right()) {
                        assert_eq!(selected[(column, y)].bg, cell.bg);
                        assert_eq!(selected[(column, y)].fg, cell.fg);
                        assert_eq!(selected[(column, y)].symbol(), before[(column, y)].symbol());
                        wide += 1;
                    }
                } else {
                    assert_eq!(cell, original);
                }

                x += width;
            }
        }

        assert!(colored > 10);
        assert!(wide >= 7);
        assert_eq!(state.selection.text(), copied);
    }

    state.selection.clear();
    draw(&mut state, &mut terminal);
    assert_eq!(terminal.backend().buffer(), &before);
}

#[test]
fn selection_leaves_frames_table_grid_and_padding_unchanged() {
    use super::super::app::{ThinkingRow, ToolRow, ToolStatus};
    use super::super::theme::THEME;
    use serde_json::json;

    let source = "| 项 | 值 |\n|---|---|\n| Orbit | **重点** 与 `code` |\n\n```rust\nlet rail = \
        \"│─\";\n```";
    for width in [28, 72, 100] {
        let mut state = UiState::new("orbit-model");
        state
            .transcript
            .push(TranscriptItem::User("Orbit 框选 👩‍💻".into()));

        state
            .transcript
            .push(TranscriptItem::Tool(ToolRow {
                changes: Vec::new(),
                purpose: None,
                id: "orbit-read".into(),
                name: "ReadFile".into(),
                arguments: json!({
                    "path": "ORBIT.md"
                })
                .to_string(),
                status: ToolStatus::Finished,
                output: json!({
                    "path": "ORBIT.md",
                    "content": source
                        .lines()
                        .enumerate()
                        .map(|(index, line)| format!("{}: {line}\n", index + 1))
                        .collect::<String>()
                })
                .to_string(),
            }));
        let mut thinking = ThinkingRow::from("```rust\nlet rail = \"│─\";\n```".to_owned());
        thinking.finished = true;
        state
            .transcript
            .push(TranscriptItem::Thinking(thinking));
        state
            .transcript
            .push(TranscriptItem::Assistant(format!("{source}\n\n---\n")));
        state.expanded.insert(2);
        state.follow_bottom = false;
        let mut terminal = Terminal::new(TestBackend::new(width, 70)).unwrap();
        draw(&mut state, &mut terminal);
        state.scroll = state.max_scroll;
        draw(&mut state, &mut terminal);
        let before = terminal.backend().buffer().clone();
        let area = state.selection.viewport;
        state
            .selection
            .handle(&mouse(
                MouseEventKind::Down(MouseButton::Left),
                area.x,
                area.y,
            ))
            .unwrap();
        state
            .selection
            .handle(&mouse(
                MouseEventKind::Drag(MouseButton::Left),
                area.right() - 1,
                area.bottom() - 1,
            ))
            .unwrap();
        terminal
            .draw(|frame| {
                assert!(state.selection.draw_frozen(frame));
            })
            .unwrap();
        let after = terminal.backend().buffer();
        let mut borders = 0;
        let mut literal_rails = 0;
        let mut selected = 0;
        let mut padding = 0;
        let mut badges = 0;
        for y in area.y..area.bottom() {
            let mut has_text = false;
            for x in area.x..area.right() {
                let cell = &before[(x, y)];
                let painted = &after[(x, y)];
                let literal =
                    cell.symbol() == "│" && x > area.x && before[(x - 1, y)].symbol() == "\""
                        || cell.symbol() == "─"
                            && x + 1 < area.right()
                            && before[(x + 1, y)].symbol() == "\"";

                if literal {
                    assert_eq!(painted.bg, THEME.selection);
                    literal_rails += 1;
                } else if cell
                    .symbol()
                    .chars()
                    .any(|ch| "│─╭╮╰╯┌┐└┘├┤┼┬┴┊▎".contains(ch))
                {
                    assert_eq!(
                        painted, cell,
                        "border changed at {x},{y} with width {width}"
                    );
                    borders += 1;
                }

                if !cell.symbol().trim().is_empty() {
                    has_text = true;
                }
                if painted.bg != cell.bg {
                    assert_eq!(painted.bg, THEME.selection);
                    selected += 1;
                }
            }

            for x in area.x + 1..area.right().saturating_sub(4) {
                if ["r", "u", "s", "t"]
                    .iter()
                    .enumerate()
                    .all(|(i, text)| before[(x + i as u16, y)].symbol() == *text)
                    && before[(x, y)].bg != ratatui::style::Color::Reset
                {
                    for column in x - 1..x + 5 {
                        assert_eq!(after[(column, y)], before[(column, y)]);
                    }
                    badges += 1;
                }
            }

            for x in area.x..area.right().saturating_sub(5) {
                if ["O", "r", "b", "i", "t"]
                    .iter()
                    .enumerate()
                    .all(|(i, text)| before[(x + i as u16, y)].symbol() == *text)
                    && let Some(border) =
                        (x + 5..area.right()).find(|&column| before[(column, y)].symbol() == "│")
                    && (x + 5..border).all(|column| {
                        before[(column, y)]
                            .symbol()
                            .trim()
                            .is_empty()
                    })
                {
                    for column in x + 5..border {
                        assert_eq!(after[(column, y)], before[(column, y)]);
                        padding += 1;
                    }
                }
            }

            if !has_text {
                for x in area.x..area.right() {
                    assert_eq!(after[(x, y)], before[(x, y)]);
                }
            }

            let tail = area.right() - 1;
            if before[(tail, y)]
                .symbol()
                .trim()
                .is_empty()
                && before[(tail - 1, y)].symbol().width() <= 1
            {
                assert_eq!(after[(tail, y)], before[(tail, y)]);
            }
        }

        assert!(borders > 50);
        assert!(literal_rails >= 2);
        assert!(selected > 20);
        assert!(padding > 0);
        assert!(badges >= 2);
        let copied = state.selection.text();
        assert!(copied.contains("│─"));
        assert!(!copied.contains(['╭', '╮', '╰', '╯', '┌', '┐', '└', '┘', '┊']));
        state.selection.clear();
        draw(&mut state, &mut terminal);
        assert_eq!(terminal.backend().buffer(), &before);
    }
}

#[test]
fn press_does_not_select_until_pointer_moves() {
    let (mut state, mut terminal) = setup();
    let area = state.selection.viewport;
    let point = (area.x + 2, area.y);
    let before = terminal.backend().buffer().clone();
    state
        .selection
        .handle(&mouse(
            MouseEventKind::Down(MouseButton::Left),
            point.0,
            point.1,
        ))
        .unwrap();
    terminal
        .draw(|frame| assert!(state.selection.draw_frozen(frame)))
        .unwrap();

    assert_eq!(terminal.backend().buffer(), &before);
    assert!(state.selection.text().is_empty());
    assert_eq!(
        state
            .selection
            .auto_scroll(Instant::now()),
        0
    );

    let release = mouse(MouseEventKind::Up(MouseButton::Left), point.0, point.1);
    assert_eq!(state.selection.click(&release), Some(point));
    state
        .selection
        .handle(&release)
        .unwrap();
    assert!(!state.selection.active());

    state
        .selection
        .handle(&mouse(
            MouseEventKind::Down(MouseButton::Left),
            point.0,
            point.1,
        ))
        .unwrap();
    state
        .selection
        .handle(&mouse(
            MouseEventKind::Drag(MouseButton::Left),
            point.0 + 3,
            point.1,
        ))
        .unwrap();
    terminal
        .draw(|frame| assert!(state.selection.draw_frozen(frame)))
        .unwrap();

    assert_eq!(
        terminal.backend().buffer()[point].bg,
        crate::tui::theme::THEME.selection
    );
    assert!(!state.selection.text().is_empty());
    assert!(
        state
            .selection
            .click(&release)
            .is_none()
    );
}

#[test]
fn dismissing_a_selection_keeps_reasoning_open_until_the_next_click() {
    use crate::tui::app::ThinkingRow;
    let mut state = UiState::new("fixture");
    let mut thinking = ThinkingRow::from("Orbit output\nsecond line".to_owned());
    thinking.finished = true;
    state
        .transcript
        .push(TranscriptItem::Thinking(thinking));
    state.expanded.insert(0);
    state.follow_bottom = false;
    let mut terminal = Terminal::new(TestBackend::new(70, 24)).unwrap();
    draw(&mut state, &mut terminal);
    let area = state.selection.viewport;
    let output_y = (area.top()..area.bottom())
        .find(|&y| {
            (area.left()..area.right())
                .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                .collect::<String>()
                .contains("Orbit output")
        })
        .unwrap();
    let point = (area.x + 4, output_y);

    state
        .selection
        .handle(&mouse(
            MouseEventKind::Down(MouseButton::Left),
            point.0,
            point.1,
        ))
        .unwrap();
    state
        .selection
        .handle(&mouse(
            MouseEventKind::Drag(MouseButton::Left),
            point.0 + 8,
            point.1,
        ))
        .unwrap();
    let release = mouse(MouseEventKind::Up(MouseButton::Left), point.0 + 8, point.1);
    assert!(
        state
            .selection
            .click(&release)
            .is_none()
    );
    state
        .selection
        .handle(&release)
        .unwrap();
    draw(&mut state, &mut terminal);
    assert!(!state.selection.text().is_empty());

    let release = mouse(MouseEventKind::Up(MouseButton::Left), point.0, point.1);
    state
        .selection
        .handle(&mouse(
            MouseEventKind::Down(MouseButton::Left),
            point.0,
            point.1,
        ))
        .unwrap();
    if let Some(point) = state.selection.click(&release) {
        super::super::toggle_transcript_at(&mut state, point);
    }
    state
        .selection
        .handle(&release)
        .unwrap();
    draw(&mut state, &mut terminal);
    assert!(
        state.expanded.contains(&0),
        "the dismissal click must not fold the tool"
    );
    assert!(!state.selection.active());

    state
        .selection
        .handle(&mouse(
            MouseEventKind::Down(MouseButton::Left),
            point.0,
            point.1,
        ))
        .unwrap();
    let point = state.selection.click(&release).unwrap();
    assert!(super::super::toggle_transcript_at(&mut state, point));
    assert!(!state.expanded.contains(&0));
    assert!(state.expanded_tool_groups.is_empty());
}

#[test]
fn replacing_a_selection_by_drag_and_clicking_the_input_remain_immediate() {
    let (mut state, mut terminal) = setup();
    let area = state.selection.viewport;
    for (start, end) in [(area.x + 2, area.x + 6), (area.x + 4, area.x + 8)] {
        for event in [
            mouse(MouseEventKind::Down(MouseButton::Left), start, area.y),
            mouse(MouseEventKind::Drag(MouseButton::Left), end, area.y),
            mouse(MouseEventKind::Up(MouseButton::Left), end, area.y),
        ] {
            assert!(state.selection.click(&event).is_none());
            state.selection.handle(&event).unwrap();
        }

        draw(&mut state, &mut terminal);
        assert!(state.selection.active());
        assert!(!state.selection.text().is_empty());
    }

    assert_eq!(state.selection.text(), "ne00");

    let input = state.input_area;
    let point = (input.x + 3, input.y);
    state
        .selection
        .handle(&mouse(
            MouseEventKind::Down(MouseButton::Left),
            point.0,
            point.1,
        ))
        .unwrap();
    let release = mouse(MouseEventKind::Up(MouseButton::Left), point.0, point.1);
    assert!(
        state
            .selection
            .click(&release)
            .is_none()
    );
    state
        .selection
        .handle(&release)
        .unwrap();
    assert!(!state.selection.active());

    let release = mouse(MouseEventKind::Up(MouseButton::Left), area.x + 2, area.y);
    state
        .selection
        .handle(&mouse(
            MouseEventKind::Down(MouseButton::Left),
            area.x + 2,
            area.y,
        ))
        .unwrap();
    assert_eq!(state.selection.click(&release), Some((area.x + 2, area.y)));
}

#[test]
fn held_wheel_extends_across_skipped_rows_and_release_stops_extension() {
    let (mut state, mut terminal) = setup();
    let area = state.selection.viewport;
    let (x, y) = (area.x + 2, area.y);
    state
        .selection
        .handle(&mouse(MouseEventKind::Down(MouseButton::Left), x, y))
        .unwrap();
    state
        .selection
        .handle(&mouse(MouseEventKind::ScrollDown, x + 5, y))
        .unwrap();

    assert!(state.selection.dragging());

    state.scroll_down(8);
    draw(&mut state, &mut terminal);

    assert_eq!(
        state.selection.text(),
        (0..=8)
            .map(|i| if i == 0 {
                "line00".into()
            } else {
                format!("  line{i:02}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    );

    state
        .selection
        .handle(&mouse(MouseEventKind::Up(MouseButton::Left), x + 5, y))
        .unwrap();
    let text = state.selection.text();
    state
        .selection
        .handle(&mouse(MouseEventKind::ScrollDown, x, y))
        .unwrap();
    state.scroll_down(8);
    draw(&mut state, &mut terminal);

    assert_eq!(state.selection.text(), text);
    assert!(!state.selection.dragging());

    state
        .selection
        .handle(&mouse(MouseEventKind::ScrollUp, x, y))
        .unwrap();
    state.scroll_up(8);
    draw(&mut state, &mut terminal);

    assert_eq!(
        terminal.backend().buffer()[(x, y)].bg,
        crate::tui::theme::THEME.selection
    );
}

#[test]
fn boundary_timer_repeats_without_motion_and_stops_on_release() {
    let (mut state, mut terminal) = setup();
    let area = state.selection.viewport;
    state
        .selection
        .handle(&mouse(
            MouseEventKind::Down(MouseButton::Left),
            area.x,
            area.y,
        ))
        .unwrap();
    state
        .selection
        .handle(&mouse(
            MouseEventKind::Drag(MouseButton::Left),
            area.x + 5,
            9,
        ))
        .unwrap();
    let now = Instant::now();

    for i in 0..3 {
        assert_eq!(
            state
                .selection
                .auto_scroll(now + Duration::from_millis(i * 100)),
            1
        );

        state.scroll_down(2);
        draw(&mut state, &mut terminal);
    }

    assert!(
        state
            .selection
            .text()
            .contains("line06")
    );

    state
        .selection
        .handle(&mouse(MouseEventKind::Drag(MouseButton::Left), area.x, 0))
        .unwrap();

    assert_eq!(
        state
            .selection
            .auto_scroll(now + Duration::from_millis(400)),
        -1
    );

    state.scroll_up(2);
    draw(&mut state, &mut terminal);
    state
        .selection
        .handle(&mouse(MouseEventKind::Up(MouseButton::Left), area.x, 0))
        .unwrap();

    assert_eq!(
        state
            .selection
            .auto_scroll(now + Duration::from_secs(1)),
        0
    );
}

#[test]
fn selection_stays_in_starting_region_and_excludes_chrome() {
    let (mut state, mut terminal) = setup();

    for region in [Region::Transcript, Region::Footer] {
        state.selection.clear();
        draw(&mut state, &mut terminal);
        let area = match region {
            Region::Transcript => state.selection.viewport,
            _ => state.selection.footer,
        };

        state
            .selection
            .handle(&mouse(
                MouseEventKind::Down(MouseButton::Left),
                area.x,
                area.y,
            ))
            .unwrap();
        state
            .selection
            .handle(&mouse(MouseEventKind::Drag(MouseButton::Left), 39, 9))
            .unwrap();
        terminal
            .draw(|frame| assert!(state.selection.draw_frozen(frame)))
            .unwrap();

        for y in 0..10 {
            for x in 0..40 {
                if !area.contains((x, y).into()) {
                    assert_ne!(
                        terminal.backend().buffer()[(x, y)].bg,
                        crate::tui::theme::THEME.selection
                    );
                }
            }
        }

        let text = state.selection.text();

        match region {
            Region::Transcript => {
                assert!(text.contains("line00"));
                assert!(!text.contains("draft"));
                assert!(!text.contains("footer-model"));
                assert!(!text.contains("Orbit"));
            }

            Region::Footer => {
                assert!(text.contains("/96k"));
                assert!(!text.contains("draft"));
            }

            _ => unreachable!(),
        }
    }
}
