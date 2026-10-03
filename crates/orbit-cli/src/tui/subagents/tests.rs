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
        for width in [12, 30, 80] {
            for expanded in [false, true] {
                let lines = card(&row, expanded, width).unwrap();
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
                    lines[0]
                        .links
                        .iter()
                        .any(|link| link.url.starts_with("orbit://session/"))
                );
                if width == 80 {
                    assert!(lines[1].line.to_string().contains(expected));
                    assert!(lines[1].line.to_string().contains("3 次调用 · 12s"));
                }
                if expanded {
                    assert!(lines.iter().any(|line| {
                        line.links
                            .iter()
                            .any(|link| link.url == "https://orbit.example/evidence")
                    }));
                    assert!(
                        lines
                            .iter()
                            .flat_map(|line| &line.line.spans)
                            .any(|span| span.content.contains("检查完成")
                                && span.style.add_modifier.contains(Modifier::BOLD))
                    );
                }
            }
        }
    }
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
