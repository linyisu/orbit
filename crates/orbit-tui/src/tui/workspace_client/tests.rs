use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

use super::*;
use crate::{
    tui::{TabAction, tabs::live::Tabs},
    workspace::protocol::{Approval, Page},
};

fn tab(label: &str) -> (Tab, mpsc::Receiver<Call>) {
    let mut tab = Tab::empty(label.into(), "/project".into(), media::Context::default());
    // Deliberately duplicate target session IDs: UI identity must also include the connection.
    tab.id = Some("same-session".into());
    let (commands, receiver) = mpsc::channel(CHANNEL_CAPACITY);
    tab.commands = Some(Arc::new(commands));
    (tab, receiver)
}

fn render_screen(state: &mut UiState) -> String {
    use ratatui::{Terminal, backend::TestBackend};
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|frame| crate::tui::render::draw_frame(frame, state))
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[tokio::test]
async fn usage_pages_scroll_during_a_run_without_changing_history_or_draft_and_close_discards_late_results()
 {
    use orbit_protocol::usage::{Account, Limit, Snapshot};
    let (mut tab, mut requests) = tab("本地");
    tab.state.submit("existing task".into());
    tab.state.input.set("keep this draft".into());
    let history = tab.state.transcript.len();
    let snapshot = Snapshot {
        account: Account {
            provider: "Gemini CLI".into(),
            name: "main".into(),
            limits: (0..40)
                .map(|index| Limit {
                    label: format!("model-{index:02}"),
                    remaining_fraction: Some(0.5),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        },
        ..Default::default()
    };
    tab.state.usage.receive(snapshot.clone());
    tab.submit("/usage".into()).unwrap();
    assert!(render_screen(&mut tab.state).contains("model-00"));
    tab.handle(Event::Key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE)))
        .unwrap();
    assert!(render_screen(&mut tab.state).contains("model-39"));
    tab.handle(Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)))
        .unwrap();
    assert!(render_screen(&mut tab.state).contains("Token"));
    assert!(
        tab.state
            .info_dialog
            .as_ref()
            .unwrap()
            .content
            .contains("模型请求")
    );
    tab.handle(Event::Key(KeyEvent::new(
        KeyCode::BackTab,
        KeyModifiers::NONE,
    )))
    .unwrap();
    assert!(render_screen(&mut tab.state).contains("model-00"));
    tab.handle(Event::Key(KeyEvent::new(
        KeyCode::Char('r'),
        KeyModifiers::NONE,
    )))
    .unwrap();
    assert!(tab.state.info_dialog.as_ref().unwrap().refresh_requested);
    tab.usage_task = Some(AbortOnDropHandle::new(tokio::spawn(
        async move { Ok(snapshot) },
    )));
    tab.handle(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
        .unwrap();
    tab.poll_usage().await;
    assert!(tab.state.info_dialog.is_none());
    assert!(tab.usage_task.is_none());
    assert_eq!(tab.state.input.text, "keep this draft");
    assert_eq!(tab.state.transcript.len(), history);
    assert!(requests.try_recv().is_err());
}

#[test]
fn help_is_discoverable_and_opens_the_same_dialog_on_home_and_during_a_run() {
    for label in ["本地", "SSH dev", "WSL Ubuntu"] {
        for running in [false, true] {
            let (mut tab, mut requests) = tab(label);
            if running {
                tab.state.submit("existing conversation".into());
            }
            let history_len = tab.state.transcript.len();
            tab.state.input.set("/he".into());
            assert!(tab.state.menu().iter().any(|entry| entry.name == "/help"));
            tab.state.input.set("keep this draft".into());
            tab.submit("/help".into()).unwrap();
            assert!(tab.state.show_help);
            assert!(!tab.state.help_keys);
            assert_eq!(tab.state.transcript.len(), history_len);
            assert!(requests.try_recv().is_err());
            tab.handle(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)))
                .unwrap();
            assert_eq!(tab.state.help_scroll, 1);
            tab.handle(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
                .unwrap();
            assert!(!tab.state.show_help);
            assert_eq!(tab.state.input.text, "keep this draft");
            assert_eq!(tab.state.is_home(), !running);
            assert!(matches!(
                tab.submit("/restore".into()).unwrap(),
                Action::Restore
            ));
        }
    }
}

#[test]
fn target_completions_apply_only_to_the_input_that_requested_them() {
    let (mut tab, _) = tab("SSH dev");
    tab.state.connection_label = Some("SSH dev".into());
    tab.state.input.set("/cd pro".into());
    tab.apply(Response::Completions {
        input: "/cd pro".into(),
        entries: vec![crate::tui::completion::Candidate {
            name: "/cd project/".into(),
            display: Some("project/".into()),
            description: String::new(),
            continue_input: true,
        }],
    });
    assert_eq!(tab.state.menu()[0].name, "/cd project/");
    tab.state.input.set("/cd different".into());
    assert!(
        tab.state
            .menu()
            .iter()
            .all(|entry| entry.name != "/cd project/")
    );
}

#[test]
fn remote_subagent_view_links_open_a_modal_and_keep_parent_input_isolated() {
    use crate::tui::app::{ToolRow, ToolStatus};
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use ratatui::{Terminal, backend::TestBackend};

    let (mut tab, mut requests) = tab("SSH dev");
    let agent = "00000000000000000000000000000001";
    tab.state.submit("parent task".into());
    tab.state.input.set("parent draft".into());
    tab.state.transcript.push(TranscriptItem::Tool(ToolRow {
        id: "spawn".into(),
        name: "spawn_agent".into(),
        status: ToolStatus::Finished,
        arguments: serde_json::json!({"task":"child task"}).to_string(),
        output: serde_json::json!({"agent_id":agent,"status":"completed"}).to_string(),
    }));
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|frame| crate::tui::render::draw(frame, &mut tab.state))
        .unwrap();
    let area = tab.state.selection.viewport;
    let point = (area.y..area.bottom())
        .find_map(|y| {
            (area.x..area.right()).find_map(|x| {
                tab.state
                    .transcript_layout
                    .link_at(area, tab.state.max_scroll, (x, y))
                    .filter(|link| link.starts_with("orbit://session/"))
                    .map(|_| (x, y))
            })
        })
        .unwrap();
    for kind in [
        MouseEventKind::Down(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        tab.handle(Event::Mouse(MouseEvent {
            kind,
            column: point.0,
            row: point.1,
            modifiers: KeyModifiers::NONE,
        }))
        .unwrap();
    }
    assert!(tab.state.subagent_dialog.is_some());
    assert!(tab.state.expanded.is_empty());
    tab.poll_subagent();
    let call = requests.try_recv().unwrap();
    assert!(
        matches!(call.request, Request::ReadSubagent { agent: ref id, after:0, .. } if id == agent)
    );
    call.reply
        .unwrap()
        .send(Ok(Response::Subagent(Page {
            user_inputs: Vec::new(),
            planning: Default::default(),
            tool_progress: Vec::new(),
            timing: None,
            next: 3,
            more: false,
            busy: false,
            model: "child-model".into(),
            approvals: vec![],
            updates: vec![
                Update::Started {
                    presentation: Default::default(),
                    prompt: "child task".into(),
                },
                Update::agent(orbit_agent::events::AgentEvent::Model(
                    orbit_model::types::StreamEvent::TextDelta("child answer".into()),
                )),
                Update::Finished {
                    elapsed_ms: None,
                    status: "completed".into(),
                },
            ],
        })))
        .unwrap();
    assert!(tab.poll_subagent());
    tab.handle(Event::Paste("follow-up".into())).unwrap();
    assert_eq!(tab.state.input.text, "parent draft");
    tab.handle(Event::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )))
    .unwrap();
    let call = requests.try_recv().unwrap();
    assert!(
        matches!(call.request, Request::SendSubagent { ref message, .. } if message == "follow-up")
    );
    call.reply
        .unwrap()
        .send(Ok(Response::Error("temporary send failure".into())))
        .unwrap();
    tab.poll_subagent();
    // A failed follow-up must keep its draft so Enter can retry it unchanged.
    tab.handle(Event::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )))
    .unwrap();
    let retry = requests.try_recv().unwrap();
    assert!(
        matches!(retry.request, Request::SendSubagent { ref message, .. } if message == "follow-up")
    );
    retry.reply.unwrap().send(Ok(Response::Ok)).unwrap();
    tab.poll_subagent();
    tab.handle(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
        .unwrap();
    assert!(tab.state.subagent_dialog.is_none());
    assert_eq!(tab.state.input.text, "parent draft");
}

fn page(updates: Vec<Update>, approvals: Vec<Approval>) -> Response {
    Response::Page(Page {
        user_inputs: Vec::new(),
        planning: Default::default(),
        tool_progress: Vec::new(),
        timing: None,
        next: updates.len(),
        more: false,
        busy: true,
        model: "mock".into(),
        updates,
        approvals,
    })
}

fn approval(id: u64) -> Approval {
    Approval {
        id,
        preview: orbit_tools::approval::ApprovalPreview::Capability(
            orbit_tools::approval::CapabilityPreview {
                environment: orbit_runtime::capability::EnvironmentId::Local,
                path: "/external".into(),
                access: orbit_runtime::capability::FilesystemAccess::Read,
                reason: "test".into(),
            },
        ),
    }
}

#[tokio::test]
async fn concurrent_tabs_keep_drafts_events_models_and_requests_separate() {
    let (mut ssh, mut ssh_requests) = tab("SSH dev");
    let (mut wsl, mut wsl_requests) = tab("WSL Ubuntu");
    ssh.state.model_completions = vec!["ssh-model".into()];
    wsl.state.model_completions = vec!["wsl-model".into()];
    ssh.submit("/model ssh-model".into()).unwrap();
    wsl.submit("/model wsl-model".into()).unwrap();
    let ssh_selection = ssh_requests.try_recv().unwrap();
    assert!(
        matches!(ssh_selection.request, Request::SetModel { model, .. } if model == "ssh-model")
    );
    ssh_selection.reply.unwrap().send(Ok(Response::Ok)).unwrap();
    let wsl_selection = wsl_requests.try_recv().unwrap();
    assert!(
        matches!(wsl_selection.request, Request::SetModel { model, .. } if model == "wsl-model")
    );
    wsl_selection.reply.unwrap().send(Ok(Response::Ok)).unwrap();
    ssh.submit("first target".into()).unwrap();
    wsl.submit("second target".into()).unwrap();
    assert!(
        matches!(ssh_requests.try_recv().unwrap().request, Request::Start { prompt, model: None, .. } if prompt == "first target")
    );
    assert!(
        matches!(wsl_requests.try_recv().unwrap().request, Request::Start { prompt, model: None, .. } if prompt == "second target")
    );
    ssh.state.input.set("ssh draft".into());
    wsl.state.input.set("wsl draft".into());
    let (sender, incoming) = mpsc::channel(CHANNEL_CAPACITY);
    ssh.incoming = Some(incoming);
    let mut tabs = Tabs::default();
    tabs.add_remote(ssh);
    let ssh_id = tabs.active_remote.clone().unwrap();
    tabs.add_remote(wsl);
    let wsl_id = tabs.active_remote.clone().unwrap();
    assert_ne!(ssh_id, wsl_id);
    sender
        .send(Ok(page(
            vec![Update::Started {
                presentation: Default::default(),
                prompt: "first target".into(),
            }],
            vec![],
        )))
        .await
        .unwrap();
    assert!(tabs.poll_remote().await);
    assert!(
        tabs.remote[&ssh_id]
            .state
            .transcript
            .iter()
            .any(|item| matches!(item, TranscriptItem::User(text) if text == "first target"))
    );
    assert!(
        !tabs.remote[&wsl_id]
            .state
            .transcript
            .iter()
            .any(|item| matches!(item, TranscriptItem::User(_)))
    );
    assert_eq!(tabs.remote[&ssh_id].state.input.text, "ssh draft");
    assert_eq!(tabs.remote[&wsl_id].state.input.text, "wsl draft");
    sender
        .send(Err(anyhow::anyhow!("transport disconnected")))
        .await
        .unwrap();
    tabs.poll_remote().await;
    assert!(tabs.remote[&ssh_id].offline);
    assert!(!tabs.remote[&wsl_id].offline);
    assert!(tabs.remote[&wsl_id].commands.is_some());
    assert_eq!(tabs.active_remote.as_deref(), Some(wsl_id.as_str()));
    tabs.sync_remote_ui();
    let event = Event::Key(KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT));
    assert_eq!(
        crate::tui::tab_target(&event, &tabs.remote[&wsl_id].state),
        Some(TabAction::Select(ssh_id.clone()))
    );
    tabs.remove_remote(&ssh_id);
    assert_eq!(tabs.active_remote.as_deref(), Some(wsl_id.as_str()));
    assert!(wsl_requests.try_recv().is_err());
}

#[test]
fn approval_keeps_composer_editable_and_enter_sends_steering() {
    let (mut tab, mut requests) = tab("local");
    tab.apply(page(vec![], vec![approval(7)]));
    tab.busy = true;
    tab.state.approval_shown = true;
    tab.handle(Event::Paste("draft".into())).unwrap();
    tab.handle(Event::Key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)))
        .unwrap();
    tab.handle(Event::Key(KeyEvent::new(
        KeyCode::Char('!'),
        KeyModifiers::NONE,
    )))
    .unwrap();
    assert_eq!(tab.state.input.text, "draf!t");
    tab.handle(Event::Key(KeyEvent::new(
        KeyCode::Char('a'),
        KeyModifiers::CONTROL,
    )))
    .unwrap();
    tab.handle(Event::Paste("use read_document".into()))
        .unwrap();
    assert_eq!(tab.state.input.text, "use read_document");
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|frame| crate::tui::render::draw(frame, &mut tab.state))
        .unwrap();
    assert!(tab.state.ime_cursor.is_some());
    tab.handle(Event::Key(KeyEvent::new(
        KeyCode::Enter,
        KeyModifiers::NONE,
    )))
    .unwrap();
    assert!(
        matches!(requests.try_recv().unwrap().request, Request::Steer { message, .. } if message == "use read_document")
    );
    assert!(requests.try_recv().is_err());
    assert_eq!(tab.approval_id, Some(7));
}

#[test]
fn approvals_and_cancellation_only_reach_the_selected_target() {
    let (mut first, mut first_requests) = tab("SSH one");
    let (mut second, mut second_requests) = tab("SSH two");
    first.apply(page(vec![], vec![approval(7)]));
    second.apply(page(vec![], vec![approval(7)]));
    second.state.approval_shown = true;
    second
        .handle(Event::Key(KeyEvent::new(
            KeyCode::Char('y'),
            KeyModifiers::CONTROL,
        )))
        .unwrap();
    assert!(matches!(
        second_requests.try_recv().unwrap().request,
        Request::Approve {
            approval: 7,
            allow: true,
            ..
        }
    ));
    assert!(first_requests.try_recv().is_err());
    first
        .handle(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )))
        .unwrap();
    assert!(matches!(
        first_requests.try_recv().unwrap().request,
        Request::Cancel { .. }
    ));
    assert!(second_requests.try_recv().is_err());
}

#[test]
fn workspace_completion_offers_shared_commands_without_local_directory_reads() {
    let (mut tab, _) = tab("SSH dev");
    tab.state.input.set("/".into());
    let menu = tab.state.menu();
    for command in [
        "/ssh",
        "/wsl",
        "/local",
        "/new",
        "/reconnect",
        "/cd",
        "/connect",
        "/undo",
    ] {
        assert!(
            menu.iter().any(|candidate| candidate.name == command),
            "{command}"
        );
    }
    tab.state.input.set("/cd /".into());
    assert!(tab.state.menu().is_empty());
}

#[test]
fn mixed_tabs_close_to_the_left_and_preserve_sibling_drafts() {
    let mut tabs = Tabs::default();
    let (mut local, mut local_requests) = tab("本地");
    local.state.input.set("local draft".into());
    local
        .state
        .transcript
        .push(TranscriptItem::User("local history".into()));
    let local_id = tabs.add_remote(local);
    let (mut ssh, mut ssh_requests) = tab("SSH dev");
    ssh.state.input.set("ssh draft".into());
    let ssh_id = tabs.add_remote(ssh);
    let (wsl, mut wsl_requests) = tab("WSL Ubuntu");
    let wsl_id = tabs.add_remote(wsl);
    tabs.sync_remote_ui();
    assert_eq!(tabs.remote[&wsl_id].state.tabs.len(), 3);
    standalone::close(&mut tabs, &wsl_id);
    assert_eq!(tabs.active_remote.as_deref(), Some(ssh_id.as_str()));
    standalone::close(&mut tabs, &ssh_id);
    assert_eq!(tabs.active_remote.as_deref(), Some(local_id.as_str()));
    assert_eq!(tabs.remote[&local_id].state.input.text, "local draft");
    assert!(
        matches!(&tabs.remote[&local_id].state.transcript[0], TranscriptItem::User(text) if text == "local history")
    );
    assert!(local_requests.try_recv().is_err());
    assert!(ssh_requests.try_recv().is_err());
    assert!(wsl_requests.try_recv().is_err());
}

#[tokio::test]
async fn cancelling_authentication_closes_only_its_own_prompt() {
    let (mut first, _) = tab("SSH first");
    let (mut second, _) = tab("SSH second");
    let (first_reply, first_answer) = tokio::sync::oneshot::channel();
    let (second_reply, mut second_answer) = tokio::sync::oneshot::channel();
    first.state.workspace_wizard = Some(wizard::Wizard::new(None));
    first.state.ssh_prompt = Some(auth::Prompt::new(workspace::interaction::Challenge {
        prompt: "first password".into(),
        confirm: false,
        reply: first_reply,
    }));
    second.state.ssh_prompt = Some(auth::Prompt::new(workspace::interaction::Challenge {
        prompt: "second password".into(),
        confirm: false,
        reply: second_reply,
    }));
    first
        .handle(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
        .unwrap();
    assert!(first_answer.await.is_err());
    assert!(first.state.ssh_prompt.is_none());
    assert!(first.state.workspace_wizard.as_ref().unwrap().failed);
    assert!(second_answer.try_recv().is_err());
    assert!(!second.state.ssh_prompt.as_ref().unwrap().closed());
    assert!(!second.offline);
}

#[test]
fn remote_modals_own_the_cursor_and_preserve_the_composer_draft() {
    let mut state = UiState::new("mock");
    state.input.set("draft".into());
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 34)).unwrap();
    terminal
        .draw(|frame| crate::tui::render::draw(frame, &mut state))
        .unwrap();
    assert!(terminal.backend().cursor_visible());

    state.workspace_wizard = Some(wizard::Wizard::new(Some(Spec::parse("/ssh").unwrap())));
    terminal
        .draw(|frame| crate::tui::render::draw(frame, &mut state))
        .unwrap();
    assert!(state.ime_cursor.is_none());
    assert!(terminal.backend().cursor_visible());
    assert!(
        !state
            .input_area
            .contains(terminal.backend().cursor_position())
    );

    let wizard = state.workspace_wizard.as_mut().unwrap();
    wizard.form.as_mut().unwrap().focus = 0;
    wizard.form.as_mut().unwrap().picker = false;
    terminal
        .draw(|frame| crate::tui::render::draw(frame, &mut state))
        .unwrap();
    assert!(terminal.backend().cursor_visible());
    assert!(
        !state
            .input_area
            .contains(terminal.backend().cursor_position())
    );

    state.workspace_wizard = Some(wizard::Wizard::new(None));
    terminal
        .draw(|frame| crate::tui::render::draw(frame, &mut state))
        .unwrap();
    assert!(!terminal.backend().cursor_visible());
    let (reply, _answer) = tokio::sync::oneshot::channel();
    state.ssh_prompt = Some(auth::Prompt::new(workspace::interaction::Challenge {
        prompt: "password:".into(),
        confirm: false,
        reply,
    }));
    terminal
        .draw(|frame| crate::tui::render::draw(frame, &mut state))
        .unwrap();
    assert!(terminal.backend().cursor_visible());
    assert!(
        !state
            .input_area
            .contains(terminal.backend().cursor_position())
    );
    assert_eq!(state.input.text, "draft");
}

#[test]
fn failed_setup_retains_the_last_diagnostic_without_duplicating_it() {
    let mut tab = Tab::connecting(Spec::parse("/ssh").unwrap(), media::Context::default());
    let (progress, updates) = workspace::interaction::channel();
    progress.log("loader diagnostic");
    tab.updates = Some(updates);
    tab.disconnected("target cannot run the executable".into());
    let wizard = tab.state.workspace_wizard.as_ref().unwrap();
    assert_eq!(wizard.logs, ["loader diagnostic"]);
    assert_eq!(wizard.status, "target cannot run the executable");
    assert_eq!(tab.state.model, "连接失败");
    assert!(tab.state.transcript.is_empty());
}

#[test]
fn file_paths_are_sent_as_messages_on_every_target() {
    for target in ["本地", "SSH dev", "WSL Ubuntu"] {
        for path in [
            "/home/user/report.txt 请看看",
            "/tmp/report.txt",
            "/报告.txt",
        ] {
            let (mut tab, mut requests) = tab(target);
            tab.submit(path.into()).unwrap();
            assert!(matches!(requests.try_recv().unwrap().request,
                Request::Start { prompt, .. } if prompt == path));
        }
        let (mut tab, mut requests) = tab(target);
        tab.submit("/stauts".into()).unwrap();
        assert!(requests.try_recv().is_err());
        assert!(render_screen(&mut tab.state).contains("/status"));
        assert!(tab.state.info_dialog.is_none());
    }
}

#[test]
fn clipboard_regular_files_are_references_without_image_decoding() {
    let (mut tab, mut requests) = tab("本地");
    tab.state.supports_vision = false;
    tab.paste_payload(crate::tui::attachments::ClipboardPayload::Files(vec![
        "/tmp/source code.rs".into(),
        "/tmp/spec.pdf".into(),
    ]))
    .unwrap();
    assert_eq!(
        tab.state.input.text,
        "[文件：/tmp/source code.rs]\n[文件：/tmp/spec.pdf]"
    );
    assert!(tab.attachment_task.is_none());
    assert!(requests.try_recv().is_err());
}

#[test]
fn compaction_and_images_use_the_same_request_for_every_target() {
    for target in ["本地", "SSH dev", "WSL Ubuntu"] {
        let (mut tab, mut requests) = tab(target);
        tab.submit("/compact".into()).unwrap();
        assert!(
            matches!(requests.try_recv().unwrap().request, Request::Start { compact: true, images, .. } if images.is_empty())
        );
    }
}

#[test]
fn steering_is_visible_before_receipt_and_is_reconciled_by_message_id() {
    use orbit_agent::events::AgentEvent;
    use orbit_model::types::StreamEvent;
    for receipt_first in [true, false] {
        let (mut tab, mut requests) = tab("local");
        tab.state.submit("initial question".into());
        tab.busy = true;
        tab.state
            .apply(AgentEvent::Model(StreamEvent::TextDelta("before ".into())));
        let mut replies = Vec::new();
        for _ in 0..2 {
            tab.state.input.set("same message".into());
            assert!(matches!(
                tab.handle(Event::Key(KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::NONE
                )))
                .unwrap(),
                Action::Redraw
            ));
            replies.push(requests.try_recv().unwrap().reply.unwrap());
        }
        let screen = render_screen(&mut tab.state);
        assert!(screen.contains("same message") && screen.replace(' ', "").contains("发送中"));
        assert_eq!(tab.state.pending_input.count(), 2);
        tab.state
            .apply(AgentEvent::Model(StreamEvent::TextDelta("after".into())));
        assert!(
            matches!(tab.state.transcript.last(), Some(TranscriptItem::Assistant(text)) if text == "before after")
        );
        if receipt_first {
            for (id, reply) in [10, 11].into_iter().zip(replies.drain(..)) {
                reply
                    .send(Ok(Response::InputQueued { message_id: id }))
                    .unwrap();
            }
            tab.poll_submissions();
            assert!(
                render_screen(&mut tab.state)
                    .replace(' ', "")
                    .contains("等待处理")
            );
        }
        // Another client can submit identical text; it must not consume our pending rows.
        tab.state.apply(AgentEvent::InputReceived {
            message_id: 20,
            content: "same message".into(),
        });
        assert_eq!(tab.state.pending_input.count(), 2);
        for id in [10, 11] {
            tab.state.apply(AgentEvent::InputReceived {
                message_id: id,
                content: "same message".into(),
            });
        }
        if !receipt_first {
            for (id, reply) in [10, 11].into_iter().zip(replies) {
                reply
                    .send(Ok(Response::InputQueued { message_id: id }))
                    .unwrap();
            }
            tab.poll_submissions();
        }
        assert_eq!(tab.state.pending_input.count(), 0);
        assert_eq!(
            tab.state
                .transcript
                .iter()
                .filter(|item| matches!(item, TranscriptItem::User(text) if text == "same message"))
                .count(),
            3
        );
        assert!(tab.state.input.text.is_empty());
    }
}

#[test]
fn rejected_steering_restores_the_draft_without_overwriting_new_input() {
    let (mut tab, mut requests) = tab("local");
    tab.submit_with_receipt(
        Request::Steer {
            session: "same-session".into(),
            message: "more context".into(),
        },
        submissions::Draft::Composer("more context".into()),
    );
    assert_eq!(tab.state.pending_input.count(), 1);
    requests
        .try_recv()
        .unwrap()
        .reply
        .unwrap()
        .send(Ok(Response::Error("session is idle".into())))
        .unwrap();
    assert!(tab.poll_submissions());
    assert_eq!(tab.state.pending_input.count(), 0);
    assert_eq!(tab.state.input.text, "more context");
    tab.state.input.set("new draft".into());
    tab.submit_with_receipt(
        Request::Steer {
            session: "same-session".into(),
            message: "previous".into(),
        },
        submissions::Draft::Composer("previous".into()),
    );
    drop(requests.try_recv().unwrap());
    tab.poll_submissions();
    assert_eq!(tab.state.pending_input.count(), 0);
    assert_eq!(tab.state.input.text, "new draft");
    assert!(
        render_screen(&mut tab.state)
            .replace(' ', "")
            .contains("请求确认前连接已关闭")
    );
    assert!(tab.state.info_dialog.is_none());
    assert!(tab.state.transcript.is_empty());
}

#[test]
fn side_question_receipts_open_the_existing_modal_and_keep_main_draft() {
    let (mut tab, mut requests) = tab("SSH dev");
    tab.state.input.set("main draft".into());
    tab.ask("why?".into(), None);
    let call = requests.try_recv().unwrap();
    assert!(matches!(
        call.request,
        Request::Ask {
            follow_up: None,
            ..
        }
    ));
    call.reply
        .unwrap()
        .send(Ok(Response::Questions(orbit_protocol::Questions {
            revision: 1,
            selected: Some(0),
            running: true,
            records: Some(vec![orbit_protocol::SideQuestion {
                thread_id: "one".into(),
                model: "mock".into(),
                question: "why?".into(),
                answer: "because".into(),
                status: "running".into(),
                error: None,
            }]),
        })))
        .unwrap();
    assert!(tab.poll_submissions());
    assert!(tab.state.btw.visible());
    assert_eq!(tab.state.input.text, "main draft");
    tab.handle(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
        .unwrap();
    assert!(!tab.state.btw.visible());
    assert!(!tab.state.btw.has_records());
    assert!(matches!(
        requests.try_recv().unwrap().request,
        Request::CloseQuestions { .. }
    ));
    tab.poll_questions();
    assert!(requests.try_recv().is_err());
    assert_eq!(tab.state.input.text, "main draft");
}

pub(in crate::tui) fn connected_tab(target: workspace::transport::Target) -> Tab {
    let (mut tab, _) = tab(&target.label());
    tab.persisted = true;
    tab.connection = Some(Arc::new(Connection {
        target,
        workspace: "/project".into(),
        executable: "/nonexistent-orbit-test-server".into(),
        config: "/config.toml".into(),
        local_configuration: None,
        database: None,
        auth_dir: None,
        allow_mcp_start: false,
        unattended: true,
        interactive: None,
    }));
    tab
}

fn saved_local(id: &str) -> workspace::windows::SavedTab {
    workspace::windows::SavedTab {
        id: id.into(),
        title: format!("本地 · {id}"),
        target: workspace::transport::Target::Local,
        workspace: "/project".into(),
        config: "/nonexistent-orbit-test-config.toml".into(),
        auth: "/auth".into(),
        database: None,
        allow_mcp_start: false,
    }
}

#[tokio::test]
async fn restore_reuses_home_immediately_and_preserves_the_other_restored_tabs() {
    use workspace::transport::Target;
    for count in [1, 2] {
        let mut tabs = crate::tui::tabs::live::Tabs::default();
        let source = tabs.add_remote(connected_tab(Target::Local));
        let mut first = saved_local("first");
        first.workspace = "/another-project".into();
        let second = saved_local("second");
        let saved = [&first, &second];
        tabs.restore_tabs(&source, &saved[..count]);
        assert_eq!(tabs.remote.len(), count);
        assert_eq!(tabs.target(0), Some(source.as_str()));
        assert!(tabs.remote[&source].connecting.is_some());
        assert_eq!(tabs.remote[&source].state.workspace, first.workspace);
        tabs.sync_remote_ui();
        let active = tabs.active_remote.clone().unwrap();
        assert_eq!(tabs.remote[&active].state.tabs.len(), count);
        let order: Vec<_> = (0..count)
            .map(|index| tabs.target(index).unwrap().to_owned())
            .collect();

        let mut restored = connected_tab(Target::Local);
        restored.id = Some(first.id);
        restored
            .state
            .transcript
            .push(TranscriptItem::User("saved conversation".into()));
        tabs.remote.insert(source.clone(), restored);
        tabs.poll_remote().await;
        assert_eq!(tabs.active_remote.as_ref(), Some(&active));
        assert_eq!(tabs.remote.len(), count);
        for (index, id) in order.iter().enumerate() {
            assert_eq!(tabs.target(index), Some(id.as_str()));
        }
    }
}

#[tokio::test]
async fn restore_failures_can_retry_in_the_same_tab() {
    use workspace::transport::Target;
    let mut tabs = crate::tui::tabs::live::Tabs::default();
    let source = tabs.add_remote(connected_tab(Target::Local));
    let saved = saved_local("failed");
    tabs.restore_tabs(&source, &[&saved]);
    let restored = tabs.active_remote.clone().unwrap();
    assert_eq!(restored, source);
    assert_eq!(tabs.remote.len(), 1);
    let task = tabs
        .remote
        .get_mut(&restored)
        .unwrap()
        .connecting
        .take()
        .unwrap();
    assert!(task.await.unwrap().is_err());
    tabs.remote
        .get_mut(&restored)
        .unwrap()
        .disconnected("connection failed".into());
    tabs.poll_remote().await;
    assert_eq!(tabs.remote.len(), 1);
    tabs.remote.get_mut(&restored).unwrap().reconnect();
    assert!(tabs.remote[&restored].connecting.is_some());
    assert_eq!(tabs.remote[&restored].state.workspace, saved.workspace);

    let mut retried = connected_tab(Target::Local);
    retried.id = Some(saved.id);
    tabs.remote.insert(restored.clone(), retried);
    tabs.poll_remote().await;
    assert_eq!(tabs.remote.len(), 1);
    assert_eq!(tabs.active_remote.as_ref(), Some(&restored));
}

#[tokio::test]
async fn restore_keeps_drafts_and_reuses_already_open_sessions() {
    use workspace::transport::Target;
    for has_draft in [false, true] {
        let mut tabs = crate::tui::tabs::live::Tabs::default();
        let mut source = connected_tab(Target::Local);
        if has_draft {
            source.state.input.set("keep draft".into());
        }
        let source = tabs.add_remote(source);
        let saved = saved_local("saved");
        tabs.restore_tabs(&source, &[&saved]);
        let restored = tabs.active_remote.clone().unwrap();
        assert_eq!(tabs.remote.len(), if has_draft { 2 } else { 1 });
        assert_eq!(restored == source, !has_draft);
        let mut ready = connected_tab(Target::Local);
        ready.id = Some(saved.id.clone());
        tabs.remote.insert(restored.clone(), ready);
        tabs.poll_remote().await;
        if has_draft {
            assert_eq!(tabs.remote[&source].state.input.text, "keep draft");
            tabs.restore_tabs(&source, &[&saved]);
            assert_eq!(tabs.remote.len(), 2);
            assert_eq!(tabs.active_remote.as_ref(), Some(&restored));
        }
    }
    let mut tabs = crate::tui::tabs::live::Tabs::default();
    let mut restored = connected_tab(Target::Local);
    restored.id = Some("saved".into());
    let restored = tabs.add_remote(restored);
    let source = tabs.add_remote(connected_tab(Target::Local));
    tabs.restore_tabs(&source, &[&saved_local("saved")]);
    assert_eq!(tabs.remote.len(), 1);
    assert_eq!(tabs.active_remote.as_ref(), Some(&restored));
}

#[test]
fn tab_titles_show_targets_only_while_different_environments_are_open() {
    use workspace::transport::{SshOptions, Target};
    let ssh = |host: &str| Target::Ssh {
        host: host.into(),
        options: SshOptions::default(),
    };
    for target in [
        Target::Local,
        ssh("dev"),
        Target::Wsl {
            distribution: "Ubuntu".into(),
            user: None,
        },
    ] {
        let mut tabs = crate::tui::tabs::live::Tabs::default();
        let label = target.label();
        for title in ["first", "second"] {
            let mut tab = connected_tab(target.clone());
            tab.title = format!("{label} · {title}");
            tabs.add_remote(tab);
        }
        tabs.sync_remote_ui();
        let active = tabs.active_remote.clone().unwrap();
        assert_eq!(
            tabs.remote[&active]
                .state
                .tabs
                .iter()
                .map(|tab| tab.title.as_str())
                .collect::<Vec<_>>(),
            ["first", "second"]
        );
        let mut other = connected_tab(ssh("other"));
        other.title = "SSH other · third".into();
        let other = tabs.add_remote(other);
        tabs.sync_remote_ui();
        assert_eq!(
            tabs.remote[&active].state.tabs[0].title,
            format!("{label} · first")
        );
        tabs.remove_remote(&other);
        tabs.active_remote = Some(active.clone());
        tabs.sync_remote_ui();
        assert_eq!(tabs.remote[&active].state.tabs[0].title, "first");
        assert_eq!(tabs.remote[&active].title, format!("{label} · second"));
        assert_eq!(
            tabs.remote[&active].state.connection_label.as_deref(),
            Some(label.as_str())
        );
    }
}

#[tokio::test]
async fn connecting_targets_also_participate_in_tab_labels() {
    use workspace::transport::Target;
    let mut tabs = crate::tui::tabs::live::Tabs::default();
    let mut local = connected_tab(Target::Local);
    local.title = "本地 · project".into();
    let local = tabs.add_remote(local);
    let remote = tabs.add_remote(Tab::connecting(
        Spec::parse("/ssh dev").unwrap(),
        media::Context::default(),
    ));
    tabs.sync_remote_ui();
    assert_eq!(tabs.remote[&local].state.tabs[0].title, "本地 · project");
    assert_eq!(tabs.remote[&local].state.tabs[1].title, "SSH dev");
    tabs.remove_remote(&remote);
    tabs.active_remote = Some(local.clone());
    tabs.sync_remote_ui();
    assert_eq!(tabs.remote[&local].state.tabs[0].title, "project");
}

#[tokio::test]
async fn resume_reuses_empty_tab_in_place_for_every_target() {
    use workspace::transport::{SshOptions, Target};
    for target in [
        Target::Local,
        Target::Ssh {
            host: "dev".into(),
            options: SshOptions::default(),
        },
        Target::Wsl {
            distribution: "Ubuntu".into(),
            user: None,
        },
    ] {
        let mut tabs = crate::tui::tabs::live::Tabs::default();
        tabs.add_remote(connected_tab(target));
        let source = tabs.active_remote.clone().unwrap();
        tabs.open_session(&source, Some("history".into()));
        assert_eq!(tabs.remote.len(), 1);
        assert_eq!(tabs.target(0), Some(source.as_str()));
        assert_eq!(tabs.active_remote.as_deref(), Some(source.as_str()));
        assert!(tabs.remote[&source].connecting.is_some());
    }
}

#[tokio::test]
async fn resume_preserves_nonempty_tabs_and_new_always_adds_a_tab() {
    use workspace::transport::Target;
    let changes: [fn(&mut Tab); 6] = [
        |tab| tab.state.input.set("unsent draft".into()),
        |tab| {
            tab.state
                .attachments
                .push(orbit_model::types::Attachment::Image {
                    lease: None,
                    name: "draft.png".into(),
                    mime_type: "image/png".into(),
                    path: "/draft.png".into(),
                })
        },
        |tab| tab.busy = true,
        |tab| tab.starting = Some("pending".into()),
        |tab| {
            tab.state
                .transcript
                .push(TranscriptItem::User("existing conversation".into()))
        },
        |tab| {
            tab.attachment_task = Some(AbortOnDropHandle::new(tokio::spawn(std::future::pending())))
        },
    ];
    for change in changes {
        let mut tabs = crate::tui::tabs::live::Tabs::default();
        let mut tab = connected_tab(Target::Local);
        change(&mut tab);
        tabs.add_remote(tab);
        let source = tabs.active_remote.clone().unwrap();
        tabs.open_session(&source, Some("history".into()));
        assert_eq!(tabs.remote.len(), 2);
        assert!(tabs.remote.contains_key(&source));
    }
    let mut tabs = crate::tui::tabs::live::Tabs::default();
    tabs.add_remote(connected_tab(Target::Local));
    let source = tabs.active_remote.clone().unwrap();
    tabs.open_session(&source, None);
    assert_eq!(tabs.remote.len(), 2);
}

#[test]
fn resume_focuses_open_conversation_and_removes_only_empty_source() {
    use workspace::transport::Target;
    for draft in ["", "keep this draft"] {
        let mut tabs = crate::tui::tabs::live::Tabs::default();
        let mut history = connected_tab(Target::Local);
        history.id = Some("history".into());
        tabs.add_remote(history);
        let history_tab = tabs.active_remote.clone().unwrap();
        let mut source = connected_tab(Target::Local);
        source.state.input.set(draft.into());
        tabs.add_remote(source);
        let source = tabs.active_remote.clone().unwrap();
        tabs.open_session(&source, Some("history".into()));
        assert_eq!(tabs.active_remote.as_ref(), Some(&history_tab));
        assert_eq!(tabs.remote.contains_key(&source), !draft.is_empty());
    }
}

#[test]
fn directory_profile_updates_workspace_media_connection_and_empty_tab_title() {
    let mut tab = connected_tab(workspace::transport::Target::Local);
    let info: SessionInfo = serde_json::from_value(serde_json::json!({
        "id":"same-session", "title":"", "workspace":"/next/project", "model":"mock", "models":[],
        "supports_vision":false, "reasoning_effort":null, "reasoning_efforts":[],
        "context_tokens":1000, "input_context_tokens":900, "max_output_tokens":100
    }))
    .unwrap();
    tab.apply(Response::Ok);
    tab.apply(Response::Page(Page {
        user_inputs: Vec::new(),
        planning: Default::default(),
        tool_progress: Vec::new(),
        timing: None,
        next: 1,
        more: false,
        busy: false,
        model: "mock".into(),
        updates: vec![Update::Profile(Box::new(info.clone()))],
        approvals: Vec::new(),
    }));
    assert_eq!(tab.state.workspace, "/next/project");
    assert_eq!(
        tab.state.media.workspace,
        std::path::Path::new("/next/project")
    );
    assert_eq!(tab.connection.as_ref().unwrap().workspace, "/next/project");
    assert_eq!(tab.title, "本地 · project");
    assert!(tab.state.info_dialog.is_none());
    let mut info = info;
    info.title = "saved title".into();
    tab.profile(&info);
    assert_eq!(tab.title, "本地 · saved title");
}

#[test]
fn directory_enter_keeps_completed_path_instead_of_choosing_its_first_child() {
    for label in ["本地", "SSH dev", "WSL Ubuntu"] {
        let (mut tab, mut requests) = tab(label);
        tab.state.input.set("/cd ~/Tests/TC_learn/".into());
        tab.apply(Response::Completions {
            input: tab.state.input.text.clone(),
            entries: vec![crate::tui::completion::Candidate {
                name: "/cd ~/Tests/TC_learn/_check/".into(),
                display: Some("_check/".into()),
                description: String::new(),
                continue_input: true,
            }],
        });
        tab.handle(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )))
        .unwrap();
        let call = requests.try_recv().unwrap();
        assert!(
            matches!(call.request, Request::SessionAction { action: orbit_protocol::SessionAction::ChangeDirectory { path }, .. } if path == "~/Tests/TC_learn/")
        );
        assert!(tab.state.info_dialog.is_none());
    }
}

#[test]
fn model_preference_changes_only_after_successful_selection() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, "[providers]\n").unwrap();
    let local = workspace::configuration::Local {
        config,
        auth: root.path().join("auth"),
        model: None,
    };
    local.remember_model("old::model").unwrap();
    let mut tab = connected_tab(workspace::transport::Target::Local);
    Arc::make_mut(tab.connection.as_mut().unwrap()).local_configuration = Some(local.clone());
    let (commands, mut requests) = mpsc::channel(CHANNEL_CAPACITY);
    tab.commands = Some(Arc::new(commands));
    tab.state.model_completions = vec!["new::model".into()];
    for accepted in [false, true] {
        tab.submit("/model new::model".into()).unwrap();
        assert_eq!(local.last_model().unwrap().as_deref(), Some("old::model"));
        let call = requests.try_recv().unwrap();
        call.reply
            .unwrap()
            .send(Ok(if accepted {
                Response::Ok
            } else {
                Response::Error("unavailable".into())
            }))
            .unwrap();
        assert!(tab.poll_submissions());
        assert_eq!(
            local.last_model().unwrap().as_deref(),
            Some(if accepted { "new::model" } else { "old::model" })
        );
    }
}

#[tokio::test]
async fn restoring_local_tabs_and_reconnecting_never_opens_remote_configuration() {
    use workspace::{transport::Target, windows::SavedTab};
    let mut tabs = crate::tui::tabs::live::Tabs::default();
    for id in ["first", "second"] {
        let saved = SavedTab {
            id: id.into(),
            title: id.into(),
            target: Target::Local,
            workspace: "/project".into(),
            config: "/config.toml".into(),
            auth: "/auth".into(),
            database: None,
            allow_mcp_start: false,
        };
        let tab = Tab::connecting(Spec::restored(&saved), media::Context::default());
        assert_eq!(tab.title, "本地");
        assert!(tab.state.workspace_wizard.is_none());
        assert!(tab.connecting.is_some());
        tabs.add_remote(tab);
    }
    assert_eq!(tabs.remote.len(), 2);
    let mut tab = connected_tab(Target::Local);
    let sibling = tab.sibling(Some("history".into()));
    assert!(sibling.state.workspace_wizard.is_none());
    tab.disconnected("test failure".into());
    assert!(tab.state.workspace_wizard.is_none());
    tab.reconnect();
    assert!(tab.connecting.is_some());
    assert!(tab.state.workspace_wizard.is_none());
}

#[tokio::test]
async fn directory_menu_stays_visible_while_refining_and_waiting_for_the_service() {
    use ratatui::{Terminal, backend::TestBackend};
    let (mut tab, mut requests) = tab("本地");
    let entries = ["Templates", "Testbed", "Tests"]
        .map(|name| crate::tui::completion::Candidate {
            name: format!("/cd ~/{name}/"),
            display: Some(format!("{name}/")),
            description: String::new(),
            continue_input: true,
        })
        .to_vec();
    tab.state.input.set("/cd ~/Te".into());
    tab.apply(Response::Completions {
        input: tab.state.input.text.clone(),
        entries: entries.clone(),
    });
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    for query in ["/cd ~/Tes", "/cd ~/Tests", "/cd ~/Tes", "/cd '~/TES"] {
        tab.state.input.set(query.into());
        let before = tab.state.menu();
        assert!(!before.is_empty(), "{query}");
        assert!(
            before[0]
                .display
                .as_deref()
                .unwrap()
                .to_lowercase()
                .starts_with("tes")
        );
        terminal
            .draw(|frame| crate::tui::render::draw(frame, &mut tab.state))
            .unwrap();
        assert!(!tab.state.menu_rows.is_empty());
        tab.next_completion = std::time::Instant::now();
        tab.poll().await;
        assert_eq!(
            tab.state
                .menu()
                .iter()
                .map(|entry| &entry.name)
                .collect::<Vec<_>>(),
            before.iter().map(|entry| &entry.name).collect::<Vec<_>>()
        );
        terminal
            .draw(|frame| crate::tui::render::draw(frame, &mut tab.state))
            .unwrap();
        assert!(!tab.state.menu_rows.is_empty());
        let mut requested = false;
        while let Ok(call) = requests.try_recv() {
            requested |=
                matches!(call.request, Request::Complete { ref input, .. } if input == query);
        }
        assert!(requested);
    }
    tab.state.input.set("/cd ~/Tests/".into());
    assert!(tab.state.menu().is_empty());
    terminal
        .draw(|frame| crate::tui::render::draw(frame, &mut tab.state))
        .unwrap();
    assert!(tab.state.menu_area.height > 0);
    assert!(tab.state.menu_rows.is_empty());
    assert!(tab.state.directory_completion_pending());
    tab.apply(Response::Completions {
        input: "/cd ~/Tes".into(),
        entries,
    });
    assert!(tab.state.menu().is_empty());
    tab.state.input.set("/model tes".into());
    assert!(
        tab.state
            .menu()
            .iter()
            .all(|entry| !entry.name.starts_with("/cd "))
    );
    tab.state.input.set("/cd ~/Tests/".into());
    let entry = crate::tui::completion::Candidate {
        name: "/cd ~/Tests/src/".into(),
        display: Some("src/".into()),
        description: String::new(),
        continue_input: true,
    };
    tab.apply(Response::Completions {
        input: tab.state.input.text.clone(),
        entries: vec![entry],
    });
    assert_eq!(tab.state.menu()[0].name, "/cd ~/Tests/src/");
    tab.apply(Response::Completions {
        input: tab.state.input.text.clone(),
        entries: vec![],
    });
    assert!(tab.state.menu().is_empty());
    assert!(!tab.state.directory_completion_pending());
}

mod permissions;
mod reconnection;
mod regressions;

#[test]
fn user_input_submission_uses_each_target_connection_and_preserves_composer() {
    for label in ["本地", "SSH test-host", "WSL Test"] {
        let (mut tab, mut requests) = tab(label);
        tab.state.input.set("尚未发送的消息".into());
        let record: orbit_protocol::user_input::Record =
            serde_json::from_value(serde_json::json!({
                "id":"request","call_id":"tool-call","source":null,"status":"pending","answers":[],
                "form":{"questions":[{"id":"q","title":"要求","prompt":"需要什么？","kind":"text"}]}
            }))
            .unwrap();
        tab.state.user_input.sync(vec![record]);
        tab.state.user_input.open_next();
        tab.handle(Event::Paste("用户填写".into())).unwrap();
        tab.handle(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )))
        .unwrap();
        tab.handle(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )))
        .unwrap();
        let call = requests.try_recv().unwrap();
        let Request::AnswerUserInput {
            session,
            request,
            answers,
        } = call.request
        else {
            panic!("expected answer request");
        };
        assert_eq!(session, "same-session");
        assert_eq!(request, "request");
        assert_eq!(answers.unwrap()[0].text, "用户填写");
        call.reply
            .unwrap()
            .send(Ok(Response::Error("临时失败".into())))
            .unwrap();
        tab.poll_submissions();
        assert!(tab.state.user_input.visible());
        assert_eq!(tab.state.input.text, "尚未发送的消息");
        tab.handle(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )))
        .unwrap();
        let retry = requests.try_recv().unwrap();
        assert!(matches!(retry.request, Request::AnswerUserInput { .. }));
        retry.reply.unwrap().send(Ok(Response::Ok)).unwrap();
        assert!(tab.poll_submissions());
        assert!(!tab.state.user_input.visible());
        tab.state.user_input.open_next();
        assert!(!tab.state.user_input.visible());
        assert_eq!(tab.state.input.text, "尚未发送的消息");
        assert!(matches!(
            requests.try_recv().unwrap().request,
            Request::UserInputHistory { .. }
        ));
        assert!(requests.try_recv().is_err());
    }
}

#[test]
fn planning_commands_and_review_decisions_use_each_tabs_connection() {
    use orbit_protocol::plan::{Action as PlanAction, WorkMode};
    for label in ["本地", "SSH dev", "WSL Ubuntu"] {
        let (mut tab, mut requests) = tab(label);
        tab.submit("/plan".into()).unwrap();
        assert!(matches!(
            requests.try_recv().unwrap().request,
            Request::Plan {
                action: PlanAction::Enter,
                ..
            }
        ));
        // The client never grants a mode transition optimistically.
        assert_eq!(tab.state.planning.mode(), WorkMode::Build);
        tab.state.planning.sync(serde_json::from_value(serde_json::json!({"generation":3,"mode":"plan","plan":{"id":"plan","workspace":"/project","revision":1,"title":"Plan","markdown":"Implement and test.","steps":["Implement"],"status":"review"}})).unwrap());
        tab.busy = true;
        tab.submit("/plan".into()).unwrap();
        assert!(requests.try_recv().is_err());
        assert!(tab.state.planning.visible());
        tab.handle(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )))
        .unwrap();
        assert!(requests.try_recv().is_err());
        for _ in 0..2 {
            tab.handle(Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)))
                .unwrap();
        }
        tab.handle(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )))
        .unwrap();
        let call = requests.try_recv().unwrap();
        assert!(matches!(
            call.request,
            Request::Plan {
                action: PlanAction::Approve { generation: 3 },
                ..
            }
        ));
        call.reply.unwrap().send(Ok(Response::Ok)).unwrap();
        assert!(tab.poll_submissions());
        assert!(!tab.state.planning.visible());
        tab.submit("/build".into()).unwrap();
        assert!(matches!(
            requests.try_recv().unwrap().request,
            Request::Plan {
                action: PlanAction::Leave,
                ..
            }
        ));
    }
}

// Shared UI fixtures deliver the same protocol pages consumed by a real connection.
pub(in crate::tui) fn replayed_tab(media: media::Context, updates: Vec<Update>) -> Tab {
    let (mut tab, _requests) = tab("本地");
    tab.state.media = media;
    tab.apply(page(updates, vec![]));
    tab
}

#[test]
fn local_commands_and_service_reports_do_not_create_a_conversation_on_home() {
    let (mut tab, mut requests) = tab("本地");
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|f| crate::tui::render::draw(f, &mut tab.state))
        .unwrap();
    let input_area = tab.state.input_area;
    for command in [
        "/mode ask",
        "/rename Home",
        "/tasks add task",
        "/status",
        "/context",
    ] {
        tab.submit(command.into()).unwrap();
        let request = requests.try_recv().unwrap().request;
        assert!(!matches!(
            request,
            Request::Start { .. } | Request::Steer { .. }
        ));
        assert!(tab.state.is_home());
        assert!(tab.state.transcript.is_empty());
    }
    tab.apply(Response::Report(orbit_protocol::Report {
        title: "Context".into(),
        text: "context report\nbudget details".into(),
    }));
    assert!(tab.state.info_dialog.is_some());
    tab.handle(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
        .unwrap();
    terminal
        .draw(|f| crate::tui::render::draw(f, &mut tab.state))
        .unwrap();
    assert!(tab.state.is_home());
    assert!(tab.state.transcript.is_empty());
    assert_eq!(tab.state.input_area, input_area);
}

#[test]
fn closing_btw_drops_late_receipts_and_retries_a_full_transport_queue() {
    let (mut tab, mut requests) = tab("本地");
    tab.ask("follow-up".into(), Some(0));
    let late_reply = requests.try_recv().unwrap().reply.unwrap();
    tab.poll_questions();
    // Fill the actual transport queue: closing must eventually reach the server.
    for _ in 0..CHANNEL_CAPACITY {
        assert!(tab.send(Request::ReadQuestions {
            session: "same-session".into(),
            after: None
        }));
    }
    tab.close_questions();
    assert!(tab.question_closing);
    assert!(tab.submissions.is_empty());
    assert!(
        late_reply
            .send(Ok(Response::Questions(orbit_protocol::Questions {
                revision: 3,
                selected: Some(0),
                running: false,
                records: Some(vec![]),
            })))
            .is_err()
    );
    requests.try_recv().unwrap();
    tab.poll_questions();
    assert!(!tab.question_closing);
    let mut closes = 0;
    while let Ok(call) = requests.try_recv() {
        closes += usize::from(matches!(call.request, Request::CloseQuestions { .. }));
    }
    assert_eq!(closes, 1);
    assert!(!tab.state.btw.visible());
    assert!(!tab.state.btw.has_records());
}

#[test]
fn closing_btw_drops_an_outstanding_poll_without_preserving_a_draft_session() {
    let (mut tab, mut requests) = tab("本地");
    tab.ask("why?".into(), None);
    assert!(!tab.submissions[0].starts_session());
    requests.try_recv().unwrap();
    let (reply, response) = tokio::sync::oneshot::channel();
    tab.question_poll = Some(response);
    tab.close_questions();
    assert!(
        reply
            .send(Ok(Response::Questions(orbit_protocol::Questions {
                revision: 3,
                selected: None,
                running: true,
                records: Some(vec![]),
            })))
            .is_err()
    );
    tab.poll_submissions();
    tab.poll_questions();
    assert!(!tab.state.btw.visible());
    assert!(tab.question_poll.is_none());
}
