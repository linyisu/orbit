use orbit_tools::approval::WritePreview;
use ratatui::{Terminal, backend::TestBackend};
use unicode_width::UnicodeWidthStr;

use super::*;

#[test]
fn approval_keeps_old_and_new_syntax_states_independent() {
    let mut state = UiState::new("mock");
    state.approval = Some(orbit_tools::approval::ApprovalPreview::Write(
        WritePreview {
            path: "src/Orbit Code.rs".into(),
            before: Some("/* Orbit block\nfn orbit() {}\n*/\n".into()),
            after: "// Orbit update\nfn orbit() {}\n".into(),
        },
    ));
    let mut terminal = Terminal::new(TestBackend::new(100, 25)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let buffer = terminal.backend().buffer();
    let point = (0..25)
        .find_map(|y| {
            let row: String = (0..100).map(|x| buffer[(x, y)].symbol()).collect();
            row.find("fn orbit").map(|x| (row[..x].width() as u16, y))
        })
        .expect("unchanged source line is visible");

    assert_eq!(buffer[point].fg, ratatui::style::Color::Rgb(198, 120, 221));
}

#[test]
fn full_preview_scrolls_to_end_and_escapes_terminal_controls() {
    let mut state = UiState::new("mock");
    state.approval = Some(orbit_tools::approval::ApprovalPreview::Write(
        WritePreview {
            path: "file".into(),
            before: Some("old".into()),
            after: format!(
                "{}END\\marker\u{1b}",
                "long line that wraps across the screen\n".repeat(80)
            ),
        },
    ));
    let mut terminal = Terminal::new(TestBackend::new(60, 20)).unwrap();
    terminal.draw(|f| draw(f, &mut state)).unwrap();

    assert!(state.approval_shown);

    handle(
        &mut state,
        Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::Tab,
            KeyModifiers::NONE,
        )),
    );
    terminal.draw(|f| draw(f, &mut state)).unwrap();

    assert_eq!(state.approval_choice, 0);
    assert!(state.approval_max_scroll > 0);

    state.approval_scroll = state.approval_max_scroll;
    terminal.draw(|f| draw(f, &mut state)).unwrap();
    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();

    assert!(text.contains("END\\marker\\u{1b}"), "{text}");
    assert!(
        text.replace(' ', "").contains("允许") && text.replace(' ', "").contains("拒绝"),
        "{text}"
    );
}

#[test]
fn command_approval_card_renders_rounded_frame_and_buttons() {
    use crossterm::event::{MouseButton, MouseEvent};
    use orbit_tools::approval::{CommandPreview, CommandWorkspaceAccess};

    let mut state = UiState::new("mock");
    state.approval = Some(orbit_tools::approval::ApprovalPreview::Command(
        CommandPreview {
            sandbox: true,
            native_workspace_scoped: false,
            command: "mkdir -p demo".into(),
            workspace_access: CommandWorkspaceAccess::ReadWrite,
            network_allowed: true,
            cwd: Some("/home/user/repo".into()),
            timeout_seconds: 30,
            external_capabilities: Vec::new(),
            external_write: false,
            git: None,
        },
    ));

    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
    terminal.draw(|f| draw(f, &mut state)).unwrap();

    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();

    let flat = text.replace(' ', "");
    assert!(text.contains("mkdir -p demo"), "{text}");
    assert!(flat.contains("权限确认"), "{text}");
    assert!(flat.contains("[沙箱·读写]"), "{text}");
    assert!(flat.contains("[联网]"), "{text}");
    assert!(flat.contains("cwd:/home/user/repo"), "{text}");
    assert!(!text.contains("Tab 切换"), "{text}");
    assert!(state.approval_shown);
    assert_eq!(state.approval_options.len(), 3);

    let (opt0_rect, _) = state.approval_options[0];
    let (opt1_rect, _) = state.approval_options[1];
    let (opt2_rect, _) = state.approval_options[2];

    let click = |x, y| {
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        })
    };

    assert_eq!(
        handle(&mut state, click(opt0_rect.x + 1, opt0_rect.y)),
        Action::Approve
    );
    assert_eq!(
        handle(&mut state, click(opt1_rect.x + 1, opt1_rect.y)),
        Action::AlwaysAllow
    );
    assert_eq!(
        handle(&mut state, click(opt2_rect.x + 1, opt2_rect.y)),
        Action::Deny
    );
}

#[test]
fn git_dangerous_card_shows_risk_and_badges() {
    use orbit_tools::approval::{CommandPreview, CommandWorkspaceAccess};

    let mut state = UiState::new("mock");
    state.approval = Some(orbit_tools::approval::ApprovalPreview::Command(
        CommandPreview {
            sandbox: true,
            native_workspace_scoped: false,
            command: "git reset --hard HEAD~1".into(),
            workspace_access: CommandWorkspaceAccess::ReadWrite,
            network_allowed: false,
            cwd: None,
            timeout_seconds: 30,
            external_capabilities: Vec::new(),
            external_write: false,
            git: orbit_runtime::git::GitCommandAnalysis::parse("git reset --hard HEAD~1"),
        },
    ));

    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
    terminal.draw(|f| draw(f, &mut state)).unwrap();

    let text: String = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect();
    let flat = text.replace(' ', "");

    assert!(flat.contains("权限确认"), "{text}");
    assert!(flat.contains("高危操作(reset)"), "{text}");
    assert!(text.contains("git reset --hard HEAD~1"), "{text}");
    assert!(flat.contains("[沙箱·读写]"), "{text}");
    assert!(!flat.contains("[联网]"), "{text}");
    assert!(!text.contains("Tab 切换"), "{text}");
}
