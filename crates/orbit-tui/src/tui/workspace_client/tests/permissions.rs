use super::*;
use crossterm::event::KeyEventKind;
use ratatui::{Terminal, backend::TestBackend};

fn permission_profile(mode: Mode, revision: u64) -> SessionInfo {
    serde_json::from_value(serde_json::json!({
        "id":"same-session", "title":"", "workspace":"/project", "model":"mock", "models":[],
        "mode":mode, "mode_revision":revision,
        "supports_vision":false, "reasoning_effort":null, "reasoning_efforts":[],
        "context_tokens":1000, "input_context_tokens":900, "max_output_tokens":100
    }))
    .unwrap()
}

#[test]
fn permissions_follow_confirmation_and_ignore_stale_snapshots() {
    let (mut tab, mut rx) = tab("本地");
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    tab.submit("/permissions yolo".into()).unwrap();
    let call = rx.try_recv().unwrap();
    assert!(matches!(
        call.request,
        Request::SetMode {
            mode: Mode::Yolo,
            ..
        }
    ));
    assert_eq!(tab.state.permissions.mode, Mode::Auto);
    terminal
        .draw(|f| crate::tui::render::draw(f, &mut tab.state))
        .unwrap();
    let screen: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    assert!(screen.contains("build  auto → yolo…"), "{screen}");

    // Another terminal changes the service before this terminal receives its earlier receipt.
    tab.profile(&permission_profile(Mode::Ask, 2));
    call.reply
        .unwrap()
        .send(Ok(Response::Profile(Box::new(permission_profile(
            Mode::Yolo,
            1,
        )))))
        .unwrap();
    assert!(tab.poll_submissions());
    tab.profile(&permission_profile(Mode::Auto, 0));
    assert_eq!(tab.state.permissions.mode, Mode::Ask);
    assert_eq!(tab.state.permissions.pending, None);

    // A failed write never becomes the confirmed mode, even if the request was queued.
    tab.submit("/permissions auto".into()).unwrap();
    rx.try_recv()
        .unwrap()
        .reply
        .unwrap()
        .send(Ok(Response::Error("cannot save permissions".into())))
        .unwrap();
    tab.poll_submissions();
    assert_eq!(tab.state.permissions.mode, Mode::Ask);
    assert_eq!(tab.state.permissions.pending, None);
    assert!(
        tab.state
            .info_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.content.contains("cannot save"))
    );

    tab.submit("/permissions yolo".into()).unwrap();
    let late = rx.try_recv().unwrap().reply.unwrap();
    tab.disconnect("connection lost".into(), false);
    assert!(
        late.send(Ok(Response::Profile(Box::new(permission_profile(
            Mode::Yolo,
            3
        )))))
        .is_err()
    );
    assert_eq!(tab.state.permissions.pending, None);
    assert_eq!(tab.state.permissions.mode, Mode::Ask);
    tab.submit("/permissions auto".into()).unwrap();
    assert_eq!(tab.state.permissions.pending, None);
    assert_eq!(tab.state.permissions.mode, Mode::Ask);
}

#[test]
fn shift_tab_cycles_pending_intent_without_changing_draft_or_work_mode() {
    for (code, modifiers) in [
        (KeyCode::BackTab, KeyModifiers::SHIFT),
        (KeyCode::BackTab, KeyModifiers::NONE),
        (KeyCode::Tab, KeyModifiers::SHIFT),
    ] {
        let (mut tab, mut rx) = tab("本地");
        tab.state.input.set("未发送草稿".into());
        let work_mode = tab.state.planning.mode();
        let mut replies = Vec::new();
        for (index, expected) in [Mode::Yolo, Mode::Ask, Mode::Auto].into_iter().enumerate() {
            tab.busy = index != 0;
            tab.handle(Event::Key(KeyEvent::new(code, modifiers)))
                .unwrap();
            let call = rx.try_recv().unwrap();
            assert!(matches!(call.request, Request::SetMode {mode, ..} if mode == expected));
            replies.push((call.reply.unwrap(), expected));
            assert_eq!(tab.state.permissions.mode, Mode::Auto);
            assert_eq!(tab.state.permissions.pending, Some(expected));
            assert_eq!(tab.state.input.text, "未发送草稿");
            assert_eq!(tab.state.planning.mode(), work_mode);
        }
        for (index, (reply, mode)) in replies.into_iter().enumerate() {
            reply
                .send(Ok(Response::Profile(Box::new(permission_profile(
                    mode,
                    index as u64 + 1,
                )))))
                .unwrap();
            tab.poll_submissions();
            assert_eq!(tab.state.permissions.mode, mode);
            assert_eq!(
                tab.state.permissions.pending,
                if index == 2 { None } else { Some(Mode::Auto) }
            );
        }
        for kind in [KeyEventKind::Release, KeyEventKind::Repeat] {
            tab.handle(Event::Key(KeyEvent::new_with_kind(code, modifiers, kind)))
                .unwrap();
            assert!(rx.try_recv().is_err());
            assert_eq!(tab.state.input.text, "未发送草稿");
        }
        tab.busy = false;
        tab.submit("/help".into()).unwrap();
        tab.handle(Event::Key(KeyEvent::new(code, modifiers)))
            .unwrap();
        assert!(rx.try_recv().is_err());
        assert_eq!(tab.state.permissions.mode, Mode::Auto);
    }
}
