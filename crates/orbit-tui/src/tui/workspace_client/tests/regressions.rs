use super::*;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{Terminal, backend::TestBackend};
fn mouse(kind: MouseEventKind, x: u16, y: u16) -> Event {
    Event::Mouse(MouseEvent {
        kind,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    })
}
fn image() -> orbit_model::types::Attachment {
    orbit_model::types::Attachment::Image {
        name: "draft.png".into(),
        mime_type: "image/png".into(),
        path: "/tmp/draft.png".into(),
    }
}

fn completion_tab() -> (Tab, mpsc::Receiver<Call>, Terminal<TestBackend>) {
    let (mut tab, rx) = tab("本地");
    tab.state.input.set("/cd Te".into());
    tab.state.completion_query = Some("/cd Te".into());
    tab.state.stored_completions = ["Tests/", "Templates/"]
        .into_iter()
        .map(|value| crate::tui::completion::Candidate {
            name: format!("/cd {value}"),
            display: Some(value.into()),
            description: String::new(),
            continue_input: true,
        })
        .collect();
    let mut term = Terminal::new(TestBackend::new(140, 40)).unwrap();
    term.draw(|f| crate::tui::render::draw(f, &mut tab.state))
        .unwrap();
    (tab, rx, term)
}

#[test]
fn header_hover_and_close_normal_path() {
    let mut tabs = crate::tui::tabs::live::Tabs::default();
    let first = tabs.add_remote(connected_tab(workspace::transport::Target::Local));
    tabs.add_remote(connected_tab(workspace::transport::Target::Local));
    tabs.sync_remote_ui();
    let active = tabs.active_remote.clone().unwrap();
    let tab = tabs.remote.get_mut(&active).unwrap();
    let mut term = Terminal::new(TestBackend::new(140, 40)).unwrap();
    term.draw(|f| crate::tui::render::draw(f, &mut tab.state))
        .unwrap();
    let rect = tab
        .state
        .tab_areas
        .iter()
        .find(|(_, id)| id == &first)
        .unwrap()
        .0;
    let event = mouse(MouseEventKind::Moved, rect.x + 1, rect.y);
    assert!(crate::tui::update_tab_hover(&event, &mut tab.state));
    tab.handle(event).unwrap();
    term.draw(|f| crate::tui::render::draw(f, &mut tab.state))
        .unwrap();
    assert!(tab.state.tab_hover.is_some());
    assert_eq!(
        term.backend().buffer()[(rect.x + 1, rect.y)].bg,
        crate::tui::theme::THEME.tab_background
    );
    let close = tab
        .state
        .tab_close_areas
        .iter()
        .find(|(_, id)| id == &first)
        .unwrap()
        .0;
    assert_eq!(
        term.backend().buffer()[(close.x + 1, close.y)].symbol(),
        crate::tui::icons::CLOSE_THICK
    );
    assert_eq!(
        crate::tui::tab_target(
            &mouse(
                MouseEventKind::Down(MouseButton::Left),
                close.x + 1,
                close.y
            ),
            &tab.state
        ),
        Some(crate::tui::TabAction::Close(first))
    );
}

#[test]
fn completion_mouse_hover() {
    let (mut tab, _rx, _term) = completion_tab();
    let rect = tab.state.menu_rows[1].0;
    tab.handle(mouse(MouseEventKind::Moved, rect.x + 1, rect.y))
        .unwrap();
    assert_eq!(tab.state.menu_index, 1);
}

#[test]
fn completion_mouse_click() {
    let (mut tab, _rx, _term) = completion_tab();
    let rect = tab.state.menu_rows[0].0;
    tab.handle(mouse(
        MouseEventKind::Down(MouseButton::Left),
        rect.x + 1,
        rect.y,
    ))
    .unwrap();
    assert_eq!(tab.state.input.text, "/cd Tests/");
}

#[test]
fn control_d_with_image_draft() {
    let (mut tab, _rx) = tab("本地");
    tab.state.attachments.push(image());
    tab.state.attachment_cursor = Some(0);
    let action = tab
        .handle(Event::Key(KeyEvent::new(
            KeyCode::Char('d'),
            KeyModifiers::CONTROL,
        )))
        .unwrap();
    assert!(!matches!(action, Action::Quit));
    assert!(tab.state.attachments.is_empty());
}

#[test]
fn bare_model_name_is_accepted() {
    let (mut tab, mut rx) = tab("本地");
    tab.state.model = "mock::previous".into();
    tab.state.model_completions = vec!["mock::model".into()];
    tab.submit("/model model".into()).unwrap();
    assert!(matches!(
        rx.try_recv(),
        Ok(Call {
            request: Request::SetModel { .. },
            ..
        })
    ));
}

#[test]
fn navigation_wheel_does_not_scroll_transcript() {
    let (mut tab, _rx) = tab("本地");
    for i in 0..50 {
        tab.state
            .transcript
            .push(TranscriptItem::User(format!("Round {i}")));
    }
    tab.state.follow_bottom = false;
    tab.state.scroll = 20;
    let mut term = Terminal::new(TestBackend::new(140, 40)).unwrap();
    term.draw(|f| crate::tui::render::draw(f, &mut tab.state))
        .unwrap();
    assert!(tab.state.max_scroll > 10);
    tab.state.scroll = tab.state.max_scroll / 2;
    let before = tab.state.scroll;
    let view = tab.state.selection.viewport;
    tab.handle(mouse(
        MouseEventKind::ScrollUp,
        view.right() + 3,
        view.y + 2,
    ))
    .unwrap();
    assert_eq!(tab.state.scroll, before);
}

#[test]
fn permissions_alias_accepts_a_mode() {
    let (mut tab, mut rx) = tab("本地");
    tab.submit("/permissions ask".into()).unwrap();
    assert!(matches!(
        rx.try_recv(),
        Ok(Call {
            request: Request::SetMode {
                mode: Mode::Ask,
                ..
            },
            ..
        })
    ));
}

#[test]
fn local_helpers_use_the_active_workspace() {
    let mut tab = connected_tab(workspace::transport::Target::Local);
    tab.state.workspace = "/changed/workspace".into();
    assert_eq!(
        tab.local_directory().unwrap(),
        std::path::PathBuf::from("/changed/workspace")
    );
}

#[test]
fn restore_populates_prompt_history() {
    let (mut tab, _rx) = tab("本地");
    apply_page(
        &mut tab,
        vec![
            Update::Started {
                presentation: Default::default(),
                prompt: "old prompt".into(),
            },
            Update::Finished {
                elapsed_ms: None,
                status: "completed".into(),
            },
        ],
    );
    assert_eq!(tab.state.input.history, vec!["old prompt"]);
}

#[test]
fn compaction_does_not_add_a_user_message() {
    let (mut tab, _rx) = tab("本地");
    apply_page(
        &mut tab,
        vec![Update::Started {
            presentation: Default::default(),
            prompt: "/compact".into(),
        }],
    );
    assert!(
        !tab.state
            .transcript
            .iter()
            .any(|item| matches!(item, TranscriptItem::User(_)))
    );
}

#[test]
fn interrupted_tool_is_no_longer_running_after_restore() {
    let (mut tab, _rx) = tab("本地");
    apply_page(
        &mut tab,
        vec![
            Update::Started {
                presentation: Default::default(),
                prompt: "old prompt".into(),
            },
            Update::Agent(Box::new(orbit_agent::events::AgentEvent::ToolStarted {
                call: orbit_model::types::ToolCall::function(
                    "call",
                    "run_command",
                    serde_json::json!({"command":"sleep 1"}),
                ),
            })),
            Update::Finished {
                elapsed_ms: None,
                status: "failed".into(),
            },
        ],
    );
    assert!(tab.state.transcript.iter().all(|item|!matches!(item,TranscriptItem::Tool(tool) if tool.status==crate::tui::app::ToolStatus::Running)));
}

#[test]
fn nonvision_model_clears_queued_images() {
    let (mut tab, _rx) = tab("本地");
    tab.state.supports_vision = true;
    tab.state.attachments.push(image());
    let info: SessionInfo=serde_json::from_value(serde_json::json!({"id":"same-session","title":"","workspace":"/project","model":"mock","models":[],"supports_vision":false,"reasoning_effort":null,"reasoning_efforts":[],"context_tokens":1000,"input_context_tokens":900,"max_output_tokens":100})).unwrap();
    apply_page(&mut tab, vec![Update::Profile(Box::new(info))]);
    assert!(tab.state.attachments.is_empty());
}

#[test]
fn start_receipt_preserves_new_image_draft() {
    let (mut tab, _rx) = tab("本地");
    tab.starting = Some("sent prompt".into());
    tab.state.attachments.push(image());
    apply_page(
        &mut tab,
        vec![Update::Started {
            presentation: Default::default(),
            prompt: "sent prompt".into(),
        }],
    );
    assert_eq!(tab.state.attachments.len(), 1);
}

#[test]
fn queued_input_replay_has_one_user_message() {
    let (mut tab, _rx) = tab("本地");
    apply_page(
        &mut tab,
        vec![
            Update::Started {
                presentation: orbit_protocol::RunPresentation {
                    queued_input: true,
                    ..Default::default()
                },
                prompt: "queued prompt".into(),
            },
            Update::agent(orbit_agent::events::AgentEvent::InputReceived {
                message_id: 1,
                content: "queued prompt".into(),
            }),
        ],
    );
    assert_eq!(
        tab.state
            .transcript
            .iter()
            .filter(|i| matches!(i, TranscriptItem::User(_)))
            .count(),
        1
    );
}

fn apply_page(tab: &mut Tab, updates: Vec<Update>) {
    tab.apply(super::page(updates, vec![]));
}
#[test]
fn sent_images_have_a_message_marker_and_preserve_the_next_draft() {
    let (mut tab, _rx) = tab("本地");
    tab.starting = Some("sent".into());
    tab.starting_attachments.push(image());
    tab.state.attachments.push(image());
    apply_page(
        &mut tab,
        vec![Update::Started {
            prompt: "sent".into(),
            presentation: orbit_protocol::RunPresentation {
                images: vec!["sent.png".into()],
                ..Default::default()
            },
        }],
    );
    assert!(
        tab.state
            .transcript
            .iter()
            .any(|i| matches!(i,TranscriptItem::User(text) if text=="sent\n[图片：sent.png]"))
    );
    assert_eq!(tab.state.attachments.len(), 1);
    assert!(tab.starting_attachments.is_empty());
}
#[test]
fn failed_start_restores_only_the_submitted_images() {
    let (mut tab, _rx) = tab("本地");
    tab.starting = Some("sent".into());
    tab.starting_attachments.push(image());
    tab.state.attachments.push(image());
    tab.apply(Response::Error("rejected".into()));
    assert_eq!(tab.state.attachments.len(), 2);
    assert_eq!(tab.state.input.text, "sent");
}
#[test]
fn compact_preserves_the_image_draft_and_sends_no_uploads() {
    let (mut tab, mut requests) = tab("本地");
    tab.state.attachments.push(image());
    tab.submit("/compact".into()).unwrap();
    assert!(
        matches!(requests.try_recv(),Ok(Call {request:Request::Start{compact:true,images,..},..}) if images.is_empty())
    );
    apply_page(
        &mut tab,
        vec![Update::Started {
            prompt: "/compact".into(),
            presentation: Default::default(),
        }],
    );
    assert_eq!(tab.state.attachments.len(), 1);
}
#[test]
fn history_keeps_recorded_elapsed_and_live_clock_comes_from_the_service() {
    let (mut tab, _rx) = tab("本地");
    apply_page(
        &mut tab,
        vec![
            Update::Started {
                prompt: "past".into(),
                presentation: Default::default(),
            },
            Update::Finished {
                status: "completed".into(),
                elapsed_ms: Some(94500),
            },
        ],
    );
    assert_eq!(
        tab.state.active_elapsed(),
        std::time::Duration::from_millis(94500)
    );
    let Response::Page(mut frame) = super::page(vec![], vec![]) else {
        unreachable!()
    };
    frame.busy = true;
    frame.timing = Some(orbit_protocol::RunTiming {
        elapsed_ms: 84000,
        paused: true,
    });
    tab.apply(Response::Page(frame));
    assert_eq!(
        tab.state.active_elapsed(),
        std::time::Duration::from_millis(84000)
    );
}

#[tokio::test]
async fn catalog_refresh_waits_until_the_current_run_finishes() {
    let (mut tab, mut requests) = tab("本地");
    tab.busy = true;
    tab.model_configuration = Some(orbit_protocol::Configuration {
        content: String::new(),
        credentials: Default::default(),
    });
    tab.poll().await;
    assert!(tab.model_configuration.is_some());
    while let Ok(call) = requests.try_recv() {
        assert!(!matches!(call.request, Request::ConfigureModels { .. }));
    }
    tab.busy = false;
    tab.poll().await;
    assert!(tab.model_configuration.is_none());
    let mut configured = false;
    while let Ok(call) = requests.try_recv() {
        configured |= matches!(call.request, Request::ConfigureModels { .. });
    }
    assert!(configured);
}
#[test]
fn replay_preserves_reading_position_and_finalizes_only_replayed_background_tools() {
    let (mut tab, _rx) = tab("本地");
    tab.state.scroll = 12;
    tab.state.follow_bottom = false;
    apply_page(
        &mut tab,
        vec![Update::Started {
            prompt: "past".into(),
            presentation: orbit_protocol::RunPresentation {
                replayed: true,
                ..Default::default()
            },
        }],
    );
    assert_eq!(tab.state.scroll, 12);
    assert!(!tab.state.follow_bottom);
    let call =
        orbit_model::types::ToolCall::function("background", "run_command", serde_json::json!({}));
    apply_page(
        &mut tab,
        vec![
            Update::Agent(Box::new(orbit_agent::events::AgentEvent::ToolStarted {
                call,
            })),
            Update::Finished {
                status: "completed".into(),
                elapsed_ms: Some(100),
            },
        ],
    );
    assert!(tab.state.transcript.iter().any(|item|matches!(item,TranscriptItem::Tool(tool) if tool.status==crate::tui::app::ToolStatus::Failed)));
}

#[test]
fn running_command_card_updates_stdout_without_overwriting_a_finished_result() {
    let (mut tab, _rx) = tab("本地");
    let call =
        orbit_model::types::ToolCall::function("command", "run_command", serde_json::json!({}));
    apply_page(
        &mut tab,
        vec![
            Update::Started {
                prompt: "run".into(),
                presentation: Default::default(),
            },
            Update::Agent(Box::new(orbit_agent::events::AgentEvent::ToolStarted {
                call: call.clone(),
            })),
        ],
    );
    let Response::Page(mut frame) = super::page(vec![], vec![]) else {
        unreachable!()
    };
    frame.tool_progress = vec![orbit_protocol::ToolProgress {
        call_id: call.id.clone(),
        stdout: Some("first\nsecond".into()),
        elapsed_ms: Some(12345),
    }];
    tab.apply(Response::Page(frame));
    assert!(tab.state.transcript.iter().any(|item| matches!(item,TranscriptItem::Tool(tool) if serde_json::from_str::<serde_json::Value>(&tool.output).is_ok_and(|value|value["stdout"]=="first\nsecond" && value["elapsed_seconds"]==12))));
    apply_page(
        &mut tab,
        vec![Update::Agent(Box::new(
            orbit_agent::events::AgentEvent::ToolFinished {
                call: call.clone(),
                content: "final result".into(),
                changes: vec![],
            },
        ))],
    );
    let Response::Page(mut frame) = super::page(vec![], vec![]) else {
        unreachable!()
    };
    frame.tool_progress = vec![orbit_protocol::ToolProgress {
        call_id: call.id,
        stdout: Some("stale tail".into()),
        elapsed_ms: None,
    }];
    tab.apply(Response::Page(frame));
    assert!(
        tab.state
            .transcript
            .iter()
            .any(|item| matches!(item,TranscriptItem::Tool(tool) if tool.output=="final result"))
    );
}

#[test]
fn user_input_completed_tool_click_releases_frozen_transcript_before_drawing_modal() {
    let (mut tab, _) = tab("本地");
    let output = serde_json::json!({
        "id": "answered-request", "call_id": "ask-1", "source": null,
        "status": "answered",
        "form": { "questions": [{
            "id": "choice", "title": "语言", "prompt": "选择开发语言",
            "kind": "single", "options": [
                { "id": "rust", "label": "Rust", "description": "系统编程" },
                { "id": "python", "label": "Python", "description": "脚本" }
            ], "allow_custom": true, "required": true
        }] },
        "answers": [{ "question_id": "choice", "selected": ["rust"], "text": "", "note": "" }]
    });
    tab.state.input.set("保留草稿".into());
    tab.state
        .transcript
        .push(TranscriptItem::Tool(crate::tui::app::ToolRow {
            id: "ask-1".into(),
            name: "ask_user".into(),
            arguments: output["form"].to_string(),
            output: output.to_string(),
            status: crate::tui::app::ToolStatus::Finished,
        }));
    let mut terminal = Terminal::new(TestBackend::new(100, 35)).unwrap();
    terminal
        .draw(|frame| {
            crate::tui::render::draw(frame, &mut tab.state);
            tab.state.selection.paint(frame);
        })
        .unwrap();
    let area = tab.state.selection.viewport;
    let offset = tab.state.max_scroll.saturating_sub(tab.state.scroll);
    let (x, y) = (area.y..area.bottom())
        .find_map(|y| {
            (area.x..area.right()).find_map(|x| {
                tab.state
                    .transcript_layout
                    .disclosure_at(&tab.state.transcript, area, offset, (x, y))
                    .map(|_| (x, y))
            })
        })
        .unwrap();
    tab.handle(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
        .unwrap();
    terminal
        .draw(|frame| assert!(tab.state.selection.draw_frozen(frame)))
        .unwrap();
    tab.handle(mouse(MouseEventKind::Up(MouseButton::Left), x, y))
        .unwrap();
    assert!(tab.state.user_input.visible());
    terminal
        .draw(|frame| {
            assert!(!tab.state.selection.draw_frozen(frame));
            crate::tui::render::draw(frame, &mut tab.state);
        })
        .unwrap();
    let screen = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(
        screen
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
            .contains("问答记录")
    );
    assert!(screen.contains("Rust"));
    assert!(tab.state.ime_cursor.is_none());
    tab.handle(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
        .unwrap();
    assert!(!tab.state.user_input.visible());
    assert_eq!(tab.state.input.text, "保留草稿");
}
