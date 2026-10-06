use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};

use super::*;
use crate::{
    tui::{TabAction, tabs::Tabs},
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
            next: 3,
            more: false,
            busy: false,
            model: "child-model".into(),
            approvals: vec![],
            updates: vec![
                Update::Started {
                    prompt: "child task".into(),
                },
                Update::agent(orbit_agent::events::AgentEvent::Model(
                    orbit_model::types::StreamEvent::TextDelta("child answer".into()),
                )),
                Update::Finished {
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
    call.reply.unwrap().send(Ok(Response::Ok)).unwrap();
    tab.poll_subagent();
    tab.handle(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
        .unwrap();
    assert!(tab.state.subagent_dialog.is_none());
    assert_eq!(tab.state.input.text, "parent draft");
}

fn page(updates: Vec<Update>, approvals: Vec<Approval>) -> Response {
    Response::Page(Page {
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
    assert!(
        matches!(ssh_requests.try_recv().unwrap().request, Request::SetModel { model, .. } if model == "ssh-model")
    );
    assert!(
        matches!(wsl_requests.try_recv().unwrap().request, Request::SetModel { model, .. } if model == "wsl-model")
    );
    ssh.submit("/mode ask".into()).unwrap();
    assert!(matches!(
        ssh_requests.try_recv().unwrap().request,
        Request::SetMode {
            mode: Mode::Ask,
            ..
        }
    ));
    ssh.submit("first target".into()).unwrap();
    wsl.submit("second target".into()).unwrap();
    assert!(
        matches!(ssh_requests.try_recv().unwrap().request, Request::Start { prompt, mode: Mode::Ask, model: None, .. } if prompt == "first target")
    );
    assert!(
        matches!(wsl_requests.try_recv().unwrap().request, Request::Start { prompt, mode: Mode::Auto, model: None, .. } if prompt == "second target")
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
fn approvals_and_cancellation_only_reach_the_selected_target() {
    let (mut first, mut first_requests) = tab("SSH one");
    let (mut second, mut second_requests) = tab("SSH two");
    first.apply(page(vec![], vec![approval(7)]));
    second.apply(page(vec![], vec![approval(7)]));
    second.state.approval_shown = true;
    second
        .handle(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
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

#[tokio::test]
async fn mixed_tabs_close_to_the_left_without_rebinding_the_local_executor() {
    use crate::session::SessionState;
    use orbit_model::{client::ModelClient, types::ChatMessage};

    let directory = tempfile::tempdir().unwrap();
    let mut session = SessionState::open_shared_in(
        directory.path().join("sessions.db"),
        None,
        "mock".into(),
        directory.path(),
    )
    .await
    .unwrap();
    let local_id = session.id().to_owned();
    let mut local = UiState::new("mock");
    local.input.set("local draft".into());
    let mut model = ModelClient::new(Default::default()).unwrap();
    let mut history = vec![ChatMessage::user("local history")];
    let mut tabs = Tabs::new(&session);
    let (ssh, mut requests) = tab("SSH dev");
    tabs.add_remote(ssh);
    let ssh_id = tabs.active_remote.clone().unwrap();
    let (wsl, _) = tab("WSL Ubuntu");
    tabs.add_remote(wsl);
    let wsl_id = tabs.active_remote.clone().unwrap();
    tabs.sync_ui(&mut local, &session);
    assert_eq!(local.tabs.len(), 3);
    assert_eq!(tabs.close_target(&wsl_id), Some(ssh_id.as_str()));
    assert_eq!(tabs.close_target(&ssh_id), Some(local_id.as_str()));
    assert!(tabs.close_tab(
        &local_id,
        &mut session,
        &mut local,
        &mut model,
        &mut history
    ));
    assert_eq!(tabs.active_remote.as_deref(), Some(ssh_id.as_str()));
    assert_eq!(session.id(), local_id);
    assert_eq!(local.input.text, "local draft");
    assert!(matches!(&history[0], ChatMessage::User { .. }));
    tabs.sync_ui(&mut local, &session);
    assert_eq!(local.tabs.len(), 2);
    assert!(!local.tabs.iter().any(|tab| tab.id == local_id));
    assert!(requests.try_recv().is_err());
    tabs.remove_remote(&wsl_id);
    assert_eq!(tabs.active_remote.as_deref(), Some(ssh_id.as_str()));
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
fn rejected_steering_restores_the_draft_without_overwriting_new_input() {
    let (mut tab, mut requests) = tab("local");
    tab.submit_with_receipt(
        Request::Steer {
            session: "same-session".into(),
            message: "more context".into(),
        },
        submissions::Draft::Composer("more context".into()),
    );
    requests
        .try_recv()
        .unwrap()
        .reply
        .unwrap()
        .send(Ok(Response::Error("session is idle".into())))
        .unwrap();
    assert!(tab.poll_submissions());
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
    assert_eq!(tab.state.input.text, "new draft");
    assert!(
        tab.state
            .info_dialog
            .as_ref()
            .unwrap()
            .content
            .contains("previous")
    );
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
    assert!(tab.state.btw.running());
    tab.poll_questions();
    let call = requests.try_recv().unwrap();
    assert!(matches!(
        call.request,
        Request::ReadQuestions { after: Some(1), .. }
    ));
    call.reply
        .unwrap()
        .send(Ok(Response::Questions(orbit_protocol::Questions {
            revision: 2,
            selected: None,
            running: false,
            records: Some(vec![orbit_protocol::SideQuestion {
                thread_id: "one".into(),
                model: "mock".into(),
                question: "why?".into(),
                answer: "done".into(),
                status: "completed".into(),
                error: None,
            }]),
        })))
        .unwrap();
    tab.poll_questions();
    assert!(!tab.state.btw.running());
    assert!(!tab.state.btw.visible());
    assert_eq!(tab.state.input.text, "main draft");
}
