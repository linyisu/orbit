use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    Terminal,
    backend::TestBackend,
    buffer::{Buffer, CellWidth},
};

use super::super::{app::UiState, transcript::TranscriptLayout};
use super::*;

fn tool(index: usize, name: &str, status: ToolStatus) -> TranscriptItem {
    TranscriptItem::Tool(ToolRow {
        id: format!("orbit-{index}"),
        name: name.into(),
        status,
        arguments: if name == "read_agent" {
            serde_json::json!({"agent_id":"orbit-agent","message_id":100+index,"offset":index*100})
        } else {
            serde_json::json!({"path":"orbit.rs","start_line":index+1})
        }
        .to_string(),
        output: format!("Orbit 结果 {index}\nhttps://orbit.test/result/{index}"),
    })
}

fn render(state: &mut UiState, width: u16, height: u16) -> Buffer {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| super::super::render::draw(frame, state))
        .unwrap();
    terminal.backend().buffer().clone()
}

fn text(buffer: &Buffer) -> String {
    let mut text = String::new();
    for y in buffer.area.y..buffer.area.bottom() {
        let mut x = buffer.area.x;
        while x < buffer.area.right() {
            let cell = &buffer[(x, y)];
            text.push_str(cell.symbol());
            x += cell.cell_width().max(1);
        }
        text.push('\n');
    }
    text
}

fn point(state: &UiState, index: usize, group: bool) -> (u16, u16) {
    let area = state.selection.viewport;
    let offset = state.max_scroll.saturating_sub(state.scroll);
    for y in area.y..area.bottom() {
        for x in area.x..area.right() {
            let hit = if group {
                state.transcript_layout.group_at(area, offset, (x, y))
            } else {
                state
                    .transcript_layout
                    .disclosure_at(&state.transcript, area, offset, (x, y))
            };
            if hit == Some(index) {
                return (x, y);
            }
        }
    }
    panic!("Orbit disclosure {index} is not visible");
}

#[test]
fn grouping_compares_operation_targets_and_ignores_only_read_pagination() {
    let cases = [
        (
            "read_file",
            json!({"path":"Orbit.md","start_line":1}),
            json!({"path":"Orbit.md","cursor":{"version":"orbit-version","offset":400,"line":50,"character":0}}),
            true,
        ),
        (
            "read_file",
            json!({"path":"Orbit.md"}),
            json!({"path":"Orbit.rs"}),
            false,
        ),
        (
            "write_file",
            json!({"path":"Orbit.rs","content":"a"}),
            json!({"path":"Orbit.rs","content":"b"}),
            true,
        ),
        (
            "edit_file",
            json!({"path":"Orbit.rs"}),
            json!({"path":"Other.rs"}),
            false,
        ),
        (
            "read_image",
            json!({"path":"Orbit.png"}),
            json!({"path":"Other.png"}),
            false,
        ),
        (
            "read_document",
            json!({"path":"Orbit.pdf"}),
            json!({"path":"Orbit.pdf","start_char":500}),
            true,
        ),
        (
            "read_document",
            json!({"path":"Orbit.pdf"}),
            json!({"path":"Other.pdf"}),
            false,
        ),
        (
            "list_files",
            json!({}),
            json!({"path":".","depth":1,"offset":200}),
            true,
        ),
        (
            "list_files",
            json!({"path":"crates/orbit-cli"}),
            json!({"path":"crates/orbit-tools"}),
            false,
        ),
        (
            "search_text",
            json!({"query":"Orbit"}),
            json!({"query":"Orbit","path":".","offset":100,"context_lines":3}),
            true,
        ),
        (
            "search_text",
            json!({"query":"Orbit"}),
            json!({"query":"Other"}),
            false,
        ),
        (
            "search_text",
            json!({"query":"Orbit","path":"src"}),
            json!({"query":"Orbit","path":"docs"}),
            false,
        ),
        (
            "search_text",
            json!({"query":"Orbit"}),
            json!({"query":"Orbit","regex":true}),
            false,
        ),
        (
            "search_text",
            json!({"query":"Orbit"}),
            json!({"query":"Orbit","glob":"*.rs"}),
            false,
        ),
        (
            "project_overview",
            json!({"path":"."}),
            json!({"path":".","max_tokens":8000}),
            true,
        ),
        (
            "project_overview",
            json!({"path":"src","query":"Orbit"}),
            json!({"path":"src","query":"Other"}),
            false,
        ),
        (
            "web_fetch",
            json!({"url":"https://orbit.test/guide"}),
            json!({"url":"https://orbit.test/guide","start_char":100}),
            true,
        ),
        (
            "web_fetch",
            json!({"url":"https://orbit.test/guide"}),
            json!({"url":"https://orbit.test/api"}),
            false,
        ),
        (
            "web_search",
            json!({"query":"Orbit"}),
            json!({"query":"Orbit","max_results":10}),
            true,
        ),
        (
            "web_search",
            json!({"query":"Orbit"}),
            json!({"query":"Other"}),
            false,
        ),
        (
            "read_agent",
            json!({"agent_id":"orbit-explore","message_id":1}),
            json!({"agent_id":"orbit-explore","message_id":2,"offset":300}),
            true,
        ),
        (
            "read_agent",
            json!({"agent_id":"orbit-explore"}),
            json!({"agent_id":"orbit-review"}),
            false,
        ),
        (
            "read_agent",
            json!({}),
            json!({"agent_id":"orbit-explore"}),
            false,
        ),
        ("read_agent", json!({}), json!({"offset":32}), true),
        (
            "read_command_output",
            json!({"output_id":"orbit-log"}),
            json!({"output_id":"orbit-log","stream":"stdout","offset":100}),
            true,
        ),
        (
            "read_command_output",
            json!({"output_id":"orbit-log"}),
            json!({"output_id":"other-log"}),
            false,
        ),
        (
            "read_command_output",
            json!({"output_id":"orbit-log"}),
            json!({"output_id":"orbit-log","stream":"stderr"}),
            false,
        ),
        (
            "wait_agents",
            json!({"agent_ids":["orbit-a","orbit-b"]}),
            json!({"agent_ids":["orbit-b","orbit-a"]}),
            true,
        ),
        (
            "wait_agents",
            json!({"agent_ids":["orbit-a"]}),
            json!({"agent_ids":["orbit-b"]}),
            false,
        ),
        ("wait_agents", json!({}), json!({}), false),
        (
            "wait_commands",
            json!({"job_ids":["orbit-job"]}),
            json!({"job_ids":["other-job"]}),
            false,
        ),
        (
            "run_command",
            json!({"command":"cargo check"}),
            json!({"command":"cargo check","cwd":"."}),
            true,
        ),
        (
            "run_command",
            json!({"command":"cargo check"}),
            json!({"command":"cargo test"}),
            false,
        ),
        (
            "run_command",
            json!({"command":"cargo check","cwd":"crates/orbit-cli"}),
            json!({"command":"cargo check","cwd":"crates/orbit-tools"}),
            false,
        ),
        (
            "git_inspect",
            json!({"view":"diff","path":"Orbit.rs"}),
            json!({"view":"diff","path":"Orbit.rs","offset":100}),
            true,
        ),
        (
            "git_inspect",
            json!({"view":"diff","path":"Orbit.rs"}),
            json!({"view":"diff","path":"Other.rs"}),
            false,
        ),
        (
            "git_inspect",
            json!({"view":"diff"}),
            json!({"view":"diff","staged":true}),
            false,
        ),
        (
            "read_history",
            json!({"archive_id":1}),
            json!({"archive_id":2}),
            false,
        ),
        (
            "memory_search",
            json!({"query":"Orbit"}),
            json!({"query":"Other"}),
            false,
        ),
        (
            "task_update",
            json!({"action":"update","id":1}),
            json!({"action":"update","id":2}),
            false,
        ),
        (
            "memory_save",
            json!({"title":"Orbit"}),
            json!({"title":"Orbit"}),
            false,
        ),
        (
            "task_update",
            json!({"action":"create","title":"Orbit"}),
            json!({"action":"create","title":"Orbit"}),
            false,
        ),
        (
            "orbit_extension",
            json!({"path":"Orbit.rs"}),
            json!({"path":"Orbit.rs"}),
            false,
        ),
    ];
    for (name, first, second, grouped) in cases {
        let item = |args: Value| {
            TranscriptItem::Tool(ToolRow {
                id: "orbit-call".into(),
                name: name.into(),
                status: ToolStatus::Finished,
                arguments: args.to_string(),
                output: String::new(),
            })
        };
        let items = [item(first.clone()), item(second.clone())];
        assert_eq!(
            range(&items, 0),
            0..if grouped { 2 } else { 1 },
            "{name}: {first} vs {second}"
        );
        assert_eq!(
            start(&items, 1),
            if grouped { 0 } else { 1 },
            "{name}: {first} vs {second}"
        );
    }
}

#[test]
fn file_groups_keep_each_file_separate_and_name_their_targets() {
    let mut state = UiState::new("orbit-model");
    state.transcript = ["Orbit.md", "Orbit.md", "Orbit.rs", "Orbit.rs"]
        .into_iter()
        .enumerate()
        .map(|(i, path)| {
            TranscriptItem::Tool(ToolRow {
                id: format!("orbit-{i}"),
                name: "read_file".into(),
                status: ToolStatus::Finished,
                arguments: json!({"path":path,"start_line":i+1}).to_string(),
                output: String::new(),
            })
        })
        .collect();
    let screen = text(&render(&mut state, 100, 30));
    assert!(screen.contains("read_file · Orbit.md · 2 次调用"));
    assert!(screen.contains("read_file · Orbit.rs · 2 次调用"));
    assert_eq!(state.transcript_layout.group_range(1), Some(0..2));
    assert_eq!(state.transcript_layout.group_range(2), Some(2..4));
    let first = point(&state, 0, true);
    super::super::toggle_transcript_at(&mut state, first);
    let screen = text(&render(&mut state, 100, 30));
    assert!(screen.contains("Orbit.md[1..]"));
    assert!(screen.contains("Orbit.md[2..]"));
    assert_eq!(screen.matches('╭').count(), 1);
    assert!(!state.expanded_tool_groups.contains(&2));

    let path = format!("{}/Orbit  文档.md", "orbit/workspace/".repeat(12));
    for item in &mut state.transcript[..2] {
        let TranscriptItem::Tool(row) = item else {
            unreachable!()
        };
        row.arguments = json!({"path":path}).to_string();
    }
    let header = header(&state.transcript[..2], 50, false);
    assert!(
        header
            .line
            .to_string()
            .ends_with("Orbit  文档.md · 2 次调用")
    );
    assert!(header.line.width() <= 50);
}

#[test]
fn long_group_targets_leave_room_for_running_status_and_trailing_count() {
    let path = format!("{}/中文文档.rs", "orbit/workspace/".repeat(12));
    let items = [ToolStatus::Finished, ToolStatus::Running].map(|status| {
        TranscriptItem::Tool(ToolRow {
            id: "orbit-read".into(),
            name: "read_file".into(),
            status,
            arguments: json!({"path":path}).to_string(),
            output: String::new(),
        })
    });
    for width in [40, 60, 100] {
        for framed in [false, true] {
            let header = header(&items, width, framed);
            let text = header.line.to_string();
            assert!(header.line.width() <= usize::from(width), "{text}");
            assert!(
                text.trim_end_matches([' ', '│']).ends_with(" · 2 次调用"),
                "{text}"
            );
            assert!(text.contains(" · 1 进行中 · 2 次调用"), "{text}");
            assert!(text.contains('…'), "{text}");
            if width >= 60 {
                assert!(text.contains("中文文档.rs"), "{text}");
            }
        }
    }
}

#[test]
fn appending_different_targets_and_editing_targets_match_a_fresh_layout() {
    let mut items = Vec::new();
    let mut layout = TranscriptLayout::default();
    for (index, path) in ["Orbit.md", "Orbit.rs", "Orbit.rs", "Other.rs", "Other.rs"]
        .into_iter()
        .enumerate()
    {
        items.push(TranscriptItem::Tool(ToolRow {
            id: format!("orbit-{index}"),
            name: "read_file".into(),
            status: ToolStatus::Finished,
            arguments: json!({"path":path}).to_string(),
            output: String::new(),
        }));
        layout.update_expanded(&items, &Default::default(), &Default::default(), 100);
        let mut fresh = TranscriptLayout::default();
        fresh.update_expanded(&items, &Default::default(), &Default::default(), 100);
        assert_eq!(layout.height(), fresh.height(), "Orbit append {index}");
    }
    for (index, path) in [(0, "Orbit.rs"), (2, "Other.rs"), (1, "Split.rs")] {
        let TranscriptItem::Tool(row) = &mut items[index] else {
            unreachable!()
        };
        row.arguments = json!({"path":path}).to_string();
        layout.invalidate_from(index);
        layout.update_expanded(&items, &Default::default(), &Default::default(), 100);
        let mut fresh = TranscriptLayout::default();
        fresh.update_expanded(&items, &Default::default(), &Default::default(), 100);
        let area = ratatui::layout::Rect::new(0, 0, 100, 40);
        let mut actual = Buffer::empty(area);
        let mut expected = Buffer::empty(area);
        layout.draw_buffer(&mut actual, area, 0);
        fresh.draw_buffer(&mut expected, area, 0);
        assert_eq!(layout.height(), fresh.height(), "Orbit target {index}");
        assert_eq!(actual, expected, "Orbit target {index}");
    }
}

#[test]
fn grouped_calls_expand_inline_without_rewriting_history() {
    let mut state = UiState::new("orbit-model");
    state
        .transcript
        .push(TranscriptItem::User("检查 Orbit".into()));
    state
        .transcript
        .extend((0..8).map(|i| tool(i, "read_agent", ToolStatus::Finished)));
    let source: Vec<_> = state
        .transcript
        .iter()
        .filter_map(|item| {
            if let TranscriptItem::Tool(row) = item {
                Some((row.id.clone(), row.arguments.clone(), row.output.clone()))
            } else {
                None
            }
        })
        .collect();
    let collapsed = text(&render(&mut state, 120, 45));
    assert!(collapsed.contains("read_agent · 子代理 · 8 次调用"));
    assert_eq!(collapsed.matches("read_agent").count(), 1);
    let header = point(&state, 1, true);
    assert!(super::super::toggle_transcript_at(&mut state, header));
    let expanded = text(&render(&mut state, 120, 45));
    assert_eq!(expanded.matches("read_agent").count(), 9);
    assert!(!expanded.contains("Orbit 结果"));
    let call = point(&state, 4, false);
    assert!(super::super::toggle_transcript_at(&mut state, call));
    let detail = text(&render(&mut state, 120, 45));
    assert!(detail.contains("Orbit 结果 3"));
    assert!(!detail.contains("Orbit 结果 2"));
    let area = state.selection.viewport;
    let offset = state.max_scroll.saturating_sub(state.scroll);
    assert!((area.y..area.bottom()).any(|y| {
        (area.x..area.right()).any(|x| {
            state.transcript_layout.link_at(area, offset, (x, y))
                == Some("https://orbit.test/result/3")
        })
    }));
    let header = point(&state, 1, true);
    super::super::toggle_transcript_at(&mut state, header);
    assert!(state.expanded.is_empty());
    assert!(state.expanded_tool_groups.is_empty());
    assert_eq!(
        source,
        state
            .transcript
            .iter()
            .filter_map(|item| if let TranscriptItem::Tool(row) = item {
                Some((row.id.clone(), row.arguments.clone(), row.output.clone()))
            } else {
                None
            })
            .collect::<Vec<_>>()
    );
}

#[test]
fn grouped_calls_share_one_frame_with_connected_separators() {
    for width in [120, 50, 20] {
        for expanded in [vec![], vec![0], vec![1], vec![2], vec![0, 2], vec![0, 1, 2]] {
            let expanded = expanded
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>();
            let items = (0..3)
                .map(|i| tool(i, "read_agent", ToolStatus::Finished))
                .collect::<Vec<_>>();
            let mut layout = TranscriptLayout::default();
            layout.update_expanded(
                &items,
                &expanded,
                &std::collections::BTreeSet::from([0]),
                width,
            );
            let area = ratatui::layout::Rect::new(0, 0, width, layout.height() as u16);
            let mut buffer = Buffer::empty(area);
            layout.draw_buffer(&mut buffer, area, 0);
            let screen = text(&buffer);
            assert_eq!(screen.matches('╭').count(), 1);
            assert_eq!(screen.matches('╰').count(), 1);
            assert_eq!(
                screen.matches('├').count(),
                expanded.len() * 2 - usize::from(expanded.contains(&2))
            );
            assert!(screen.lines().next().unwrap().starts_with('╭'));
            assert!((1..width - 1).all(|x| buffer[(x, 2)].symbol().trim().is_empty()));
            assert!(layout.group_at(area, 0, (2, 2)).is_none());
            assert!(layout.disclosure_at(&items, area, 0, (2, 2)).is_none());
            if width >= 50 {
                assert!(
                    screen
                        .lines()
                        .nth(1)
                        .unwrap()
                        .contains("read_agent · 子代理 · 3 次调用")
                );
            }
            for i in 0..3 {
                assert_eq!(
                    screen.contains(&format!("Orbit 结果 {i}")),
                    expanded.contains(&i)
                );
            }
            if expanded.is_empty() {
                let titles = (0..area.bottom())
                    .filter(|&y| layout.disclosure_at(&items, area, 0, (2, y)).is_some())
                    .collect::<Vec<_>>();
                assert_eq!(titles, vec![3, 4, 5]);
            }

            for y in 0..area.bottom() {
                if matches!(buffer[(0, y)].symbol(), "╭" | "├" | "╰") {
                    assert!(layout.disclosure_at(&items, area, 0, (2, y)).is_none());
                }
                assert!(matches!(buffer[(0, y)].symbol(), "╭" | "│" | "├" | "╰"));
                assert!(matches!(
                    buffer[(width - 1, y)].symbol(),
                    "╮" | "│" | "┤" | "╯"
                ));
                for x in [0, width - 1] {
                    assert!(
                        layout
                            .decorations(y as usize)
                            .iter()
                            .any(|range| range.contains(&(x as usize)))
                    );
                    assert!(layout.group_at(area, 0, (x, y)).is_none());
                    if matches!(buffer[(x, y)].symbol(), "╭" | "├" | "╰" | "╮" | "┤" | "╯")
                    {
                        assert!(layout.disclosure_at(&items, area, 0, (x, y)).is_none());
                    }
                }
            }
            for i in expanded {
                let target = format!("https://orbit.test/result/{i}");
                assert!((area.y..area.bottom()).any(|y| {
                    (area.x..area.right())
                        .any(|x| layout.link_at(area, 0, (x, y)) == Some(target.as_str()))
                }));
            }
        }
    }
}

#[test]
fn individual_call_headers_toggle_independently_inside_the_group() {
    let mut state = UiState::new("orbit-model");
    state.transcript = (0..3)
        .map(|i| tool(i, "read_agent", ToolStatus::Finished))
        .collect();
    render(&mut state, 100, 60);
    let group = point(&state, 0, true);
    super::super::toggle_transcript_at(&mut state, group);
    render(&mut state, 100, 60);

    for i in [0, 2] {
        let call = point(&state, i, false);
        super::super::toggle_transcript_at(&mut state, call);
        render(&mut state, 100, 60);
    }
    let expanded = text(&render(&mut state, 100, 60));
    assert_eq!(expanded.matches('╭').count(), 1);
    assert_eq!(expanded.matches('╰').count(), 1);
    assert_eq!(expanded.matches('├').count(), 3);
    assert!(expanded.contains("Orbit 结果 0"));
    assert!(expanded.contains("Orbit 结果 2"));
    let call = point(&state, 0, false);
    super::super::toggle_transcript_at(&mut state, call);
    let collapsed = text(&render(&mut state, 100, 60));
    assert!(!collapsed.contains("Orbit 结果 0"));
    assert!(collapsed.contains("Orbit 结果 2"));
    assert!(state.expanded_tool_groups.contains(&0));
    assert_eq!(state.expanded, std::collections::BTreeSet::from([2]));
}

#[test]
fn framed_group_heading_keeps_its_borders_on_hover_and_closes_the_whole_group() {
    use ratatui::style::Modifier;

    let mut state = UiState::new("orbit-model");
    state.transcript = (0..2)
        .map(|i| tool(i, "read_agent", ToolStatus::Finished))
        .collect();
    state.expanded_tool_groups.insert(0);
    state.expanded.insert(1);
    let before = render(&mut state, 100, 60);
    let heading = point(&state, 0, true);
    let area = state.selection.viewport;
    let offset = state.max_scroll.saturating_sub(state.scroll);
    assert_eq!(
        state.transcript_layout.group_offset(0),
        Some(offset + usize::from(heading.1 - area.y))
    );
    state.transcript_pointer = Some(heading);
    let hovered = render(&mut state, 100, 60);
    for x in [area.x, area.right() - 1] {
        assert_eq!(before[(x, heading.1)], hovered[(x, heading.1)]);
    }
    assert!((area.x..area.right()).any(|x| {
        before[(x, heading.1)].fg == THEME.tool_finished
            && hovered[(x, heading.1)].fg == THEME.tool_hover
            && hovered[(x, heading.1)].modifier.contains(Modifier::BOLD)
    }));
    assert!(super::super::toggle_transcript_at(&mut state, heading));
    let collapsed = text(&render(&mut state, 100, 60));
    assert!(collapsed.contains("read_agent · 子代理 · 2 次调用"));
    assert_eq!(collapsed.matches("read_agent").count(), 1);
    assert!(!collapsed.contains('╭'));
    assert!(state.expanded_tool_groups.is_empty());
    assert!(state.expanded.is_empty());
}

#[test]
fn tool_frames_replace_adjacent_spacing_and_cached_toggles_match_fresh_layouts() {
    let mut search = tool(1, "search_text", ToolStatus::Finished);
    if let TranscriptItem::Tool(row) = &mut search {
        row.arguments = json!({"path":".","query":"Orbit"}).to_string();
    }
    let items = vec![
        TranscriptItem::Assistant("Orbit before".into()),
        tool(0, "read_file", ToolStatus::Finished),
        search,
        TranscriptItem::Assistant("Orbit after".into()),
    ];
    let mut layout = TranscriptLayout::default();
    for width in [80, 28, 80] {
        for expanded in [
            vec![],
            vec![1],
            vec![2],
            vec![1, 2],
            vec![],
            vec![2],
            vec![],
        ] {
            let expanded = expanded.into_iter().collect();
            layout.update_expanded(&items, &expanded, &Default::default(), width);
            let mut fresh = TranscriptLayout::default();
            fresh.update_expanded(&items, &expanded, &Default::default(), width);
            let area = ratatui::layout::Rect::new(0, 0, width, layout.height() as u16);
            let mut actual = Buffer::empty(area);
            let mut expected = Buffer::empty(area);
            layout.draw_buffer(&mut actual, area, 0);
            fresh.draw_buffer(&mut expected, area, 0);
            assert_eq!(layout.height(), fresh.height());
            assert_eq!(actual, expected);

            let screen = text(&actual);
            let lines = screen.lines().map(str::trim).collect::<Vec<_>>();
            let row = |label| lines.iter().position(|line| line.contains(label)).unwrap();
            let first_open = expanded.contains(&1);
            let second_open = expanded.contains(&2);
            let first_start = row("read_file") - usize::from(first_open);
            let second_start = row("search_text") - usize::from(second_open);
            let first_end = if first_open {
                lines[..second_start]
                    .iter()
                    .rposition(|line| line.starts_with('╰'))
                    .unwrap()
            } else {
                first_start
            };
            let second_end = if second_open {
                lines
                    .iter()
                    .rposition(|line| line.starts_with('╰'))
                    .unwrap()
            } else {
                second_start
            };
            for (start, end, gap) in [
                (
                    row("Orbit before") + 1,
                    first_start,
                    usize::from(!first_open),
                ),
                (
                    first_end + 1,
                    second_start,
                    usize::from(!first_open && !second_open),
                ),
                (
                    second_end + 1,
                    row("Orbit after"),
                    usize::from(!second_open),
                ),
            ] {
                assert_eq!(end - start, gap, "{width}: {expanded:?}\n{screen}");
                assert!(lines[start..end].iter().all(|line| line.is_empty()));
            }
        }
    }
}

#[test]
fn errors_messages_and_subagent_view_links_keep_calls_separate() {
    let items = vec![
        tool(0, "read_file", ToolStatus::Finished),
        tool(1, "read_file", ToolStatus::Running),
        tool(2, "read_file", ToolStatus::Failed),
        tool(3, "read_file", ToolStatus::Finished),
        TranscriptItem::Thinking("Orbit 思考".to_owned().into()),
        tool(5, "read_file", ToolStatus::Finished),
        tool(6, "search_text", ToolStatus::Finished),
        tool(7, "spawn_agent", ToolStatus::Finished),
        tool(8, "spawn_agent", ToolStatus::Finished),
    ];
    let mut state = UiState::new("orbit-model");
    state.transcript = items;
    let screen = text(&render(&mut state, 120, 45));
    assert!(screen.contains("read_file · orbit.rs · 1 进行中 · 2 次调用"));
    assert_eq!(screen.matches("read_file").count(), 4);
    assert_eq!(screen.matches("spawn_agent").count(), 2);
    assert!(state.transcript_layout.group_range(2).is_none());
    assert!(state.transcript_layout.group_range(7).is_none());
}

#[test]
fn incremental_groups_match_fresh_layout_after_append_failure_recovery_and_resize() {
    let mut items = vec![tool(0, "read_file", ToolStatus::Finished)];
    let mut layout = TranscriptLayout::default();
    let mut expanded_groups = std::collections::BTreeSet::new();
    for step in 0..12 {
        if step < 8 {
            items.push(tool(step + 1, "read_file", ToolStatus::Running));
        }
        if step == 8 || step == 9 {
            let TranscriptItem::Tool(row) = &mut items[4] else {
                unreachable!()
            };
            row.status = if step == 8 {
                ToolStatus::Failed
            } else {
                ToolStatus::Finished
            };
            layout.invalidate_from(4);
        }
        if step == 10 {
            expanded_groups.insert(0);
        }
        let widths: &[u16] = if step == 11 { &[90, 29, 8, 1] } else { &[90] };
        for &width in widths {
            layout.update_expanded(&items, &Default::default(), &expanded_groups, width);
            let mut fresh = TranscriptLayout::default();
            fresh.update_expanded(&items, &Default::default(), &expanded_groups, width);
            let area = ratatui::layout::Rect::new(0, 0, width, 60);
            let mut actual = Buffer::empty(area);
            let mut expected = Buffer::empty(area);
            layout.draw_buffer(&mut actual, area, 0);
            fresh.draw_buffer(&mut expected, area, 0);
            assert_eq!(layout.height(), fresh.height());
            assert_eq!(actual, expected, "Orbit step {step}, width {width}");
        }
    }
}

#[test]
fn ctrl_o_and_collapse_from_scrolled_details_return_to_the_group_header() {
    let mut state = UiState::new("orbit-model");
    state.transcript = (0..4)
        .map(|i| tool(i, "read_file", ToolStatus::Finished))
        .collect();
    render(&mut state, 90, 24);
    let key = Event::Key(KeyEvent::new(KeyCode::Char('o'), KeyModifiers::CONTROL));
    super::super::keyboard_action(&mut state, key.clone(), false);
    render(&mut state, 90, 24);
    assert!(state.expanded_tool_groups.contains(&0));
    super::super::keyboard_action(&mut state, key.clone(), false);
    render(&mut state, 90, 24);
    assert!(state.expanded_tool_groups.is_empty());
    super::super::keyboard_action(&mut state, key, false);
    let TranscriptItem::Tool(row) = &mut state.transcript[2] else {
        unreachable!()
    };
    row.output = "Orbit 长输出\n".repeat(100);
    state.invalidate_transcript(2);
    state.expanded.insert(2);
    render(&mut state, 90, 24);
    let child = state.transcript_layout.item_offset(2).unwrap();
    state.follow_bottom = false;
    state.scroll = state.max_scroll.saturating_sub(child + 15);
    render(&mut state, 90, 24);
    super::super::toggle_transcript_group(&mut state, 0);
    assert!(text(&render(&mut state, 90, 24)).contains("read_file · orbit.rs · 4 次调用"));
    assert_eq!(state.max_scroll.saturating_sub(state.scroll), 0);
}
