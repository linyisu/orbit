use crossterm::event::KeyEvent;
use ratatui::{Terminal, backend::TestBackend};

use super::*;

#[test]
fn transcript_dialog_grows_on_wide_screens_and_keeps_half_screen_size() {
    for (full, expected) in [
        (Rect::new(0, 0, 105, 47), (96, 24)),
        (Rect::new(0, 0, 120, 50), (96, 24)),
        (Rect::new(0, 0, 150, 45), (120, 30)),
        (Rect::new(0, 0, 200, 50), (160, 40)),
        (Rect::new(0, 0, 200, 12), (160, 10)),
    ] {
        let area = transcript_area(full);
        assert_eq!((area.width, area.height), expected);
    }

    let widths = (100..=240)
        .map(|width| transcript_area(Rect::new(0, 0, width, 50)))
        .collect::<Vec<_>>();
    assert!(
        widths
            .windows(2)
            .all(|pair| { pair[1].width >= pair[0].width && pair[1].width - pair[0].width <= 1 })
    );
}

#[test]
fn transcript_dialog_preserves_centering_and_bounds_at_all_sizes() {
    for (width, height) in [(0, 0), (1, 1), (8, 3), (100, 30), (201, 51)] {
        let full = Rect::new(7, 11, width, height);
        let area = transcript_area(full);
        assert_eq!(area.intersection(full), area);
        let left = area.left() - full.left();
        let right = full.right() - area.right();
        let top = area.top() - full.top();
        let bottom = full.bottom() - area.bottom();
        assert!(left.abs_diff(right) <= 1);
        assert!(top.abs_diff(bottom) <= 1);
    }
}

#[test]
fn home_output_scrolls_in_a_temporary_dialog_without_changing_the_draft() {
    let mut state = UiState::new("orbit::model");
    state.input.set("Orbit 草稿".into());
    state.show_output(
        "Orbit 状态",
        format!("首行 Orbit\n{}\n末行 Orbit", "中文 👩‍💻 状态\n".repeat(80)),
    );
    let mut terminal = Terminal::new(TestBackend::new(70, 30)).unwrap();
    terminal
        .draw(|frame| super::super::render::draw(frame, &mut state))
        .unwrap();
    assert!(state.ime_cursor.is_none());
    assert!(state.info_dialog.as_ref().unwrap().max_scroll > 0);
    let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
    handle(&mut state, key(KeyCode::PageDown));
    let dialog = state.info_dialog.as_ref().unwrap();
    assert_eq!(dialog.scroll, dialog.page_rows);
    handle(&mut state, key(KeyCode::End));
    terminal
        .draw(|frame| super::super::render::draw(frame, &mut state))
        .unwrap();
    let screen = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    let text = screen.replace(' ', "");
    assert!(text.contains("末行Orbit"));
    assert!(!text.contains("首行Orbit"));
    handle(
        &mut state,
        Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
    );
    assert!(state.info_dialog.is_none());
    assert_eq!(state.input.text, "Orbit 草稿");
    assert!(state.transcript.is_empty());
    terminal
        .draw(|frame| super::super::render::draw(frame, &mut state))
        .unwrap();
    assert!(state.ime_cursor.is_some());
}

#[test]
fn home_errors_leave_no_transcript_and_keep_dialogs_safe_at_small_sizes() {
    let mut state = UiState::new("orbit::model");
    for (width, height) in [(141, 41), (28, 18), (10, 3), (1, 1)] {
        state.report_local(super::super::app::TranscriptItem::CommandError(
            "Orbit 操作失败：\u{1b}[31m".into(),
        ));
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| super::super::render::draw(frame, &mut state))
            .unwrap();
        assert!(state.transcript.is_empty());
        assert!(state.ime_cursor.is_none());
        let buffer = terminal.backend().buffer();
        assert!(
            buffer
                .content
                .iter()
                .all(|cell| !cell.symbol().contains('\u{1b}'))
        );
        if width > 20 {
            let border = buffer
                .content
                .iter()
                .position(|cell| cell.symbol() == "┌")
                .unwrap();
            let x = border as u16 % width;
            let panel_width = width.saturating_sub(4).min(96);
            assert!(width - x - panel_width - x <= 1);
        }

        handle(
            &mut state,
            Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
        );
        assert!(state.info_dialog.is_none());
    }
}

#[test]
fn delete_requires_two_presses_to_confirm_and_any_other_key_cancels() {
    let mut picker = SessionPicker::new(vec![
        session("keep", "保留的会话"),
        session("del", "要删除的会话"),
    ]);
    picker.selected = 1;

    assert!(picker.pending_delete.is_none());
    let del_key = Event::Key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE));

    let mut state = UiState::new("model");
    state.picker = Some(picker);

    handle(&mut state, del_key.clone());
    assert_eq!(
        state.picker.as_ref().unwrap().pending_delete.as_deref(),
        Some("del")
    );
    assert!(state.delete_request.is_none());

    handle(
        &mut state,
        Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
    );
    assert!(state.picker.as_ref().unwrap().pending_delete.is_none());

    handle(
        &mut state,
        Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
    );
    handle(&mut state, del_key.clone()); // first press
    handle(&mut state, del_key.clone()); // second press confirms
    assert_eq!(state.delete_request.as_deref(), Some("del"));
    assert!(state.picker.as_ref().unwrap().pending_delete.is_none());
}

#[test]
fn delete_confirmed_with_enter_after_first_del_press() {
    let mut state = UiState::new("model");
    state.picker = Some(SessionPicker::new(vec![session("abc", "会话")]));

    handle(
        &mut state,
        Event::Key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE)),
    );
    assert!(state.picker.as_ref().unwrap().pending_delete.is_some());

    handle(
        &mut state,
        Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
    );
    assert_eq!(state.delete_request.as_deref(), Some("abc"));
    assert!(state.picker.as_ref().unwrap().pending_delete.is_none());
}

#[test]
fn calendar_year_changes_only_once_for_january_and_february() {
    assert_eq!(days_to_ymd(19722), (2023, 12, 31));
    assert_eq!(days_to_ymd(19723), (2024, 1, 1));
    assert_eq!(days_to_ymd(19782), (2024, 2, 29));
    assert_eq!(days_to_ymd(19783), (2024, 3, 1));
}

#[test]
fn format_session_time_produces_iso_date_and_dash_for_zero() {
    assert_eq!(format_session_time(0), "—");
    assert_eq!(format_session_time(-1), "—");

    // 2024-06-15 12:00:00 UTC. On any system we get HH:MM in local time, so
    // only the date portion is safe to assert without knowing the TZ offset.
    let s = format_session_time(1_718_445_600);
    assert!(
        s.starts_with("2024-06-"),
        "expected 2024-06-xx date, got {s}"
    );
    let re_like = s.len() == 16
        && s.as_bytes()[4] == b'-'
        && s.as_bytes()[7] == b'-'
        && s.as_bytes()[10] == b' '
        && s.as_bytes()[13] == b':';

    assert!(re_like, "unexpected format: {s}");
}

#[test]
fn resume_search_ranks_titles_with_the_shared_matching_rules() {
    let mut picker = SessionPicker::new(vec![
        session("one", "a-long-b-long-c"),
        session("two", "项目 abc"),
        session("three", "ABC 开头"),
        session("aabbcc", "按编号查找"),
    ]);

    picker.query = "abc".into();

    let ids = picker
        .matches()
        .iter()
        .map(|session| session.id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(ids, ["three", "two", "one"]);
    picker.query = "项c".into();
    assert_eq!(picker.matches()[0].id, "two");
}

#[test]
fn mouse_confirms_session_rows_and_wheel_preserves_selection() {
    use crossterm::event::{MouseButton, MouseEvent};
    let mut state = UiState::new("model");
    state.picker = Some(SessionPicker::new(vec![
        session("a", "first"),
        session("b", "second"),
    ]));
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal
        .draw(|frame| super::super::render::draw(frame, &mut state))
        .unwrap();
    let area = state
        .dialog_rows
        .iter()
        .find(|(_, index)| *index == 1)
        .unwrap()
        .0;
    let mut mouse = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: area.x,
        row: area.y,
        modifiers: KeyModifiers::NONE,
    };

    assert_eq!(handle(&mut state, Event::Mouse(mouse)), Some("b".into()));
    assert_eq!(state.picker.as_ref().unwrap().selected, 1);
    mouse.kind = MouseEventKind::ScrollUp;
    handle(&mut state, Event::Mouse(mouse));
    assert_eq!(state.picker.as_ref().unwrap().selected, 1);
}

#[test]
fn selected_session_title_scrolls_and_resets_for_selection_search_and_width() {
    use std::time::{Duration, Instant};

    let mut picker = SessionPicker::new(vec![
        session("first", "中文标题 abcdefghijklmnopqrstuvwxyz"),
        session("second", "另一段很长的会话标题 abcdefghijklmnopqrstuvwxyz"),
        session("short", "短标题"),
    ]);

    picker.prepare_title_scroll(12);
    picker.title_scroll.as_mut().unwrap().2 = Instant::now() - Duration::from_secs(1);

    assert!(picker.advance_title_scroll());
    let elapsed = picker.prepare_title_scroll(12);
    let title = &picker.sessions[0].title;
    assert_ne!(
        super::super::render::scrolling_title(title, 12, elapsed),
        super::super::render::scrolling_title(title, 12, Some(Duration::ZERO)),
    );

    picker.move_by(true);
    picker.prepare_title_scroll(12);
    assert_eq!(picker.title_scroll_tick, 0);
    picker.title_scroll.as_mut().unwrap().2 = Instant::now() - Duration::from_secs(1);
    picker.prepare_title_scroll(14);
    assert_eq!(picker.title_scroll_tick, 0);
    assert!(picker.title_scroll.as_ref().unwrap().2.elapsed() < Duration::from_secs(1));

    picker.query = "短标题".into();
    picker.selected = 0;
    assert!(picker.prepare_title_scroll(12).is_none());
    assert!(!picker.advance_title_scroll());
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn session(id: &str, title: &str) -> Session {
    Session {
        id: id.into(),
        title: title.into(),
        workspace: "/project".into(),
        project_id: "project-id".into(),
        model: "model".into(),
        reasoning_effort: None,
        demo: true,
        // 2024-06-15 12:00:00 UTC — a stable, non-zero timestamp so that
        // the rendered subtitle shows a date rather than "—".
        updated_at: 1_718_445_600,
    }
}

#[test]
fn model_picker_draws_on_a_terminal_too_short_for_body_rows() {
    let mut state = UiState::new("model");
    state.model_picker = Some(ModelPicker::new(
        Vec::<ModelOption>::new(),
        "model".into(),
        None,
    ));
    let mut terminal = Terminal::new(TestBackend::new(12, 6)).unwrap();

    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
}

#[test]
fn grouped_models_keep_last_row_visible_and_support_mouse_selection() {
    let models: Vec<_> = (0..30)
        .map(|index| ModelOption {
            max_output_tokens: 32_000,
            provider: format!("provider-{}", index / 3),
            model: format!("model-{index}"),
            context_tokens: 128_000,
            input_context_tokens: 96_000,
            reasoning_efforts: Vec::new(),
            supports_vision: false,
            config: orbit_model::types::ModelConfig::default(),
        })
        .collect();
    let mut state = UiState::new("provider-9::model-29");
    state.model_picker = Some(ModelPicker::new(models, state.model.clone(), None));
    let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();

    terminal.draw(|frame| draw(frame, &mut state)).unwrap();

    let (last, _) = state
        .dialog_rows
        .iter()
        .find(|(_, index)| *index == 29)
        .unwrap();

    assert!(last.height > 0 && last.width > 0);
    let (area, index) = state.dialog_rows[0];
    let mouse = |kind| {
        Event::Mouse(crossterm::event::MouseEvent {
            kind,
            column: area.x,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        })
    };

    assert!(super::super::handle_list_hover(
        &mut state,
        &mouse(MouseEventKind::Moved)
    ));
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();

    assert_eq!(state.model_picker.as_ref().unwrap().selected, index);
    assert_eq!(state.dialog_rows[0], (area, index));
    assert_eq!(
        terminal.backend().buffer()[(area.x, area.y)].bg,
        crate::tui::theme::THEME.accent
    );
    assert!(state.chosen_model.is_none());

    handle(&mut state, mouse(MouseEventKind::ScrollUp));
    assert_eq!(state.model_picker.as_ref().unwrap().selected, index);
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    assert!(state.dialog_rows[0].1 < index);
    state.model_picker.as_mut().unwrap().offset = index;
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let blank = Event::Mouse(crossterm::event::MouseEvent {
        kind: MouseEventKind::Moved,
        column: area.right() + 1,
        row: area.y,
        modifiers: KeyModifiers::NONE,
    });

    assert!(!super::super::handle_list_hover(&mut state, &blank));
    assert_eq!(state.model_picker.as_ref().unwrap().selected, index);

    handle(
        &mut state,
        mouse(MouseEventKind::Down(crossterm::event::MouseButton::Left)),
    );
    assert!(state.model_picker.is_none());
    assert_eq!(
        state.chosen_model,
        Some(format!("provider-{}::model-{index}", index / 3))
    );
}

#[test]
fn scrolling_recomputes_hover_from_the_new_rows_without_pointer_motion() {
    let models: Vec<_> = (0..30)
        .map(|index| ModelOption {
            max_output_tokens: 32_000,
            provider: "provider".into(),
            model: format!("model-{index}"),
            context_tokens: 128_000,
            input_context_tokens: 96_000,
            reasoning_efforts: Vec::new(),
            supports_vision: false,
            config: orbit_model::types::ModelConfig::default(),
        })
        .collect();
    let mut state = UiState::new("provider::model-0");
    state.model_picker = Some(ModelPicker::new(models, state.model.clone(), None));
    let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
    terminal
        .draw(|frame| super::super::render::draw(frame, &mut state))
        .unwrap();
    let (area, before) = state.dialog_rows[2];

    handle(
        &mut state,
        Event::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: area.x + 2,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        }),
    );
    terminal
        .draw(|frame| super::super::render::draw(frame, &mut state))
        .unwrap();

    let after = state
        .dialog_rows
        .iter()
        .find(|(rect, _)| rect.contains((area.x + 2, area.y).into()))
        .unwrap()
        .1;

    assert!(after > before);
    assert_eq!(state.model_picker.as_ref().unwrap().selected, after);
    assert!(state.pending_list_hover.is_none());
    assert!(state.chosen_model.is_none());
    assert_eq!(
        terminal.backend().buffer()[(area.x + 2, area.y)].bg,
        crate::tui::theme::THEME.accent
    );
    let buffer = terminal.backend().buffer();
    let footer_y = (0..20)
        .find(|y| {
            (0..100)
                .map(|x| buffer[(x, *y)].symbol())
                .collect::<String>()
                .replace(' ', "")
                .contains("共30个模型·Esc返回")
        })
        .unwrap();

    assert!((3..97).all(|x| buffer[(x, footer_y - 1)].symbol() == " "));
}

#[test]
fn picker_filters_selects_and_cancels_without_touching_draft() {
    let mut state = UiState::new("model");
    state.input.insert("draft");
    state.picker = Some(SessionPicker::new(vec![
        session("abc", "older task"),
        session("def", "目标任务"),
    ]));
    handle(&mut state, Event::Paste("目标".into()));

    assert_eq!(handle(&mut state, key(KeyCode::Enter)), Some("def".into()));

    handle(&mut state, key(KeyCode::Esc));

    assert!(state.picker.is_none());
    assert_eq!(state.input.text, "draft");
}

#[test]
fn model_picker_filters_and_returns_a_qualified_model_name() {
    let mut state = UiState::new("deepseek::deepseek-chat");
    state.model_picker = Some(ModelPicker::new(
        vec![
            ModelOption {
                max_output_tokens: 32_000,
                provider: "deepseek".into(),
                model: "deepseek-chat".into(),
                context_tokens: 64_000,
                input_context_tokens: 32_000,
                reasoning_efforts: vec![orbit_model::types::ReasoningEffort::Low],
                supports_vision: false,
                config: orbit_model::types::ModelConfig::default(),
            },
            ModelOption {
                max_output_tokens: 32_000,
                provider: "openai".into(),
                model: "gpt-6-astra".into(),
                context_tokens: 128_000,
                input_context_tokens: 96_000,
                reasoning_efforts: vec![
                    orbit_model::types::ReasoningEffort::Medium,
                    orbit_model::types::ReasoningEffort::High,
                ],
                supports_vision: true,
                config: orbit_model::types::ModelConfig::default(),
            },
        ],
        state.model.clone(),
        Some(orbit_model::types::ReasoningEffort::Low),
    ));
    handle(&mut state, Event::Paste("astra".into()));
    handle(&mut state, key(KeyCode::Right));
    handle(&mut state, key(KeyCode::Enter));

    assert!(state.model_picker.is_none());
    assert_eq!(state.chosen_model.as_deref(), Some("openai::gpt-6-astra"));
    assert_eq!(
        state.chosen_effort,
        Some(orbit_model::types::ReasoningEffort::Medium)
    );
}

#[test]
fn model_picker_arrows_do_not_change_thinking_effort() {
    let mut state = UiState::new("deepseek::deepseek-chat");
    state.model_picker = Some(ModelPicker::new(
        vec![
            ModelOption {
                max_output_tokens: 32_000,
                provider: "deepseek".into(),
                model: "deepseek-chat".into(),
                context_tokens: 64_000,
                input_context_tokens: 32_000,
                reasoning_efforts: vec![
                    orbit_model::types::ReasoningEffort::None,
                    orbit_model::types::ReasoningEffort::Low,
                ],
                supports_vision: false,
                config: orbit_model::types::ModelConfig::default(),
            },
            ModelOption {
                max_output_tokens: 32_000,
                provider: "deepseek".into(),
                model: "strict-model".into(),
                context_tokens: 64_000,
                input_context_tokens: 32_000,
                reasoning_efforts: vec![orbit_model::types::ReasoningEffort::Low],
                supports_vision: false,
                config: orbit_model::types::ModelConfig::default(),
            },
        ],
        state.model.clone(),
        Some(orbit_model::types::ReasoningEffort::Low),
    ));
    handle(&mut state, key(KeyCode::Left));
    handle(&mut state, key(KeyCode::Enter));

    assert!(state.model_picker.is_none());
    assert_eq!(
        state.chosen_effort,
        Some(orbit_model::types::ReasoningEffort::Low)
    );

    state.model = "deepseek::strict-model".into();
    state.model_picker = Some(ModelPicker::new(
        vec![ModelOption {
            max_output_tokens: 32_000,
            provider: "deepseek".into(),
            model: "strict-model".into(),
            context_tokens: 64_000,
            input_context_tokens: 32_000,
            reasoning_efforts: vec![orbit_model::types::ReasoningEffort::Low],
            supports_vision: false,
            config: orbit_model::types::ModelConfig::default(),
        }],
        state.model.clone(),
        Some(orbit_model::types::ReasoningEffort::Low),
    ));
    handle(&mut state, key(KeyCode::Left));
    handle(&mut state, key(KeyCode::Enter));

    assert_eq!(
        state.chosen_effort,
        Some(orbit_model::types::ReasoningEffort::Low)
    );
}

#[test]
fn marketplace_dialog_browses_plugins_then_returns_to_market_list() {
    let mut state = UiState::new("model");
    state.marketplace_dialog = Some(MarketplaceDialog::new(
        vec![Marketplace {
            name: "official".into(),
            source: "owner/catalog".into(),
            directory: "/catalog".into(),
            revision: None,
            plugins: vec![
                crate::marketplaces::Plugin {
                    name: "review".into(),
                    description: "Review pull requests".into(),
                    source: None,
                },
                crate::marketplaces::Plugin {
                    name: "docs".into(),
                    description: "Write documentation".into(),
                    source: None,
                },
            ],
        }],
        "project".into(),
        Vec::new(),
    ));

    handle(&mut state, key(KeyCode::Enter));

    assert!(
        state
            .marketplace_dialog
            .as_ref()
            .is_some_and(|dialog| dialog.viewing_plugins)
    );

    handle(&mut state, key(KeyCode::Down));

    assert_eq!(
        state
            .marketplace_dialog
            .as_ref()
            .map(|dialog| dialog.selected_plugin),
        Some(1)
    );

    handle(&mut state, key(KeyCode::Esc));

    assert!(
        state
            .marketplace_dialog
            .as_ref()
            .is_some_and(|dialog| !dialog.viewing_plugins && dialog.selected_plugin == 0)
    );
}

#[test]
fn marketplace_dialog_collects_source_before_starting_download() {
    let mut state = UiState::new("model");
    state.marketplace_dialog = Some(MarketplaceDialog::new(
        Vec::new(),
        "project".into(),
        Vec::new(),
    ));

    handle(&mut state, key(KeyCode::Enter));
    handle(&mut state, Event::Paste("owner/catalog".into()));
    handle(&mut state, key(KeyCode::Enter));

    assert!(state.marketplace_dialog.is_none());
    assert_eq!(
        state.pending_marketplace_source.as_deref(),
        Some("owner/catalog")
    );
}

fn plugin_marketplace() -> Marketplace {
    Marketplace {
        name: "official".into(),
        source: "owner/catalog".into(),
        directory: "/catalog".into(),
        revision: None,
        plugins: vec![crate::marketplaces::Plugin {
            name: "review".into(),
            description: "Review pull requests".into(),
            source: Some("plugins/review".into()),
        }],
    }
}

fn inspected_plugin() -> PluginPackage {
    PluginPackage {
        name: "review".into(),
        version: Some("1.2.3".into()),
        description: Some("Review pull requests".into()),
        directory: "/catalog/plugins/review".into(),
        skills: Vec::new(),
        commands: Vec::new(),
        agents: Vec::new(),
        declares_hooks: false,
        declares_mcp: false,
    }
}

#[test]
fn marketplace_dialog_enables_inspected_plugin_for_current_project() {
    let mut state = UiState::new("model");
    state.marketplace_dialog = Some(MarketplaceDialog::new(
        vec![plugin_marketplace()],
        "project".into(),
        Vec::new(),
    ));
    let dialog = state.marketplace_dialog.as_mut().unwrap();
    dialog.viewing_plugins = true;
    dialog.inspection = Some(Ok(inspected_plugin()));

    handle(&mut state, key(KeyCode::Char('e')));

    assert!(state.marketplace_dialog.is_none());
    assert_eq!(
        state.pending_project_plugin_change,
        Some(ProjectPluginChange::Enable(ProjectPlugin {
            project_id: "project".into(),
            marketplace_source: "owner/catalog".into(),
            plugin_name: "review".into(),
            package_source: "plugins/review".into(),
            version: Some("1.2.3".into()),
            marketplace_revision: None,
            enabled_at: 0,
        }))
    );
}

#[test]
fn marketplace_dialog_disables_already_enabled_plugin() {
    let enabled = ProjectPlugin {
        project_id: "project".into(),
        marketplace_source: "owner/catalog".into(),
        plugin_name: "review".into(),
        package_source: "plugins/review".into(),
        version: Some("1.2.3".into()),
        marketplace_revision: None,
        enabled_at: 1,
    };
    let mut state = UiState::new("model");
    state.marketplace_dialog = Some(MarketplaceDialog::new(
        vec![plugin_marketplace()],
        "project".into(),
        vec![enabled],
    ));
    let dialog = state.marketplace_dialog.as_mut().unwrap();
    dialog.viewing_plugins = true;
    dialog.inspection = Some(Ok(inspected_plugin()));

    handle(&mut state, key(KeyCode::Char('e')));

    assert_eq!(
        state.pending_project_plugin_change,
        Some(ProjectPluginChange::Disable {
            project_id: "project".into(),
            marketplace_source: "owner/catalog".into(),
            plugin_name: "review".into(),
        })
    );
}

#[test]
fn help_has_its_own_panel_and_all_shortcuts_are_reachable() {
    let mut state = UiState::new("model");
    state.show_help = true;

    for (width, height) in [(80, 24), (40, 14), (24, 10)] {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();

        assert!(text.contains("Orbit"));
    }

    handle(&mut state, key(KeyCode::Right));

    assert!(state.help_keys);

    for _ in 0..100 {
        handle(&mut state, key(KeyCode::Down));
    }

    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let text: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();

    assert!(text.contains("?"));
    assert!(!text.contains("/help"));

    handle(&mut state, key(KeyCode::Esc));

    assert!(!state.show_help);
}

#[test]
fn locked_session_error_is_visible_above_the_list_in_error_color() {
    let mut state = UiState::new("model");
    let mut picker = SessionPicker::new(vec![session("abc", "目标会话")]);
    picker.error = Some("该会话已在其他 Orbit 窗口或进程中打开，请先关闭后再恢复。".into());
    state.picker = Some(picker);

    for width in [40, 80, 120] {
        let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let buffer = terminal.backend().buffer();
        let error_cells: Vec<_> = buffer
            .content()
            .iter()
            .enumerate()
            .filter(|(_, cell)| {
                cell.fg == crate::tui::theme::THEME.danger && !cell.symbol().trim().is_empty()
            })
            .collect();

        assert!(!error_cells.is_empty());
        assert!(error_cells.iter().all(|(i, _)| i / usize::from(width) < 8));

        let text = error_cells
            .iter()
            .map(|(_, cell)| cell.symbol())
            .collect::<String>();

        assert!(text.contains("已在其他Orbit"), "{text}");
        assert!(state.picker.is_some());
    }
}

#[test]
fn panel_title_uses_the_inner_header_not_the_border() {
    let mut state = UiState::new("model");
    state.picker = Some(SessionPicker::new(vec![session("abc", "a session")]));
    let width = 80;
    let mut terminal = Terminal::new(TestBackend::new(width, 24)).unwrap();
    terminal.draw(|frame| draw(frame, &mut state)).unwrap();
    let buffer = terminal.backend().buffer();
    let text_at = |row: u16| {
        (0..width)
            .map(|column| {
                buffer.content()[(row as usize * width as usize) + column as usize].symbol()
            })
            .collect::<String>()
    };

    assert!(!text_at(1).contains('恢'));
    assert!(text_at(2).contains('恢'), "{}", text_at(2));
    assert!(text_at(1).contains('─'));
}

#[test]
fn dialog_borders_survive_wide_background_text_and_restore_on_close() {
    for width in [80, 81, 120, 121] {
        for glyph in ["中", "👩‍💻", "a\u{301}"] {
            for indent in ["", " "] {
                let mut state = UiState::new("model");
                let mut terminal = Terminal::new(TestBackend::new(width, 30)).unwrap();
                let background =
                    Paragraph::new(vec![
                        Line::from(format!("{indent}{}", glyph.repeat(150)));
                        30
                    ]);

                terminal
                    .draw(|frame| frame.render_widget(background.clone(), frame.area()))
                    .unwrap();
                let original = terminal.backend().buffer().clone();
                let area = Rect::new(
                    (width - width.saturating_sub(4).min(96)) / 2,
                    3,
                    width.saturating_sub(4).min(96),
                    24,
                );

                for query in ["", "中文"] {
                    let mut picker = SessionPicker::new(vec![session("abc", "中文会话")]);
                    picker.query = query.into();
                    state.picker = Some(picker);
                    terminal
                        .draw(|frame| {
                            frame.render_widget(background.clone(), frame.area());
                            draw(frame, &mut state);
                        })
                        .unwrap();
                    let buffer = terminal.backend().buffer();

                    for y in area.y + 1..area.bottom() - 1 {
                        for x in [area.x, area.right() - 1] {
                            assert_eq!(
                                buffer[(x, y)].symbol(),
                                "│",
                                "width={width}, glyph={glyph}, indent={indent:?}, point=({x},{y})"
                            );
                        }
                    }
                }

                state.picker = None;
                terminal
                    .draw(|frame| {
                        frame.render_widget(background.clone(), frame.area());
                        draw(frame, &mut state);
                    })
                    .unwrap();

                use ratatui::buffer::CellWidth;

                for y in 0..30 {
                    let mut x = 0;

                    while x < width {
                        assert_eq!(
                            terminal.backend().buffer()[(x, y)],
                            original[(x, y)],
                            "restored visible cell at ({x},{y})"
                        );

                        x += original[(x, y)].cell_width().max(1);
                    }
                }
            }
        }
    }
}
