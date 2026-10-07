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

#[test]
fn user_input_tab_changes_questions_and_enter_confirms_single_choice() {
    let mut s = state();
    s.handle(&key(KeyCode::Down));
    assert!(matches!(&s.dialog, Some(Dialog::Form(v)) if v.focus == Focus::Option(1)));
    s.handle(&key(KeyCode::Enter));
    assert_eq!(s.answers("request")[0].selected, ["b"]);
    assert!(matches!(&s.dialog, Some(Dialog::Form(v)) if v.page == 1 && v.focus == Focus::Text));
    s.handle(&Event::Paste("要求".into()));
    s.handle(&key(KeyCode::BackTab));
    assert!(matches!(&s.dialog, Some(Dialog::Form(v)) if v.page == 0));
    s.handle(&key(KeyCode::Tab));
    assert_eq!(s.answers("request")[1].text, "要求");
    s.handle(&key(KeyCode::Tab));
    assert!(matches!(&s.dialog, Some(Dialog::Form(v)) if v.page == 2 && !v.sending));
    assert!(matches!(
        s.handle(&key(KeyCode::Enter)),
        Action::Answer { .. }
    ));
}

#[test]
fn user_input_multiple_choices_use_shared_icons_and_explicit_space_selection() {
    let mut s = state();
    s.records[0].form.questions[0].kind = Kind::Multiple;
    s.handle(&key(KeyCode::Char(' ')));
    s.handle(&key(KeyCode::Down));
    let mut terminal = Terminal::new(TestBackend::new(100, 35)).unwrap();
    terminal
        .draw(|frame| {
            s.draw(frame);
        })
        .unwrap();
    let screen = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(screen.contains(crate::tui::icons::CHECKBOX_MARKED_OUTLINE));
    assert!(screen.contains(crate::tui::icons::CHECKBOX_BLANK_OUTLINE));
    assert!(!screen.contains('☑') && !screen.contains('○'));
    let (row, _) = s
        .hits
        .iter()
        .find(|(_, focus)| *focus == Focus::Option(1))
        .unwrap();
    assert_eq!(
        terminal.backend().buffer()[(row.right() - 1, row.y)].bg,
        THEME.accent
    );
    s.handle(&key(KeyCode::Enter));
    assert_eq!(s.answers("request")[0].selected, ["a"]);
    assert!(matches!(&s.dialog, Some(Dialog::Form(v)) if v.page == 1));
}

#[test]
fn user_input_completed_view_discards_editor_hits_and_scrolls_without_focus_snap() {
    let mut s = state();
    s.activate(Focus::Tab(1));
    let mut terminal = Terminal::new(TestBackend::new(70, 25)).unwrap();
    terminal
        .draw(|frame| {
            s.draw(frame);
        })
        .unwrap();
    let old_editor = s.editor_rows[0].0;
    s.handle(&key(KeyCode::Esc));
    let mut r = record();
    r.status = Status::Answered;
    r.answers.push(Answer {
        question_id: "text".into(),
        text: "很多行的答案\n".repeat(40),
        ..Default::default()
    });
    assert!(s.open_tool("call", &serde_json::to_string(&r).unwrap()));
    s.handle(&Event::Mouse(MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: old_editor.x,
        row: old_editor.y,
        modifiers: KeyModifiers::NONE,
    }));
    terminal
        .draw(|frame| {
            assert!(s.draw(frame).is_none());
        })
        .unwrap();
    s.handle(&key(KeyCode::PageDown));
    terminal
        .draw(|frame| {
            s.draw(frame);
        })
        .unwrap();
    assert!(matches!(&s.dialog, Some(Dialog::Form(v)) if v.offset > 0));
    s.handle(&key(KeyCode::BackTab));
    terminal
        .draw(|frame| {
            assert!(s.draw(frame).is_none());
        })
        .unwrap();
    assert!(s.editor_rows.is_empty());
}

#[test]
fn user_input_enter_on_custom_multiple_choice_opens_editor_without_losing_selections() {
    let mut s = state();
    s.records[0].form.questions[0].kind = Kind::Multiple;
    s.handle(&key(KeyCode::Char(' ')));
    s.handle(&key(KeyCode::Down));
    s.handle(&key(KeyCode::Down));
    s.handle(&key(KeyCode::Enter));
    assert!(matches!(&s.dialog, Some(Dialog::Form(v)) if v.page == 0 && v.focus == Focus::Text));
    s.handle(&Event::Paste("另一个选项".into()));
    let answer = &s.answers("request")[0];
    assert_eq!(answer.selected, ["a"]);
    assert_eq!(answer.text, "另一个选项");
}

fn paint(s: &mut State, terminal: &mut Terminal<TestBackend>) -> Option<Position> {
    let mut cursor = None;
    terminal.draw(|frame| cursor = s.draw(frame)).unwrap();
    cursor
}
fn hit(s: &State, focus: Focus) -> Rect {
    s.hits
        .iter()
        .find(|(_, target)| *target == focus)
        .unwrap()
        .0
}
fn pointer(kind: MouseEventKind, rect: Rect) -> Event {
    Event::Mouse(MouseEvent {
        kind,
        column: rect.x,
        row: rect.y,
        modifiers: KeyModifiers::NONE,
    })
}
fn screen(terminal: &Terminal<TestBackend>) -> String {
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn user_input_mouse_click_advances_single_choices_but_keeps_multi_and_custom_open() {
    for kind in [Kind::Single, Kind::Multiple] {
        let mut s = state();
        s.records[0].form.questions[0].kind = kind;
        let mut terminal = Terminal::new(TestBackend::new(100, 35)).unwrap();
        paint(&mut s, &mut terminal);
        let option = hit(&s, Focus::Option(1));
        s.handle(&pointer(MouseEventKind::Down(MouseButton::Left), option));
        paint(&mut s, &mut terminal);
        s.handle(&pointer(MouseEventKind::Up(MouseButton::Left), option));
        assert_eq!(s.answers("request")[0].selected, ["b"]);
        assert!(
            matches!(&s.dialog, Some(Dialog::Form(v)) if v.page == usize::from(kind == Kind::Single))
        );
        if kind == Kind::Single {
            assert!(paint(&mut s, &mut terminal).is_some());
            s.handle(&Event::Paste("第二题回答".into()));
            assert_eq!(s.answers("request")[1].text, "第二题回答");
        }
    }
}

#[test]
fn user_input_custom_answer_edits_the_choice_row_without_an_extra_note_field() {
    let mut s = state();
    let mut terminal = Terminal::new(TestBackend::new(100, 35)).unwrap();
    paint(&mut s, &mut terminal);
    let custom = hit(&s, Focus::Option(2));
    s.handle(&pointer(MouseEventKind::Down(MouseButton::Left), custom));
    let cursor = paint(&mut s, &mut terminal).unwrap();
    assert_eq!(cursor.y, custom.y);
    assert_eq!(s.editor_rows.len(), 1);
    assert!(!screen(&terminal).contains("补充说明"));
    assert!(!screen(&terminal).contains("填写答案"));
    s.handle(&pointer(MouseEventKind::Up(MouseButton::Left), custom));
    s.handle(&Event::Paste("我的答案".into()));
    let cursor = paint(&mut s, &mut terminal).unwrap();
    assert_eq!(cursor.y, custom.y);
    assert_eq!(s.editor_rows.len(), 1);
    let editor = s.editor_rows[0].0;
    s.handle(&pointer(MouseEventKind::Down(MouseButton::Left), editor));
    s.handle(&key(KeyCode::Char('新')));
    assert_eq!(s.answers("request")[0].text, "新我的答案");
    assert!(s.answers("request")[0].note.is_empty());
    assert!(matches!(&s.dialog, Some(Dialog::Form(v)) if v.page == 0 && v.focus == Focus::Text));
}

#[test]
fn user_input_next_click_release_and_hover_preserve_the_new_text_editor_focus() {
    let mut s = state();
    let mut terminal = Terminal::new(TestBackend::new(100, 35)).unwrap();
    paint(&mut s, &mut terminal);
    let next = hit(&s, Focus::Next);
    s.handle(&pointer(MouseEventKind::Down(MouseButton::Left), next));
    assert!(paint(&mut s, &mut terminal).is_some());
    assert_eq!(
        hit(&s, Focus::Next),
        next,
        "navigation buttons stay in fixed slots"
    );
    let previous = hit(&s, Focus::Previous);
    for event in [
        pointer(MouseEventKind::Drag(MouseButton::Left), previous),
        pointer(MouseEventKind::Up(MouseButton::Left), previous),
        pointer(MouseEventKind::Moved, next),
        pointer(MouseEventKind::Moved, previous),
    ] {
        s.handle(&event);
        assert!(paint(&mut s, &mut terminal).is_some());
        assert!(
            matches!(&s.dialog, Some(Dialog::Form(v)) if v.page == 1 && v.focus == Focus::Text)
        );
    }
    s.handle(&Event::Paste("光标仍在回答中".into()));
    assert_eq!(s.answers("request")[1].text, "光标仍在回答中");
}

#[test]
fn user_input_hover_moves_without_scrolling_relayout_or_highlighting_other_controls() {
    let mut s = state();
    let mut terminal = Terminal::new(TestBackend::new(60, 24)).unwrap();
    paint(&mut s, &mut terminal);
    let geometry = s.hits.clone();
    let first = hit(&s, Focus::Option(0));
    let last = hit(&s, Focus::Option(2));
    let next = hit(&s, Focus::Next);
    for row in first.y..=last.y {
        let rect = Rect::new(first.x, row, 1, 1);
        s.handle(&pointer(MouseEventKind::Moved, rect));
        paint(&mut s, &mut terminal);
        assert_eq!(s.hits, geometry);
        assert!(matches!(&s.dialog, Some(Dialog::Form(v)) if v.focus == Focus::Option(0)));
        assert_ne!(
            terminal.backend().buffer()[(next.x, next.y)].bg,
            THEME.accent
        );
        let hovered = geometry
            .iter()
            .find(|(r, _)| r.contains(Position::new(first.x, row)))
            .map(|(_, focus)| *focus);
        assert_eq!(s.hovered, hovered);
        for (target, focus) in &geometry {
            if matches!(focus, Focus::Option(_)) {
                assert_eq!(
                    terminal.backend().buffer()[(target.x, target.y)].bg == THEME.accent,
                    hovered == Some(*focus)
                );
            }
        }
    }
}

#[test]
fn user_input_holding_question_tab_does_not_highlight_or_activate_next() {
    let mut s = state();
    let mut terminal = Terminal::new(TestBackend::new(100, 35)).unwrap();
    paint(&mut s, &mut terminal);
    let tab = hit(&s, Focus::Tab(0));
    s.handle(&pointer(MouseEventKind::Down(MouseButton::Left), tab));
    paint(&mut s, &mut terminal);
    let next = hit(&s, Focus::Next);
    for kind in [
        MouseEventKind::Drag(MouseButton::Left),
        MouseEventKind::Up(MouseButton::Left),
    ] {
        s.handle(&pointer(kind, next));
        paint(&mut s, &mut terminal);
        assert!(matches!(&s.dialog, Some(Dialog::Form(v)) if v.page == 0));
        assert_ne!(
            terminal.backend().buffer()[(next.x, next.y)].bg,
            THEME.accent
        );
    }
}

#[test]
fn user_input_submission_closes_on_receipt_and_failures_preserve_the_answers() {
    let mut s = state();
    s.activate(Focus::Option(0));
    s.activate(Focus::Next);
    s.handle(&Event::Paste("答案".into()));
    s.activate(Focus::Next);
    assert!(matches!(s.activate(Focus::Next), Action::Answer { .. }));
    s.submission_error("request", "重试".into());
    assert!(s.visible());
    assert_eq!(s.answers("request")[1].text, "答案");
    assert!(matches!(s.activate(Focus::Next), Action::Answer { .. }));
    s.receive(History {
        before: None,
        records: vec![record()],
        next: None,
    });
    assert!(matches!(&s.dialog, Some(Dialog::Form(v)) if v.sending));
    let mut completed = record();
    completed.status = Status::Answered;
    completed.answers = s.answers("request");
    s.receive(History {
        before: None,
        records: vec![completed.clone()],
        next: None,
    });
    assert!(!s.visible());
    s.open_next();
    assert!(!s.visible());
    assert!(s.hits.is_empty() && s.editor_rows.is_empty());
    assert!(s.open_tool("call", &serde_json::to_string(&completed).unwrap()));
    let mut terminal = Terminal::new(TestBackend::new(100, 35)).unwrap();
    assert!(paint(&mut s, &mut terminal).is_none());
    assert!(!screen(&terminal).contains("查看完毕"));
    assert!(!s.hits.iter().any(|(_, focus)| *focus == Focus::Next));
    s.activate(Focus::Later);
    assert!(!s.visible());
}

#[test]
fn user_input_choice_padding_survives_hover_wrapping_and_custom_editing() {
    for width in [42, 100] {
        let mut s = state();
        s.records[0].form.questions[0].kind = Kind::Multiple;
        s.records[0].form.questions[0].options[0].description =
            "长说明文字需要换行显示，并保持左右的留白。".repeat(3);
        let mut terminal = Terminal::new(TestBackend::new(width, 35)).unwrap();
        paint(&mut s, &mut terminal);
        let option = hit(&s, Focus::Option(0));
        s.handle(&pointer(MouseEventKind::Moved, option));
        paint(&mut s, &mut terminal);
        for (row, _) in s
            .hits
            .iter()
            .filter(|(_, focus)| *focus == Focus::Option(0))
        {
            for x in [row.x, row.x + 1, row.right() - 2, row.right() - 1] {
                let cell = &terminal.backend().buffer()[(x, row.y)];
                assert_eq!(cell.symbol(), " ", "padding at width {width}");
                assert_eq!(cell.bg, THEME.accent);
            }
        }
        // Selecting the custom choice preserves its inset on the same row.
        s.records[0].form.questions[0].options[0]
            .description
            .clear();
        paint(&mut s, &mut terminal);
        let custom = hit(&s, Focus::Option(2));
        s.handle(&pointer(MouseEventKind::Down(MouseButton::Left), custom));
        let cursor = paint(&mut s, &mut terminal).unwrap();
        assert_eq!(cursor.y, custom.y);
        assert_eq!(cursor.x, custom.x + 4);
        s.handle(&Event::Paste("我的答案".into()));
        paint(&mut s, &mut terminal);
        let editor = s.editor_rows[0].0;
        s.handle(&pointer(MouseEventKind::Down(MouseButton::Left), editor));
        s.handle(&key(KeyCode::Char('新')));
        assert_eq!(s.answers("request")[0].text, "新我的答案");
    }
}

#[test]
fn user_input_summary_preserves_prompt_numbering() {
    for status in [Status::Pending, Status::Answered] {
        let mut s = state();
        let mut r = record();
        r.status = status;
        r.form.questions[0].prompt = "1. Which editor?".into();
        r.form.questions[1].prompt = "32 bit or 64 bit?".into();
        s.upsert(r);
        s.open("request");
        s.activate(Focus::Tab(2));
        let mut terminal = Terminal::new(TestBackend::new(100, 35)).unwrap();
        paint(&mut s, &mut terminal);
        for (index, expected) in [(0, "1. Which editor?"), (1, "32 bit or 64 bit?")] {
            let row = hit(&s, Focus::Review(index));
            let text: String = (row.x..row.right())
                .map(|x| terminal.backend().buffer()[(x, row.y)].symbol())
                .collect();
            assert_eq!(text.trim(), expected);
        }
    }
}

#[test]
fn user_input_summary_hover_highlights_only_the_question_heading() {
    for status in [Status::Pending, Status::Answered] {
        let mut s = state();
        let mut r = record();
        r.status = status;
        r.form.questions[1].prompt = "较长的题目文字需要正确换行并整体高亮。".repeat(3);
        s.upsert(r);
        s.open("request");
        s.activate(Focus::Tab(2));
        let mut terminal = Terminal::new(TestBackend::new(100, 35)).unwrap();
        paint(&mut s, &mut terminal);
        let tabs: Vec<_> = s
            .hits
            .iter()
            .filter(|(_, focus)| matches!(focus, Focus::Tab(_)))
            .copied()
            .collect();
        let review_rows: Vec<_> = s
            .hits
            .iter()
            .filter(|(_, focus)| matches!(focus, Focus::Review(_)))
            .map(|(row, focus)| {
                (
                    *row,
                    *focus,
                    terminal.backend().buffer()[(row.x, row.y)].fg == THEME.accent,
                )
            })
            .collect();
        for (row, focus, _) in &review_rows {
            // Both the heading and its answer navigate to this question, but
            // only the heading receives the hover highlight.
            s.handle(&pointer(MouseEventKind::Moved, *row));
            paint(&mut s, &mut terminal);
            assert_eq!(s.hovered, Some(*focus));
            for (target, target_focus, heading) in &review_rows {
                let cell = &terminal.backend().buffer()[(target.x, target.y)];
                let highlighted = target_focus == focus && *heading;
                assert_eq!(cell.bg == THEME.accent, highlighted);
                if highlighted {
                    assert!(cell.modifier.contains(Modifier::BOLD));
                }
            }
            for (tab, focus) in &tabs {
                assert_eq!(
                    terminal.backend().buffer()[(tab.x, tab.y)].bg == THEME.accent,
                    *focus == Focus::Tab(2),
                    "body hover must not highlight a header tab"
                );
            }
        }
        let review = hit(&s, Focus::Review(1));
        s.handle(&pointer(MouseEventKind::Down(MouseButton::Left), review));
        assert!(matches!(&s.dialog, Some(Dialog::Form(v)) if v.page == 1));
        assert_eq!(
            paint(&mut s, &mut terminal).is_some(),
            status == Status::Pending
        );
    }
}
