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
    ssh.submit("/mode ask".into()).unwrap();
    ssh.submit("first target".into()).unwrap();
    wsl.submit("second target".into()).unwrap();
    assert!(
        matches!(ssh_requests.try_recv().unwrap().request, Request::Start { prompt, mode: Mode::Ask, model: Some(model), .. } if prompt == "first target" && model == "ssh-model")
    );
    assert!(
        matches!(wsl_requests.try_recv().unwrap().request, Request::Start { prompt, mode: Mode::Auto, model: Some(model), .. } if prompt == "second target" && model == "wsl-model")
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
fn workspace_completion_does_not_offer_local_filesystem_commands() {
    let (mut tab, _) = tab("SSH dev");
    tab.state.input.set("/".into());
    let menu = tab.state.menu();
    for command in ["/ssh", "/wsl", "/local", "/new", "/reconnect"] {
        assert!(
            menu.iter().any(|candidate| candidate.name == command),
            "{command}"
        );
    }
    assert!(
        !menu
            .iter()
            .any(|candidate| matches!(candidate.name.as_str(), "/cd" | "/connect" | "/undo"))
    );
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
