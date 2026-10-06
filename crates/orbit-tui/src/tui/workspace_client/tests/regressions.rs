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
