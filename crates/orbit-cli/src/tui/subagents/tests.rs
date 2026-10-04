use super::*;
use crossterm::event::{KeyEvent, MouseEvent};
use orbit_model::types::ToolCall;
use ratatui::{Terminal, backend::TestBackend};

fn mouse(kind: MouseEventKind, x: u16, y: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    })
}

#[test]
fn task_cards_preserve_status_statistics_links_and_shared_markdown() {
    for (status, expected) in [
        ("running", "read_file"),
        ("completed", "完成"),
        ("failed", "失败"),
        ("cancelled", "已取消"),
        ("interrupted", "已中断"),
        ("queued", "已排队"),
    ] {
        let row = ToolRow {
            id:"orbit-child".into(),name:"spawn_agent".into(),
            arguments:json!({"task":"审查中文边界 🦀 和长文件名","role":"review"}).to_string(),
            status:if status=="running" {ToolStatus::Running} else {ToolStatus::Finished},
            output:json!({"agent_id":"00000000000000000000000000000001","status":status,"model":"orbit::model","tool_calls":3,"elapsed_ms":12900,"_activity":{"current":"read_file crates/orbit-cli/src/tui/很长很长很长的文件名.rs"},"summary":"**检查完成**\n\n[证据](https://orbit.example/evidence)"}).to_string(),
        };
        for width in [10, 11, 12, 30, 80] {
            for expanded in [false, true] {
                let lines = super::super::render::tool_card_with_links(&row, expanded, width);
                assert!(
                    lines
                        .iter()
                        .all(|line| line.line.width() <= usize::from(width)),
                    "{status} expanded={expanded} width={width}: {:?}",
                    lines
                        .iter()
                        .map(|line| line.line.to_string())
                        .collect::<Vec<_>>()
                );
                assert!(
                    lines[usize::from(expanded)]
                        .links
                        .iter()
                        .any(|link| link.url.starts_with("orbit://session/"))
                );
                let header = &lines[usize::from(expanded)];
                let link = &header.links[0];
                let text = header.line.to_string();
                assert_eq!(&text[link.start..link.end], "查看");
                assert!(text[..link.start].ends_with(' '));
                assert!(text[link.end..].starts_with(' '));
                assert!(
                    header
                        .line
                        .spans
                        .iter()
                        .filter(|span| span.style.add_modifier.contains(Modifier::UNDERLINED))
                        .all(|span| span.content == "查看")
                );
                if width == 80 && expanded {
                    assert!(
                        lines
                            .iter()
                            .any(|line| line.line.to_string().contains(expected))
                    );
                    assert!(
                        lines
                            .iter()
                            .any(|line| line.line.to_string().contains("3 次调用 · 12s"))
                    );
                }
                if expanded {
                    assert!(lines.first().unwrap().line.to_string().starts_with('╭'));
                    assert!(lines.last().unwrap().line.to_string().starts_with('╰'));
                    assert!(lines[1..lines.len() - 1].iter().all(|line| {
                        let text = line.line.to_string();
                        text.starts_with('│') && text.ends_with('│')
                    }));
                } else {
                    assert_eq!(lines.len(), 1);
                    if width == 80 {
                        assert!(text.contains("3 次调用 · 12s"));
                    }
                }
                if expanded {
                    assert!(lines.iter().any(|line| {
                        line.links
                            .iter()
                            .any(|link| link.url == "https://orbit.example/evidence")
                    }));
                    let bold = lines
                        .iter()
                        .flat_map(|line| &line.line.spans)
                        .filter(|span| span.style.add_modifier.contains(Modifier::BOLD))
                        .map(|span| span.content.as_ref())
                        .collect::<String>();
                    assert!(bold.contains("检查完成"));
                }
            }
        }
    }
}

#[test]
fn subagent_results_and_rules_stay_inside_the_shared_tool_frame() {
    for width in [12, 40, 160] {
        let mut state = UiState::new("orbit::model");
        state.transcript.push(TranscriptItem::Tool(ToolRow {
            id: "orbit-report".into(),
            name: "spawn_agent".into(),
            arguments: json!({"task":"检查 Orbit 报告"}).to_string(),
            status: ToolStatus::Finished,
            output: json!({"agent_id":"00000000000000000000000000000001","status":"completed","summary":"# Orbit\n\n---\n\n```rust\n// Orbit 注释\nfn orbit() {}\n```"}).to_string(),
        }));
        state.expanded.insert(0);
        let mut terminal = Terminal::new(TestBackend::new(width + 6, 50)).unwrap();
        terminal
            .draw(|frame| super::super::render::draw(frame, &mut state))
            .unwrap();
        let viewport = state.selection.viewport;
        let buffer = terminal.backend().buffer();
        let mut found = false;
        for y in viewport.y..viewport.bottom() {
            for x in 0..buffer.area.width {
                let cell = &buffer[(x, y)];
                if cell.fg == THEME.md_rule && cell.symbol() == "─" {
                    found = true;
                    assert!(x >= viewport.x + 2 && x < viewport.right() - 2);
                    assert_eq!(buffer[(viewport.x, y)].symbol(), "│");
                    assert_eq!(buffer[(viewport.right() - 1, y)].symbol(), "│");
                    assert_eq!(buffer[(viewport.x, y)].fg, THEME.tool_border);
                }
            }
        }
        assert!(found, "width {width}");
    }
}

#[tokio::test]
async fn viewer_updates_live_keeps_parent_state_and_accepts_follow_up_drafts() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("orbit.db"))
        .await
        .unwrap();
    let parent = store
        .create(
            directory.path().to_string_lossy().into_owned(),
            "orbit::parent".into(),
            false,
        )
        .await
        .unwrap();
    let mut child = store
        .create_subagent(
            parent.id(),
            "orbit::child".into(),
            "review".into(),
            "检查 Orbit".into(),
        )
        .await
        .unwrap();
    child
        .begin("检查 Orbit".into(), "orbit::child".into())
        .await
        .unwrap();
    child
        .append(vec![AgentEvent::Model(StreamEvent::TextDelta(
            "首段 Orbit\n\n".into(),
        ))])
        .await
        .unwrap();
    let mut state = UiState::new("orbit::parent");
    state.submit("父任务".into());
    state
        .transcript
        .push(TranscriptItem::Assistant("Orbit 父任务\n".repeat(60)));
    state.input.set("Orbit 未发送草稿".into());
    let mut terminal = Terminal::new(TestBackend::new(141, 41)).unwrap();
    terminal
        .draw(|frame| super::super::render::draw(frame, &mut state))
        .unwrap();
    state.scroll_up(10);
    let parent_scroll = state.scroll;
    let parent_items = state.transcript.len();
    open(&mut state, child.id().into());
    let viewer = state.subagent_dialog.as_mut().unwrap();
    assert!(viewer.poll(&store, parent.id()).await);
    assert!(viewer.error.is_none());
    let call = ToolCall::function("orbit-read", "read_file", json!({"path":"src/orbit.rs"}));
    child
        .append(vec![
            AgentEvent::ToolStarted { call: call.clone() },
            AgentEvent::ToolFinished {
                call,
                content: "Orbit 工具结果".into(),
                changes: vec![],
            },
            AgentEvent::Model(StreamEvent::TextDelta(format!(
                "{}\n\n---\n\n末段 Orbit",
                "Orbit 记录\n".repeat(50)
            ))),
        ])
        .await
        .unwrap();
    assert!(viewer.poll(&store, parent.id()).await);
    child
        .finish("completed", vec![], Some(vec![]))
        .await
        .unwrap();
    assert!(viewer.poll(&store, parent.id()).await);
    assert_eq!(viewer.view.ui.status, RunStatus::Completed);
    assert_eq!(viewer.view.ui.tool_calls, 1);
    terminal
        .draw(|frame| super::super::render::draw(frame, &mut state))
        .unwrap();
    assert!(state.ime_cursor.is_some());
    let viewer = state.subagent_dialog.as_mut().unwrap();
    assert!(viewer.view.ui.max_scroll > 0);
    viewer
        .handle(&Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)))
        .unwrap();
    viewer
        .handle(&Event::Key(KeyEvent::new(
            KeyCode::Home,
            KeyModifiers::NONE,
        )))
        .unwrap();
    assert_eq!(viewer.view.ui.scroll, viewer.view.ui.max_scroll);
    let area = viewer.view.ui.selection.viewport;
    for event in [
        mouse(
            MouseEventKind::Down(MouseButton::Left),
            area.x + 2,
            area.y + 2,
        ),
        mouse(
            MouseEventKind::Drag(MouseButton::Left),
            area.x + 8,
            area.y + 2,
        ),
        mouse(
            MouseEventKind::Up(MouseButton::Left),
            area.x + 8,
            area.y + 2,
        ),
    ] {
        viewer.handle(&event).unwrap();
    }
    assert!(viewer.view.ui.selection.active());
    assert!(!viewer.poll(&store, parent.id()).await);
    viewer.handle(&Event::Paste("Orbit 追问".into())).unwrap();
    viewer
        .handle(&Event::Key(KeyEvent::new(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
        )))
        .unwrap();
    assert_eq!(viewer.view.ui.input.text, "Orbit 追问a");
    assert!(
        matches!(viewer.handle(&Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))).unwrap(), ViewerAction::Submit(text) if text == "Orbit 追问a")
    );
    assert!(matches!(
        viewer
            .handle(&Event::Key(KeyEvent::new(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL
            )))
            .unwrap(),
        ViewerAction::Close
    ));
    assert_eq!(state.model, "orbit::parent");
    assert_eq!(state.status, RunStatus::Running);
    assert_eq!(state.input.text, "Orbit 未发送草稿");
    assert_eq!(state.scroll, parent_scroll);
    assert_eq!(state.transcript.len(), parent_items);
    assert!(state.resume_request.is_none());
    assert!(!state.new_tab_request);
    assert!(store.load(parent.id()).await.unwrap().runs.is_empty());
    for (width, height) in [(40, 18), (10, 3), (1, 1)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| super::super::render::draw(frame, &mut state))
            .unwrap();
        if let Some(cursor) = state.ime_cursor {
            assert!(Rect::new(0, 0, width, height).contains(cursor));
        }
    }
}

#[tokio::test]
async fn viewer_follow_up_uses_the_existing_tool_and_keeps_failed_drafts() {
    use std::sync::{Arc, Mutex};
    let mut state = UiState::new("orbit-parent");
    state.input.set("Orbit 父会话草稿".into());
    open(&mut state, "orbit-child".into());
    let viewer = state.subagent_dialog.as_mut().unwrap();
    let mut tools = orbit_agent::tool::ToolRegistry::new();
    viewer.view.ui.input.set("Orbit 补充问题".into());
    viewer.send(&tools, "Orbit 补充问题".into()).await;
    assert_eq!(viewer.view.ui.input.text, "Orbit 补充问题");
    let received = Arc::new(Mutex::new(Vec::new()));
    let calls = received.clone();
    tools.register(orbit_agent::tool::ClosureTool::new(
        "send_agent_message",
        "",
        json!({}),
        move |args, _| {
            calls.lock().unwrap().push(args);
            async { Ok::<_, orbit_agent::tool::ToolError>("queued".into()) }
        },
    ));
    viewer.send(&tools, "Orbit 补充问题".into()).await;
    assert_eq!(
        received.lock().unwrap()[0],
        json!({"agent_id":"orbit-child","message":"Orbit 补充问题"})
    );
    assert!(viewer.view.ui.input.text.is_empty());
    assert_eq!(state.input.text, "Orbit 父会话草稿");
    assert!(state.transcript.is_empty());
}

#[tokio::test]
async fn viewer_replays_large_logs_in_batches_without_recovering_an_orphaned_writer() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("orbit.db"))
        .await
        .unwrap();
    let parent = store
        .create(
            directory.path().to_string_lossy().into_owned(),
            "orbit::parent".into(),
            false,
        )
        .await
        .unwrap();
    let mut child = store
        .create_subagent(
            parent.id(),
            "orbit::child".into(),
            "explore".into(),
            "Orbit 日志".into(),
        )
        .await
        .unwrap();
    let id = child.id().to_owned();
    child
        .begin("Orbit 日志".into(), "orbit::child".into())
        .await
        .unwrap();
    child
        .append(
            (0..5000)
                .map(|_| AgentEvent::Model(StreamEvent::TextDelta("轨".into())))
                .collect(),
        )
        .await
        .unwrap();
    drop(child);
    let mut state = UiState::new("orbit::parent");
    open(&mut state, id.clone());
    let viewer = state.subagent_dialog.as_mut().unwrap();
    assert!(viewer.poll(&store, parent.id()).await);
    let text_len = |viewer: &Viewer| {
        viewer
            .view
            .ui
            .transcript
            .iter()
            .filter_map(|item| {
                if let TranscriptItem::Assistant(text) = item {
                    Some(text.chars().count())
                } else {
                    None
                }
            })
            .sum::<usize>()
    };
    assert!(text_len(viewer) > 0 && text_len(viewer) < 5000);
    for _ in 0..100 {
        if !viewer.poll(&store, parent.id()).await {
            break;
        }
    }
    assert_eq!(text_len(viewer), 5000);
    assert_eq!(
        store.session_revision(&id).await.unwrap().unwrap().status,
        "running"
    );
    assert_eq!(store.load(&id).await.unwrap().runs[0].events.len(), 5000);
}

#[tokio::test]
async fn journal_progress_is_incremental_and_continuations_do_not_change_old_cards() {
    let directory = tempfile::tempdir().unwrap();
    let store = Store::open(directory.path().join("orbit.db"))
        .await
        .unwrap();
    let parent = store
        .create(
            directory.path().to_string_lossy().into_owned(),
            "orbit::model".into(),
            false,
        )
        .await
        .unwrap();
    let mut child = store
        .create_subagent(
            parent.id(),
            "orbit::model".into(),
            "review".into(),
            "检查输入".into(),
        )
        .await
        .unwrap();
    assert_eq!(
        store.subagent_parent(child.id()).await.unwrap().as_deref(),
        Some(parent.id())
    );
    assert!(store.subagent_parent(parent.id()).await.unwrap().is_none());
    child
        .begin("检查输入".into(), "orbit::model".into())
        .await
        .unwrap();
    let first_run = child.active_run_id().unwrap();
    let mut state = UiState::new("orbit::model");
    state.transcript.push(TranscriptItem::Tool(ToolRow {
        id: "orbit-spawn".into(),
        name: "spawn_agent".into(),
        arguments: "{}".into(),
        status: ToolStatus::Running,
        output: json!({"agent_id":child.id(),"run_id":first_run,"status":"running"}).to_string(),
    }));
    let mut progress = Progress::default();
    child
        .append(vec![
            AgentEvent::TurnStarted { turn: 1 },
            AgentEvent::ToolStarted {
                call: ToolCall::function("read", "read_file", json!({"path":"src/orbit.rs"})),
            },
        ])
        .await
        .unwrap();
    assert!(progress.poll(&store, &mut state).await.unwrap());
    let output = |state: &UiState, index: usize| match &state.transcript[index] {
        TranscriptItem::Tool(row) => serde_json::from_str::<Value>(&row.output).unwrap(),
        _ => panic!("expected child card"),
    };
    assert_eq!(output(&state, 0)["_activity"]["tool_calls"], 1);
    assert!(
        output(&state, 0)["_activity"]["current"]
            .as_str()
            .unwrap()
            .contains("src/orbit.rs")
    );
    assert!(!progress.poll(&store, &mut state).await.unwrap());
    child
        .append(vec![AgentEvent::Model(StreamEvent::ReasoningDelta(
            "接着考虑".into(),
        ))])
        .await
        .unwrap();
    assert!(progress.poll(&store, &mut state).await.unwrap());
    assert_eq!(output(&state, 0)["_activity"]["current"], "思考中");
    child
        .finish("completed", vec![], Some(vec![]))
        .await
        .unwrap();
    if let TranscriptItem::Tool(row) = &mut state.transcript[0] {
        row.status = ToolStatus::Finished;
    }
    progress.poll(&store, &mut state).await.unwrap();
    let original = output(&state, 0);
    child
        .begin("继续审查".into(), "orbit::model".into())
        .await
        .unwrap();
    let next_run = child.active_run_id().unwrap();
    child
        .append(vec![AgentEvent::ToolStarted {
            call: ToolCall::function("search", "search_text", json!({"query":"Orbit"})),
        }])
        .await
        .unwrap();
    state.transcript.push(TranscriptItem::Tool(ToolRow {
        id: "orbit-send".into(),
        name: "send_agent_message".into(),
        arguments: "{}".into(),
        status: ToolStatus::Running,
        output: json!({"agent_id":child.id(),"run_id":next_run,"status":"running"}).to_string(),
    }));
    assert!(progress.poll(&store, &mut state).await.unwrap());
    assert_eq!(output(&state, 0), original);
    assert_eq!(output(&state, 1)["_activity"]["tool_calls"], 1);
    assert!(
        output(&state, 1)["_activity"]["current"]
            .as_str()
            .unwrap()
            .starts_with("search_text")
    );
    state.transcript.clear();
    progress.poll(&store, &mut state).await.unwrap();
    assert!(progress.runs.is_empty());
}

#[test]
fn parent_navigation_has_hover_respects_selection_and_does_not_cover_the_editor() {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut state = UiState::new("orbit::model");
    state.subagent_parent = Some("orbit-parent".into());
    state.transcript.push(TranscriptItem::User("子任务".into()));
    terminal
        .draw(|frame| super::super::render::draw(frame, &mut state))
        .unwrap();
    let area = state.subagent_parent_area;
    assert_eq!(area.height, 1);
    assert!(area.bottom() <= state.input_area.y);
    assert!(area.y >= state.selection.viewport.bottom());
    assert_eq!(
        handle_parent(&mut state, &mouse(MouseEventKind::Moved, area.x, area.y)),
        Some(true)
    );
    assert!(state.subagent_parent_hovered);
    let down = mouse(MouseEventKind::Down(MouseButton::Left), area.x, area.y);
    let up = mouse(MouseEventKind::Up(MouseButton::Left), area.x, area.y);
    state.selection.viewport = Rect::new(0, 1, 80, 10);
    for event in [
        mouse(MouseEventKind::Down(MouseButton::Left), 1, 2),
        mouse(MouseEventKind::Drag(MouseButton::Left), 5, 2),
        mouse(MouseEventKind::Up(MouseButton::Left), 5, 2),
    ] {
        state.selection.handle(&event).unwrap();
    }
    assert!(state.selection.active());
    handle_parent(&mut state, &down);
    handle_parent(&mut state, &up);
    assert!(state.resume_request.is_none());
    handle_parent(&mut state, &down);
    handle_parent(&mut state, &up);
    assert_eq!(state.resume_request.take().as_deref(), Some("orbit-parent"));
    let key = Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT));
    state.show_help = true;
    assert!(handle_parent(&mut state, &key).is_none());
    assert!(state.resume_request.is_none());
    state.show_help = false;
    assert_eq!(handle_parent(&mut state, &key), Some(true));
    assert_eq!(state.resume_request.take().as_deref(), Some("orbit-parent"));
}
