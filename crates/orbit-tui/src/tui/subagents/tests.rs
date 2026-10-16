use crossterm::event::{KeyEvent, MouseEvent};
use ratatui::{Terminal, backend::TestBackend};
use serde_json::json;

use super::*;

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
        ("running", "ReadFile"),
        ("completed", "完成"),
        ("failed", "失败"),
        ("cancelled", "已取消"),
        ("interrupted", "已中断"),
        ("queued", "已排队"),
    ] {
        let row = ToolRow {
            changes: Vec::new(),
            purpose: None,
            id: "orbit-child".into(),
            name: "SpawnAgent".into(),
            arguments: json!({"task":"审查中文边界 🦀 和长文件名","role":"review"}).to_string(),
            status: if status == "running" {
                ToolStatus::Running
            } else {
                ToolStatus::Finished
            },
            output: json!({
                "agent_id": "00000000000000000000000000000001",
                "status": status,
                "model": "orbit::model",
                "tool_calls": 3,
                "elapsed_ms": 12_900,
                "_activity": {
                    "current": "ReadFile crates/orbit-tui/src/tui/很长很长很长的文件名.rs"
                },
                "summary": "**检查完成**\n\n[证据](https://orbit.example/evidence)"
            })
            .to_string(),
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
                        .filter(|span| span
                            .style
                            .add_modifier
                            .contains(Modifier::UNDERLINED))
                        .all(|span| span.content == "查看")
                );
                if width == 80 && expanded {
                    assert!(
                        lines
                            .iter()
                            .any(|line| line.line.to_string().contains(expected))
                    );
                    assert!(lines.iter().any(|line| {
                        line.line
                            .to_string()
                            .contains("3 次调用 · 12s")
                    }));
                }

                if expanded {
                    assert!(
                        lines
                            .first()
                            .unwrap()
                            .line
                            .to_string()
                            .starts_with('╭')
                    );
                    assert!(
                        lines
                            .last()
                            .unwrap()
                            .line
                            .to_string()
                            .starts_with('╰')
                    );
                    assert!(
                        lines[1..lines.len() - 1]
                            .iter()
                            .all(|line| {
                                let text = line.line.to_string();
                                text.starts_with('│') && text.ends_with('│')
                            })
                    );
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
                        .filter(|span| {
                            span.style
                                .add_modifier
                                .contains(Modifier::BOLD)
                        })
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
        state
            .transcript
            .push(TranscriptItem::Tool(ToolRow {
                changes: Vec::new(),
                purpose: None,
                id: "orbit-report".into(),
                name: "SpawnAgent".into(),
                arguments: json!({"task":"检查 Orbit 报告"}).to_string(),
                status: ToolStatus::Finished,
                output: json!({
                    "agent_id": "00000000000000000000000000000001",
                    "status": "completed",
                    "summary": "# Orbit\n\n---\n\n```rust\n// Orbit 注释\nfn orbit() {}\n```"
                })
                .to_string(),
            }));
        super::super::toggle_transcript_item(&mut state, 0);
        let mut terminal = Terminal::new(TestBackend::new(width + 6, 50)).unwrap();
        terminal
            .draw(|frame| super::super::render::draw(frame, &mut state))
            .unwrap();
        let dialog = state.info_dialog.as_ref().unwrap();
        assert!(dialog.content.contains("fn orbit()"));
        assert!(!dialog.content.contains('╭'));
    }
}

#[test]
fn parent_navigation_has_hover_respects_selection_and_does_not_cover_the_editor() {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut state = UiState::new("orbit::model");
    state.subagent_parent = Some("orbit-parent".into());
    state
        .transcript
        .push(TranscriptItem::User("子任务".into()));
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
