use super::super::form::ACCENT;
use super::*;
use ratatui::{Terminal, backend::TestBackend};

fn setup() -> (tempfile::TempDir, ConnectionDraft, Panel) {
    let directory = tempfile::tempdir().unwrap();
    let draft = ConnectionDraft::load(&directory.path().join("config.toml"), None).unwrap();
    let service = vec![
        Field::text("名称", "local", ""),
        Field::text("地址", "http://localhost:8000/v1", ""),
        Field::choice("协议", "OpenAI Chat Completions", &super::super::FORMATS),
        Field::text("密钥", "test-key", ""),
    ];
    let panel = Panel::new(service, &draft).unwrap();
    (directory, draft, panel)
}

#[test]
fn inline_edits_survive_collapsing_filtering_and_default_change() {
    let (directory, mut draft, mut panel) = setup();
    panel.add(model::blank("first"), true);
    panel.add(model::blank("second"), true);
    panel.expanded = Some(0);
    panel.models[0].fields[1].input.set("65536".into());
    panel.models[0].fields[3].input.set("4096".into());
    panel.models[0].fields[4].input.set("支持".into());

    panel.expanded = Some(1);
    panel.query.input.set("second".into());
    panel.default = Some(1);
    panel.service_draft(&mut draft).unwrap();
    panel.apply_models(&mut draft).unwrap();

    assert_eq!(draft.models[0].id, "second");
    assert_eq!(draft.models[1].max_output_tokens, Some(4096));
    assert_eq!(draft.models[1].context_tokens, Some(65536));
    assert_eq!(draft.models[1].supports_vision, Some(true));
    assert!(!directory.path().join("config.toml").exists());
}

#[test]
fn scroll_recomputes_hover_and_keeps_footer_fixed() {
    let (_directory, _draft, mut panel) = setup();
    for i in 0..30 {
        panel.add(model::blank(&format!("model-{i}")), true);
    }
    let first_line = panel
        .rows()
        .iter()
        .position(|r| r.target == Some(Target::Action(Action::Toggle(0))))
        .unwrap();
    panel.offset = first_line;
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    let mut before = None;
    terminal
        .draw(|frame| {
            before = Some(view::draw(&mut panel, frame, false));
        })
        .unwrap();
    let (_, hits, _) = before.unwrap();
    let point = hits
        .iter()
        .find(|(_, t)| *t == Target::Action(Action::Toggle(0)))
        .unwrap()
        .0
        .as_position();
    let save = hits
        .iter()
        .find(|(_, t)| *t == Target::Action(Action::Save))
        .unwrap()
        .0;

    panel.pointer = Some(point);
    panel.offset += 3;
    let mut after = None;
    terminal
        .draw(|frame| {
            after = Some(view::draw(&mut panel, frame, false));
        })
        .unwrap();
    let (_, hits, _) = after.unwrap();

    assert!(
        hits.iter()
            .any(|(rect, t)| rect.contains(point) && *t == Target::Action(Action::Toggle(3)))
    );
    assert_eq!(terminal.backend().buffer()[(point.x, point.y)].bg, ACCENT);
    assert!(hits.contains(&(save, Target::Action(Action::Save))));
    assert!(
        !hits
            .iter()
            .any(|(rect, _)| rect.contains(Position::new(point.x + 40, point.y)))
    );
}

#[test]
fn unchanged_reasoning_preserves_provider_adapter_and_extra() {
    let profile = crate::config::ReasoningProfile {
        adapter: orbit_model::types::ReasoningAdapter::AnthropicMessages,
        efforts: vec![orbit_model::types::ReasoningEffort::High],
        default: orbit_model::types::ReasoningEffort::High,
        extra: toml::from_str("budget_tokens = 4096").unwrap(),
    };
    let fields = model::reasoning_fields(&Some(profile.clone()));

    let restored = model::read_reasoning(&fields, &Some(profile), ApiFormat::AnthropicMessages)
        .unwrap()
        .unwrap();

    assert_eq!(
        restored.adapter,
        orbit_model::types::ReasoningAdapter::AnthropicMessages
    );
    assert_eq!(restored.extra["budget_tokens"].as_integer(), Some(4096));
}

#[test]
fn reasoning_buttons_share_a_row_and_keep_default_enabled() {
    let (_directory, mut draft, mut panel) = setup();
    panel.add(model::blank("custom"), true);
    panel.expanded = Some(0);
    assert!(panel.models[0].fields[1].value().is_empty());
    assert!(panel.models[0].fields[1].hint.is_empty());
    assert!(panel.models[0].reasoning[8].value().is_empty());

    panel.toggle_effort(0, 3);
    panel.toggle_effort(0, 5);
    panel.toggle_effort(0, 3);
    panel.apply_models(&mut draft).unwrap();

    let reasoning = draft.models[0].reasoning.as_ref().unwrap();
    assert_eq!(
        reasoning.efforts,
        vec![orbit_model::types::ReasoningEffort::High]
    );
    assert_eq!(reasoning.default, orbit_model::types::ReasoningEffort::High);
    let mut terminal = Terminal::new(TestBackend::new(110, 45)).unwrap();
    let mut layout = None;
    terminal
        .draw(|frame| {
            layout = Some(view::draw(&mut panel, frame, false));
        })
        .unwrap();
    let (_, hits, _) = layout.unwrap();
    let buttons = hits
        .iter()
        .filter(|(_, t)| matches!(t, Target::Action(Action::Effort(_, _))))
        .collect::<Vec<_>>();
    assert_eq!(buttons.len(), 7);
    assert!(
        buttons
            .iter()
            .all(|(rect, _)| rect.y == buttons[0].0.y && rect.width > 0)
    );

    panel.toggle_effort(0, 5);
    panel.apply_models(&mut draft).unwrap();

    assert!(draft.models[0].reasoning.is_none());
}

#[test]
fn toolbar_and_single_choices_are_inline_and_focused_label_is_accented() {
    let (_directory, _draft, mut panel) = setup();
    panel.focus = Target::Field(FieldId::Service(1));
    let mut terminal = Terminal::new(TestBackend::new(110, 40)).unwrap();
    let mut layout = None;

    terminal
        .draw(|frame| {
            layout = Some(view::draw(&mut panel, frame, false));
        })
        .unwrap();

    let (viewport, hits, _) = layout.unwrap();
    let buttons = [Action::Load, Action::Add, Action::All].map(|action| {
        hits.iter()
            .find(|(_, target)| *target == Target::Action(action))
            .unwrap()
            .0
    });
    assert!(buttons.iter().all(|rect| rect.y == buttons[0].y));
    let choices = hits
        .iter()
        .filter(|(_, target)| matches!(target, Target::Choice(FieldId::Service(2), _)))
        .collect::<Vec<_>>();
    assert_eq!(choices.len(), 3);
    assert!(
        choices
            .iter()
            .all(|(rect, _)| rect.y == choices[0].0.y && rect.width > 0)
    );
    let input = hits
        .iter()
        .find(|(_, target)| *target == Target::Field(FieldId::Service(1)))
        .unwrap()
        .0;
    assert_eq!(
        terminal.backend().buffer()[(viewport.x, input.y)].fg,
        ACCENT
    );
}

#[test]
fn arrows_follow_rows_and_columns_instead_of_tab_order() {
    let (_directory, _draft, mut panel) = setup();
    panel.focus = Target::Choice(FieldId::Service(2), 0);

    panel.navigate(KeyCode::Right, 20);
    assert_eq!(panel.focus, Target::Choice(FieldId::Service(2), 1));
    panel.navigate(KeyCode::Down, 20);
    assert_eq!(panel.focus, Target::Field(FieldId::Service(3)));
    panel.navigate(KeyCode::Up, 20);
    assert_eq!(panel.focus, Target::Choice(FieldId::Service(2), 0));
    panel.navigate(KeyCode::Left, 20);
    assert_eq!(panel.focus, Target::Choice(FieldId::Service(2), 0));

    panel.focus = Target::Action(Action::Load);
    panel.navigate(KeyCode::Right, 20);
    assert_eq!(panel.focus, Target::Action(Action::Add));
    panel.navigate(KeyCode::Down, 20);
    assert_eq!(panel.focus, Target::Field(FieldId::Search));
    let rows = panel.rows();
    let search = rows
        .iter()
        .position(|row| row.target == Some(panel.focus))
        .unwrap();
    assert!(rows[search - 1].target.is_none());
}

#[test]
fn default_effort_only_offers_enabled_values() {
    let (_directory, _draft, mut panel) = setup();
    panel.add(model::blank("custom"), true);
    panel.expanded = Some(0);
    let id = FieldId::Reasoning(0, 8);
    assert!(
        !panel
            .rows()
            .iter()
            .any(|row| row.target == Some(Target::Field(id)))
    );

    panel.toggle_effort(0, 3);
    panel.toggle_effort(0, 5);
    let visible = (0..7)
        .filter(|index| panel.choice_visible(id, *index))
        .collect::<Vec<_>>();

    assert_eq!(visible, vec![2, 4]);
    assert!(
        panel
            .rows()
            .iter()
            .any(|row| row.target == Some(Target::Field(id)))
    );
    panel.focus = Target::Choice(id, 2);
    panel.navigate(KeyCode::Right, 30);
    assert_eq!(panel.focus, Target::Choice(id, 4));

    panel.toggle_effort(0, 3);
    panel.toggle_effort(0, 5);

    assert!(
        !panel
            .rows()
            .iter()
            .any(|row| row.target == Some(Target::Field(id)))
    );
}

#[test]
fn editing_saved_key_preserves_it_until_replacement_is_entered() {
    let (_directory, _draft, mut panel) = setup();
    panel.saved_auth = true;
    panel.service[3].input.set(String::new());
    panel.service[3].hint = "输入 API Key".into();
    assert!(matches!(panel.key(), KeyChange::Keep));

    panel.service[3].input.set("replacement-key".into());
    assert!(matches!(panel.key(), KeyChange::Replace("replacement-key")));

    panel.service[3].input.clear();
    assert!(matches!(panel.key(), KeyChange::Keep));
    panel.saved_auth = false;
    assert!(matches!(panel.key(), KeyChange::Replace("")));
}

#[tokio::test]
async fn changing_protocol_converts_reasoning_before_saving() {
    use orbit_model::types::{ReasoningAdapter, ReasoningEffort};

    let (directory, mut draft, mut panel) = setup();
    let mut profile = model::blank("model");
    profile.reasoning = Some(crate::config::ReasoningProfile {
        adapter: ReasoningAdapter::AnthropicMessages,
        efforts: vec![ReasoningEffort::Low, ReasoningEffort::High],
        default: ReasoningEffort::High,
        extra: toml::from_str("thinking = { type = 'enabled' }").unwrap(),
    });
    panel.add(profile, true);

    panel.service_draft(&mut draft).unwrap();
    panel.apply_models(&mut draft).unwrap();
    let options = crate::connections::edit::save(
        &directory.path().join("config.toml"),
        &directory.path().join("auth"),
        &draft,
        panel.key(),
    )
    .await
    .unwrap();

    assert_eq!(options.len(), 1);
    let reasoning = draft.models[0].reasoning.as_ref().unwrap();
    assert_eq!(reasoning.adapter, ReasoningAdapter::OpenaiChat);
    assert_eq!(
        reasoning.efforts,
        vec![ReasoningEffort::Low, ReasoningEffort::High]
    );
    assert_eq!(reasoning.default, ReasoningEffort::High);
    assert!(reasoning.extra.is_empty());
}

#[test]
fn caret_follows_the_focused_field_and_hides_on_buttons_or_during_loading() {
    let (_directory, _draft, mut panel) = setup();
    panel.service[0].input.set("Orbit你好".into());
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    let mut field_area = None;
    terminal
        .draw(|frame| {
            let (_, hits, _) = view::draw(&mut panel, frame, false);
            field_area = hits
                .into_iter()
                .find(|(_, target)| *target == Target::Field(FieldId::Service(0)))
                .map(|(area, _)| area);
        })
        .unwrap();
    let area = field_area.unwrap();
    assert!(terminal.backend().cursor_visible());
    terminal
        .backend_mut()
        .assert_cursor_position((area.x + 9, area.y));
    panel.focus = Target::Action(Action::Save);
    terminal
        .draw(|frame| {
            view::draw(&mut panel, frame, false);
        })
        .unwrap();
    assert!(!terminal.backend().cursor_visible());
    panel.focus = Target::Field(FieldId::Service(0));
    terminal
        .draw(|frame| {
            view::draw(&mut panel, frame, true);
        })
        .unwrap();
    assert!(!terminal.backend().cursor_visible());
}
