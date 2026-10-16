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

#[test]
fn confirmed_model_changes_refresh_context_and_dismiss_only_current_failure_status() {
    let (mut tab, _requests) = tab("本地");
    let mut info: SessionInfo = serde_json::from_value(serde_json::json!({
        "id": "generated-session",
        "title": "Generated task",
        "workspace": "/generated/workspace",
        "model": "alpha",
        "models": [],
        "supports_vision": false,
        "reasoning_effort": null,
        "reasoning_efforts": [],
        "context_tokens": 2_000,
        "input_context_tokens": 1_800,
        "max_output_tokens": 200,
        "context_used_tokens": 3_600
    }))
    .unwrap();
    tab.profile(&info);
    tab.state
        .feedback
        .info("Generated diagnostic", "Independent information");
    tab.state
        .apply(orbit_client::protocol::events::AgentEvent::Failed {
            error: "generated summary request failed".into(),
        });
    tab.state
        .input
        .set("preserve the generated draft".into());
    let transcript_len = tab.state.transcript.len();
    assert_eq!(tab.state.status, RunStatus::Failed);
    assert!(render_screen(&mut tab.state).contains("generated summary request failed"));
    info.model = "beta".into();
    info.input_context_tokens = 8_000;
    info.context_tokens = 8_200;
    info.context_used_tokens = Some(1_200);
    tab.profile(&info);
    assert_eq!(tab.state.status, RunStatus::Idle);
    assert_eq!(tab.state.context_used_tokens, 1_200);
    assert_eq!(tab.state.input_context_tokens, 8_000);
    assert_eq!(tab.state.transcript.len(), transcript_len);
    assert_eq!(tab.state.input.text, "preserve the generated draft");
    let screen = render_screen(&mut tab.state);
    assert!(!screen.contains("generated summary request failed"));
    assert!(screen.contains("Generated diagnostic"));
    tab.state.status = RunStatus::Failed;
    info.title = "Renamed generated task".into();
    tab.profile(&info);
    assert_eq!(tab.state.status, RunStatus::Failed);
}

#[test]
fn report_dialog_scrolls_through_real_input_even_under_a_notification() {
    let (mut tab, _requests) = tab("本地");
    tab.state
        .submit("existing conversation".into());
    tab.state
        .input
        .set("keep this draft".into());
    let history_len = tab.state.transcript.len();
    let chat_scroll = tab.state.scroll;
    tab.apply(Response::Report(orbit_protocol::Report {
        title: "诊断记录".into(),
        text: (0..100)
            .map(|i| format!("record-{i:03}\n"))
            .collect(),
    }));
    tab.state
        .feedback
        .info("提示", "a notification over the dialog");
    let initial = render_screen(&mut tab.state);
    assert!(initial.contains("record-000"));

    // The notification covers this point. It must not intercept the wheel.
    tab.handle(mouse(MouseEventKind::ScrollDown, 70, 3))
        .unwrap();
    let scrolled = render_screen(&mut tab.state);
    assert!(!scrolled.contains("record-000"));
    assert!(scrolled.contains("record-008"));
    tab.handle(mouse(MouseEventKind::ScrollUp, 70, 3))
        .unwrap();
    assert!(render_screen(&mut tab.state).contains("record-000"));

    for (key, first_visible, last_visible) in [
        (KeyCode::PageDown, false, false),
        (KeyCode::End, false, true),
        (KeyCode::Home, true, false),
    ] {
        tab.handle(Event::Key(KeyEvent::new(key, KeyModifiers::NONE)))
            .unwrap();
        let screen = render_screen(&mut tab.state);
        assert_eq!(screen.contains("record-000"), first_visible);
        assert_eq!(screen.contains("record-099"), last_visible);
    }
    tab.handle(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
        .unwrap();
    assert!(tab.state.info_dialog.is_none());
    assert_eq!(tab.state.input.text, "keep this draft");
    assert_eq!(tab.state.transcript.len(), history_len);
    assert_eq!(tab.state.scroll, chat_scroll);
}

fn image() -> orbit_client::protocol::model::Attachment {
    orbit_client::protocol::model::Attachment::Image {
        lease: None,
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
        crate::tui::theme::THEME.tab_hover_background
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
fn pending_image_mouse_and_keyboard_focus_preserve_the_caption() {
    let (mut tab, _requests) = tab("本地");
    tab.state
        .submit("existing conversation".into());
    tab.state.attachments = vec![image(), image()];
    tab.state.input.set("caption".into());
    render_screen(&mut tab.state);
    let second = tab.state.attachment_areas[1];
    assert!(matches!(
        tab.handle(mouse(
            MouseEventKind::Down(MouseButton::Left),
            second.x,
            second.y
        ))
        .unwrap(),
        Action::Redraw
    ));
    assert_eq!(tab.state.attachment_cursor, Some(1));
    assert!(!tab.state.input_dragging);
    for (key, focused) in [
        (KeyCode::Left, Some(0)),
        (KeyCode::Down, None),
        (KeyCode::Up, Some(0)),
    ] {
        tab.handle(Event::Key(KeyEvent::new(key, KeyModifiers::NONE)))
            .unwrap();
        assert_eq!(tab.state.attachment_cursor, focused);
        render_screen(&mut tab.state);
    }
    tab.handle(Event::Key(KeyEvent::new(
        KeyCode::Delete,
        KeyModifiers::NONE,
    )))
    .unwrap();
    assert_eq!(tab.state.attachments.len(), 1);
    render_screen(&mut tab.state);
    let input = tab.state.input_area;
    tab.handle(mouse(
        MouseEventKind::Down(MouseButton::Left),
        input.x,
        input.y,
    ))
    .unwrap();
    assert!(tab.state.attachment_cursor.is_none());
    assert_eq!(tab.state.input.text, "caption");
}

#[test]
fn bare_model_name_is_accepted() {
    let (mut tab, mut rx) = tab("本地");
    tab.state.model = "mock::previous".into();
    tab.state.model_completions = vec!["mock::model".into()];
    tab.submit("/model model".into())
        .unwrap();
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
            Update::Agent(Box::new(
                orbit_client::protocol::events::AgentEvent::ToolStarted {
                    call: orbit_client::protocol::model::ToolCall::function(
                        "call",
                        "run_command",
                        serde_json::json!({
                            "command": "sleep 1"
                        }),
                    ),
                },
            )),
            Update::Finished {
                elapsed_ms: None,
                status: "failed".into(),
            },
        ],
    );
    assert!(
        tab.state
            .transcript
            .iter()
            .all(|item| !matches!(
                item,
                TranscriptItem::Tool(tool) if tool.status == crate::tui::app::ToolStatus::Running
            ))
    );
}

#[test]
fn model_changes_preserve_image_drafts_and_only_send_to_a_compatible_model() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("draft.png");
    image::RgbaImage::new(1, 1)
        .save(&path)
        .unwrap();
    let attachment = orbit_client::protocol::model::Attachment::Image {
        lease: None,
        name: "draft.png".into(),
        mime_type: "image/png".into(),
        path,
    };
    let (mut tab, mut requests) = tab("本地");
    tab.state
        .attachments
        .push(attachment.clone());
    tab.state
        .input
        .set("describe this picture".into());
    let mut info: SessionInfo = serde_json::from_value(serde_json::json!({
        "id": "same-session",
        "title": "",
        "workspace": "/project",
        "model": "mock",
        "models": [],
        "supports_vision": false,
        "reasoning_effort": null,
        "reasoning_efforts": [],
        "context_tokens": 1_000,
        "input_context_tokens": 900,
        "max_output_tokens": 100
    }))
    .unwrap();
    for supports_vision in [false, true] {
        info.supports_vision = supports_vision;
        let Response::Page(mut update) =
            page(vec![Update::Profile(Box::new(info.clone()))], vec![])
        else {
            unreachable!()
        };
        update.busy = false;
        tab.apply(Response::Page(update));
        assert_eq!(tab.state.attachments, vec![attachment.clone()]);
        tab.handle(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )))
        .unwrap();
        if !supports_vision {
            assert!(requests.try_recv().is_err());
            assert_eq!(tab.state.input.text, "describe this picture");
            assert_eq!(tab.state.attachments, vec![attachment.clone()]);
            tab.handle(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
                .unwrap();
        } else {
            assert!(
                matches!(requests.try_recv().unwrap().request, Request::Start { images, prompt, .. }
                if images.len() == 1 && prompt == "describe this picture")
            );
            assert!(tab.state.attachments.is_empty());
        }
    }
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
            Update::agent(orbit_client::protocol::events::AgentEvent::InputReceived {
                attachments: vec![],
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
            .any(|i| matches!(
                i,
                TranscriptItem::User(text) if text == "sent\n[图片：sent.png]"
            ))
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
    assert!(matches!(
        requests.try_recv(),
        Ok(Call {
            request:
                Request::Start {
                    compact: true,
                    images,
                    ..
                },
            ..
        }) if images.is_empty()
    ));
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
                elapsed_ms: Some(94_500),
            },
        ],
    );
    assert_eq!(
        tab.state.active_elapsed(),
        std::time::Duration::from_millis(94_500)
    );
    let Response::Page(mut frame) = super::page(vec![], vec![]) else {
        unreachable!()
    };
    frame.busy = true;
    frame.timing = Some(orbit_protocol::RunTiming {
        elapsed_ms: 84_000,
        paused: true,
    });
    tab.apply(Response::Page(frame));
    assert_eq!(
        tab.state.active_elapsed(),
        std::time::Duration::from_millis(84_000)
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
    let call = orbit_client::protocol::model::ToolCall::function(
        "background",
        "run_command",
        serde_json::json!({}),
    );
    apply_page(
        &mut tab,
        vec![
            Update::Agent(Box::new(
                orbit_client::protocol::events::AgentEvent::ToolStarted { call },
            )),
            Update::Finished {
                status: "completed".into(),
                elapsed_ms: Some(100),
            },
        ],
    );
    assert!(
        tab.state
            .transcript
            .iter()
            .any(|item| matches!(
                item,
                TranscriptItem::Tool(tool) if tool.status == crate::tui::app::ToolStatus::Failed
            ))
    );
}

#[test]
fn running_command_card_updates_stdout_without_overwriting_a_finished_result() {
    let (mut tab, _rx) = tab("本地");
    let call = orbit_client::protocol::model::ToolCall::function(
        "command",
        "run_command",
        serde_json::json!({}),
    );
    apply_page(
        &mut tab,
        vec![
            Update::Started {
                prompt: "run".into(),
                presentation: Default::default(),
            },
            Update::Agent(Box::new(
                orbit_client::protocol::events::AgentEvent::ToolStarted { call: call.clone() },
            )),
        ],
    );
    let Response::Page(mut frame) = super::page(vec![], vec![]) else {
        unreachable!()
    };
    frame.tool_progress = vec![orbit_protocol::ToolProgress {
        call_id: call.id.clone(),
        stdout: Some("first\nsecond".into()),
        elapsed_ms: Some(12_345),
    }];
    tab.apply(Response::Page(frame));
    assert!(
        tab.state
            .transcript
            .iter()
            .any(|item| matches!(
                item,
                TranscriptItem::Tool(tool)
                    if serde_json::from_str::<serde_json::Value>(&tool.output).is_ok_and(|value| {
                        value["stdout"] == "first\nsecond" && value["elapsed_seconds"] == 12
                    })
            ))
    );
    apply_page(
        &mut tab,
        vec![Update::Agent(Box::new(
            orbit_client::protocol::events::AgentEvent::ToolFinished {
                call: call.clone(),
                content: "final result".into(),
                changes: vec![],
                files: None,
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
            .any(|item| matches!(
                item,
                TranscriptItem::Tool(tool) if tool.output == "final result"
            ))
    );
}

#[test]
fn user_input_completed_tool_click_releases_frozen_transcript_before_drawing_modal() {
    let (mut tab, _) = tab("本地");
    let output = serde_json::json!({
        "id": "answered-request",
        "call_id": "ask-1",
        "source": null,
        "status": "answered",
        "form": {
            "questions": [{
                "id": "choice",
                "title": "语言",
                "prompt": "选择开发语言",
                "kind": "single",
                "options": [
                    {
                        "id": "rust",
                        "label": "Rust",
                        "description": "系统编程"
                    },
                    {
                        "id": "python",
                        "label": "Python",
                        "description": "脚本"
                    }
                ],
                "allow_custom": true,
                "required": true
            }]
        },
        "answers": [{
            "question_id": "choice",
            "selected": ["rust"],
            "text": "",
            "note": ""
        }]
    });
    tab.state.input.set("保留草稿".into());
    tab.state
        .transcript
        .push(TranscriptItem::Tool(crate::tui::app::ToolRow {
            changes: Vec::new(),
            purpose: None,
            id: "ask-1".into(),
            name: "ask_user".into(),
            arguments: output["form"].to_string(),
            output: output.to_string(),
            status: crate::tui::app::ToolStatus::Finished,
        }));
    tab.state.expanded_tool_groups.insert(0);
    let mut terminal = Terminal::new(TestBackend::new(100, 35)).unwrap();
    terminal
        .draw(|frame| {
            crate::tui::render::draw(frame, &mut tab.state);
            tab.state.selection.paint(frame);
        })
        .unwrap();
    let area = tab.state.selection.viewport;
    let offset = tab
        .state
        .max_scroll
        .saturating_sub(tab.state.scroll);
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
    assert!(tab.state.info_dialog.is_some());
    tab.handle(Event::Key(KeyEvent::new(
        KeyCode::Char('a'),
        KeyModifiers::NONE,
    )))
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

#[test]
fn sent_images_use_target_assets_for_live_and_replayed_messages() {
    let images = vec![orbit_protocol::MessageImage {
        name: "clipboard.png".into(),
        path: "/target/session.attachments/asset.png".into(),
    }];
    for replayed in [false, true] {
        let (mut tab, _rx) = tab("SSH test");
        tab.state.attachments.push(image());
        apply_page(
            &mut tab,
            vec![Update::Started {
                prompt: "看这张图".into(),
                presentation: orbit_protocol::RunPresentation {
                    images: vec!["clipboard.png".into()],
                    image_assets: images.clone(),
                    replayed,
                    ..Default::default()
                },
            }],
        );
        assert!(matches!(&tab.state.transcript[0],
            TranscriptItem::UserWithImages { text, images: actual }
            if text == "看这张图" && actual == &images));
        assert_eq!(tab.state.input.history.last().unwrap(), "看这张图");
        assert_eq!(tab.state.attachments.len(), 1);
        assert!(!tab.state.is_home());
    }
}

#[test]
fn foreground_image_closes_before_plan_and_restores_the_composer_after_both_close() {
    use crate::tui::{media, overlay};
    let (mut tab, mut requests) = tab("本地");
    tab.state.input.set("未发送草稿".into());
    tab.state.planning.open();
    overlay::sync(&mut tab.state);
    tab.state.media_preview = Some(media::Preview::new(
        tab.state.media.clone(),
        media::Source::Image("missing-test-image.png".into()),
    ));
    let escape = Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    tab.handle(escape.clone()).unwrap();
    assert!(tab.state.media_preview.is_none());
    assert!(tab.state.planning.visible());
    assert_eq!(overlay::active(&tab.state), Some(overlay::Layer::Plan));
    tab.handle(escape).unwrap();
    assert!(!tab.state.dialog_open());
    assert_eq!(tab.state.input.text, "未发送草稿");
    assert!(requests.try_recv().is_err());
}

#[test]
fn resize_rejects_old_button_coordinates_until_the_dialog_is_painted() {
    let (mut tab, mut requests) = tab("本地");
    tab.state.planning.sync(
        serde_json::from_value(serde_json::json!({
            "generation": 3,
            "mode": "plan",
            "plan": {
                "id": "layout-test",
                "workspace": "/project",
                "revision": 1,
                "title": "方案",
                "markdown": "正文",
                "steps": ["实施"],
                "status": "review"
            }
        }))
        .unwrap(),
    );
    tab.state.planning.open();
    let mut terminal = Terminal::new(TestBackend::new(100, 35)).unwrap();
    terminal
        .draw(|frame| crate::tui::render::draw(frame, &mut tab.state))
        .unwrap();
    let approve_point = |terminal: &Terminal<TestBackend>| {
        let buffer = terminal.backend().buffer();
        let area = *buffer.area();
        (area.y..area.bottom())
            .find_map(|y| {
                (area.x..area.right().saturating_sub(2)).find_map(|x| {
                    (buffer[(x, y)].symbol() == "开" && buffer[(x + 2, y)].symbol() == "始")
                        .then_some((x, y))
                })
            })
            .unwrap()
    };
    let (x, y) = approve_point(&terminal);
    tab.handle(Event::Resize(80, 25))
        .unwrap();
    tab.handle(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
        .unwrap();
    assert!(requests.try_recv().is_err());
    terminal.backend_mut().resize(80, 25);
    terminal
        .resize(ratatui::layout::Rect::new(0, 0, 80, 25))
        .unwrap();
    terminal
        .draw(|frame| crate::tui::render::draw(frame, &mut tab.state))
        .unwrap();
    let (x, y) = approve_point(&terminal);
    tab.handle(mouse(MouseEventKind::Down(MouseButton::Left), x, y))
        .unwrap();
    assert!(matches!(
        requests.try_recv().unwrap().request,
        Request::Plan {
            action: orbit_protocol::plan::Action::Approve { generation: 3 },
            ..
        }
    ));
}

#[test]
fn file_approval_scroll_and_editing_use_the_live_event_route() {
    let (mut tab, mut requests) = tab("本地");
    tab.state
        .transcript
        .push(TranscriptItem::Assistant("chat line\n".repeat(100)));
    tab.apply(page(
        vec![],
        vec![Approval {
            id: 42,
            preview: orbit_client::protocol::approval::ApprovalPreview::Write(
                orbit_client::protocol::approval::WritePreview {
                    path: "src/main.rs".into(),
                    before: Some("old\n".repeat(100)),
                    after: "new\n".repeat(100),
                },
            ),
        }],
    ));
    tab.state.input.set("draft".into());
    let mut terminal = Terminal::new(TestBackend::new(100, 35)).unwrap();
    terminal
        .draw(|f| crate::tui::render::draw(f, &mut tab.state))
        .unwrap();
    let panel = tab.state.approval_area;
    let chat = tab.state.selection.viewport;
    assert!(panel.height > 0 && chat.height > 0);
    for x in [panel.x + 1, panel.right() - 2] {
        tab.handle(mouse(MouseEventKind::ScrollDown, x, panel.y + 2))
            .unwrap();
    }
    let preview_scroll = tab.state.approval_scroll;
    assert!(preview_scroll > 0);
    let chat_scroll = tab.state.scroll;
    tab.handle(mouse(MouseEventKind::ScrollUp, chat.x + 1, chat.y + 1))
        .unwrap();
    assert!(tab.state.scroll > chat_scroll);
    assert_eq!(tab.state.approval_scroll, preview_scroll);
    tab.handle(Event::Paste(" text".into()))
        .unwrap();
    assert_eq!(tab.state.input.text, "draft text");
    assert!(requests.try_recv().is_err());
    for (width, height) in [(24, 10), (10, 5), (1, 1), (140, 40)] {
        terminal
            .backend_mut()
            .resize(width, height);
        terminal
            .resize(ratatui::layout::Rect::new(0, 0, width, height))
            .unwrap();
        terminal
            .draw(|f| crate::tui::render::draw(f, &mut tab.state))
            .unwrap();
    }
    assert_eq!(tab.state.input.text, "draft text");
}

#[test]
fn replayed_reasoning_uses_saved_duration_and_live_reasoning_still_times_itself() {
    use orbit_client::protocol::events::AgentEvent;
    use orbit_client::protocol::model::StreamEvent;
    let (mut tab, _requests) = tab("本地");
    for (replayed, saved) in [(true, Some(7_500)), (true, None), (false, None)] {
        let mut updates = vec![
            Update::Started {
                prompt: "question".into(),
                presentation: orbit_protocol::RunPresentation {
                    replayed,
                    ..Default::default()
                },
            },
            Update::agent(AgentEvent::Model(StreamEvent::ReasoningDelta(
                "thought".into(),
            ))),
        ];
        if let Some(elapsed_ms) = saved {
            updates.push(Update::agent(AgentEvent::ReasoningFinished { elapsed_ms }));
        }
        updates.push(Update::agent(AgentEvent::Model(StreamEvent::TextDelta(
            "answer".into(),
        ))));
        updates.push(Update::Finished {
            status: "completed".into(),
            elapsed_ms: Some(8_000),
        });
        apply_page(&mut tab, updates);
        let row = tab
            .state
            .transcript
            .iter()
            .rev()
            .find_map(|item| match item {
                TranscriptItem::Thinking(row) => Some(row),
                _ => None,
            })
            .unwrap();
        assert!(row.finished);
        if replayed {
            assert_eq!(row.elapsed, saved.map(Duration::from_millis));
        } else {
            assert!(row.elapsed.is_some());
        }
    }
}
