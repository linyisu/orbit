use super::*;
use workspace::transport::Target;

#[tokio::test]
async fn every_target_reconnects_original_session_without_a_wizard_or_resubmission() {
    for target in [
        Target::Local,
        Target::Ssh {
            host: "dev".into(),
            options: Default::default(),
        },
        Target::Wsl {
            distribution: "Ubuntu".into(),
            user: None,
        },
    ] {
        let mut tab = connected_tab(target);
        tab.state.transcript.clear();
        tab.starting = Some("unconfirmed input".into());
        tab.starting_attachments
            .push(orbit_model::types::Attachment::Image {
                lease: None,
                name: "image.png".into(),
                mime_type: "image/png".into(),
                path: "/tmp/image.png".into(),
            });
        tab.disconnected("server replaced".into());
        assert_eq!(tab.state.input.text, "unconfirmed input");
        assert_eq!(tab.state.attachments.len(), 1);
        assert!(tab.starting.is_none());
        assert!(tab.reconnect_at.is_some());
        assert!(!tab.poll_reconnection(), "backoff must avoid tight retries");
        tab.reconnect_at = Some(std::time::Instant::now());
        assert!(tab.poll_reconnection());
        assert!(tab.connecting.is_some());
        assert_eq!(tab.id.as_deref(), Some("same-session"));
        assert!(tab.state.workspace_wizard.is_none());
        assert!(
            tab.commands.is_none(),
            "never automatically resubmit a command"
        );
        assert_eq!(tab.state.input.text, "unconfirmed input");
        assert_eq!(tab.state.attachments.len(), 1);
        tab.cancel_connect();
        assert!(tab.reconnect_at.is_none());
        assert!(tab.connecting.is_none());
        assert!(!tab.poll_reconnection());
    }
}

#[test]
fn failed_retries_back_off_without_flooding_history_and_initial_setup_stays_manual() {
    let mut tab = connected_tab(Target::Local);
    tab.disconnected("connection closed".into());
    let history_len = tab.state.transcript.len();
    for _ in 0..20 {
        tab.disconnected("still unavailable".into());
    }
    assert_eq!(tab.state.transcript.len(), history_len);
    let delay = tab
        .reconnect_at
        .unwrap()
        .duration_since(std::time::Instant::now());
    assert!(delay > Duration::from_secs(7) && delay <= Duration::from_secs(8));
    let mut initial = Tab::empty(
        "SSH dev".into(),
        "/project".into(),
        media::Context::default(),
    );
    initial.disconnected("bad configuration".into());
    assert!(initial.reconnect_at.is_none());
}

#[tokio::test]
async fn escape_cancels_a_scheduled_retry_and_manual_reconnect_still_works() {
    let mut tab = connected_tab(Target::Local);
    tab.disconnected("server stopped".into());
    tab.handle(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
        .unwrap();
    assert!(tab.reconnect_at.is_none());
    assert!(!tab.poll_reconnection());
    tab.reconnect();
    assert!(tab.connecting.is_some());
    assert!(tab.state.workspace_wizard.is_none());
}

#[test]
fn disconnected_draft_is_discarded_but_an_unconfirmed_first_message_keeps_its_identity() {
    let mut draft = connected_tab(Target::Local);
    draft.persisted = false;
    draft.disconnected("connection closed".into());
    assert!(draft.id.is_none());
    assert!(draft.reconnect_at.is_none());

    let (mut configuring, _requests) = tab("本地");
    configuring.submit("/permissions yolo".into()).unwrap();
    configuring.disconnected("configuration acknowledgement lost".into());
    assert!(configuring.id.is_none());

    let mut sending = connected_tab(Target::Local);
    sending.persisted = false;
    sending.starting = Some("first message".into());
    sending.disconnected("acknowledgement lost".into());
    assert_eq!(sending.id.as_deref(), Some("same-session"));
    assert!(sending.reconnect_at.is_some());
}
