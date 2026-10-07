use super::*;
use crossterm::event::{KeyEvent, MouseEvent};
use orbit_protocol::user_input::{Form, OptionItem, Question};
use ratatui::{Terminal, backend::TestBackend};
fn record() -> Record {
    Record {
        id: "request".into(),
        call_id: "call".into(),
        source: None,
        status: Status::Pending,
        answers: vec![],
        form: Form {
            questions: vec![
                Question {
                    id: "choice".into(),
                    title: "方式".into(),
                    prompt: "使用哪种方式？".into(),
                    kind: Kind::Single,
                    options: vec![
                        OptionItem {
                            id: "a".into(),
                            label: "甲".into(),
                            description: "说明".into(),
                        },
                        OptionItem {
                            id: "b".into(),
                            label: "乙".into(),
                            description: String::new(),
                        },
                    ],
                    allow_custom: true,
                    required: true,
                },
                Question {
                    id: "text".into(),
                    title: "要求".into(),
                    prompt: "填写要求".into(),
                    kind: Kind::Text,
                    options: vec![],
                    allow_custom: true,
                    required: true,
                },
            ],
        },
    }
}
fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}
fn state() -> State {
    let mut s = State::default();
    s.sync(vec![record()]);
    s.open_next();
    s
}
#[test]
fn user_input_requires_explicit_choice_and_keeps_draft_when_dismissed() {
    let mut s = state();
    assert!(s.answers("request")[0].selected.is_empty());
    s.activate(Focus::Option(2));
    s.handle(&Event::Paste("自定义内容".into()));
    s.handle(&key(KeyCode::Esc));
    assert!(!s.visible());
    s.open_next();
    assert!(!s.visible());
    s.open("request");
    assert_eq!(s.answers("request")[0].text, "自定义内容");
    s.activate(Focus::Option(0));
    assert!(s.answers("request")[0].text.is_empty());
    assert_eq!(s.answers("request")[0].selected, ["a"]);
}
#[test]
fn user_input_submit_validates_and_multi_terminal_result_replaces_local_draft() {
    let mut s = state();
    s.activate(Focus::Tab(2));
    assert!(matches!(s.activate(Focus::Next), Action::None));
    s.activate(Focus::Option(0));
    s.activate(Focus::Next);
    if let Some(Dialog::Form(v)) = &mut s.dialog {
        v.focus = Focus::Text;
    }
    s.handle(&Event::Paste("说明".into()));
    s.activate(Focus::Next);
    assert!(matches!(
        s.activate(Focus::Next),
        Action::Answer {
            answers: Some(_),
            ..
        }
    ));
    assert!(s.sync(vec![]));
    let mut r = record();
    r.status = Status::Answered;
    r.answers = vec![
        Answer {
            question_id: "choice".into(),
            selected: vec!["b".into()],
            ..Default::default()
        },
        Answer {
            question_id: "text".into(),
            text: "另一终端的答案".into(),
            ..Default::default()
        },
    ];
    s.receive(History {
        before: None,
        records: vec![r],
        next: None,
    });
    assert_eq!(s.answers("request")[1].text, "另一终端的答案");
    assert!(matches!(s.activate(Focus::Decline), Action::None));
}
#[test]
fn user_input_mouse_hover_is_bold_but_does_not_select_and_click_selects() {
    let mut s = state();
    let mut t = Terminal::new(TestBackend::new(100, 35)).unwrap();
    t.draw(|f| {
        s.draw(f);
    })
    .unwrap();
    let (rect, _) = s.hits.iter().find(|(_, f)| *f == Focus::Option(1)).unwrap();
    let mut mouse = MouseEvent {
        kind: MouseEventKind::Moved,
        column: rect.x,
        row: rect.y,
        modifiers: KeyModifiers::NONE,
    };
    s.handle(&Event::Mouse(mouse));
    assert!(s.answers("request")[0].selected.is_empty());
    t.draw(|f| {
        s.draw(f);
    })
    .unwrap();
    let cell = &t.backend().buffer()[(mouse.column, mouse.row)];
    assert_eq!(cell.bg, THEME.accent);
    assert!(cell.modifier.contains(Modifier::BOLD));
    mouse.kind = MouseEventKind::Down(MouseButton::Left);
    s.handle(&Event::Mouse(mouse));
    assert_eq!(s.answers("request")[0].selected, ["b"]);
}
#[test]
fn user_input_small_terminal_and_long_unicode_wrap_without_panics() {
    let mut s = state();
    s.records[0].form.questions[0].prompt = "很长的问题和说明".repeat(100);
    for (width, height) in [(100, 35), (42, 18), (20, 10), (5, 3)] {
        let mut t = Terminal::new(TestBackend::new(width, height)).unwrap();
        t.draw(|f| {
            s.draw(f);
        })
        .unwrap();
        for (rect, _) in &s.hits {
            assert!(rect.right() <= width && rect.bottom() <= height);
        }
    }
}

#[test]
fn user_input_modal_owns_keyboard_and_cursor_and_history_opens_from_tool() {
    let mut ui = crate::tui::app::UiState::new("test");
    ui.input.set("保留主输入框草稿".into());
    ui.user_input.sync(vec![record()]);
    let mut terminal = Terminal::new(TestBackend::new(100, 35)).unwrap();
    terminal
        .draw(|f| crate::tui::render::draw(f, &mut ui))
        .unwrap();
    assert!(ui.dialog_open());
    assert!(ui.ime_cursor.is_none());
    ui.user_input.activate(Focus::Tab(1));
    if let Some(Dialog::Form(v)) = &mut ui.user_input.dialog {
        v.focus = Focus::Text;
    }
    terminal
        .draw(|f| crate::tui::render::draw(f, &mut ui))
        .unwrap();
    let cursor = ui.ime_cursor.unwrap();
    assert!(!ui.input_area.contains(cursor));
    ui.user_input.handle(&Event::Paste("回答内容".into()));
    assert_eq!(ui.input.text, "保留主输入框草稿");
    ui.user_input.handle(&key(KeyCode::Esc));
    let mut completed = record();
    completed.status = Status::Answered;
    completed.answers = vec![
        Answer {
            question_id: "choice".into(),
            selected: vec!["a".into()],
            ..Default::default()
        },
        Answer {
            question_id: "text".into(),
            text: "保存的回答".into(),
            ..Default::default()
        },
    ];
    assert!(
        ui.user_input
            .open_tool("call", &serde_json::to_string(&completed).unwrap())
    );
    assert_eq!(ui.user_input.answers("request")[1].text, "保存的回答");
}

#[test]
fn user_input_keyboard_scrolls_focused_option_into_view_and_queue_is_preserved() {
    let mut s = state();
    let mut second = record();
    second.id = "second".into();
    second.call_id = "call-2".into();
    s.sync(vec![record(), second]);
    s.records[0].form.questions[0].prompt = "很长的问题说明，确保选项在下方。".repeat(40);
    let mut t = Terminal::new(TestBackend::new(60, 20)).unwrap();
    t.draw(|f| {
        s.draw(f);
    })
    .unwrap();
    assert!(s.hits.iter().any(|(_, focus)| *focus == Focus::Option(0)));
    s.handle(&key(KeyCode::Esc));
    s.open_next();
    assert!(matches!(&s.dialog,Some(Dialog::Form(v)) if v.id=="second"));
}

#[test]
fn user_input_multiple_choice_keeps_custom_text_with_selections() {
    let mut s = state();
    s.records[0].form.questions[0].kind = Kind::Multiple;
    s.activate(Focus::Option(0));
    s.activate(Focus::Option(1));
    s.activate(Focus::Option(2));
    s.handle(&Event::Paste("另一种方式".into()));
    let answers = s.answers("request");
    assert_eq!(answers[0].selected, ["a", "b"]);
    assert_eq!(answers[0].text, "另一种方式");
    s.activate(Focus::Option(0));
    assert_eq!(s.answers("request")[0].selected, ["b"]);
}

#[test]
fn user_input_history_paging_preserves_latest_order_and_cursor() {
    let mut s = state();
    let mut newest = record();
    newest.id = "newest".into();
    newest.status = Status::Declined;
    let mut older = record();
    older.id = "older".into();
    older.status = Status::Declined;
    s.history();
    s.receive(History {
        before: None,
        records: vec![newest.clone(), record()],
        next: Some(2),
    });
    assert_eq!(s.records[0].id, "newest");
    s.receive(History {
        before: Some(2),
        records: vec![older],
        next: None,
    });
    assert_eq!(
        s.records.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
        ["newest", "request", "older"]
    );
}

#[test]
fn user_input_history_uses_request_identity_when_tool_call_ids_repeat() {
    let mut s = state();
    let mut newer = record();
    newer.id = "newer-request".into();
    newer.status = Status::Declined;
    s.upsert(newer.clone());
    assert!(s.open_tool("call", &serde_json::to_string(&newer).unwrap()));
    assert!(matches!(&s.dialog,Some(Dialog::Form(v)) if v.id=="newer-request"));
}
