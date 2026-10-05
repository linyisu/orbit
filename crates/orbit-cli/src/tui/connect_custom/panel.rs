//! Shared connection draft and inline editing interactions.
#[cfg(test)]
mod tests;
mod view;

use anyhow::{Result, ensure};
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind,
};
use futures_util::StreamExt;
use orbit_model::client::ApiFormat;
use ratatui::layout::Position;
use tokio_util::task::AbortOnDropHandle;

use super::super::{ModelOption, icons, search, terminal};
use super::{
    form::{self, Field},
    model, models,
};
use crate::{
    config::{FileConfig, ModelProfile},
    connections::edit::{ConnectionDraft, KeyChange},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FieldId {
    Service(usize),
    Search,
    Model(usize, usize),
    Reasoning(usize, usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Action {
    Load,
    Add,
    All,
    Toggle(usize),
    Expand(usize),
    Default(usize),
    Effort(usize, usize),
    Save,
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    Field(FieldId),
    Choice(FieldId, usize),
    Action(Action),
}

struct Row {
    label: String,
    target: Option<Target>,
    indent: u16,
}

struct ModelDraft {
    entry: models::Entry,
    fields: Vec<Field>,
    reasoning: Vec<Field>,
}

struct Panel {
    service: Vec<Field>,
    saved_auth: bool,
    query: Field,
    models: Vec<ModelDraft>,
    expanded: Option<usize>,
    default: Option<usize>,
    focus: Target,
    offset: usize,
    pointer: Option<Position>,
    message: String,
    config: FileConfig,
}

impl Panel {
    fn new(service: Vec<Field>, draft: &ConnectionDraft) -> Result<Self> {
        let config: FileConfig = toml::from_str(&draft.source)?;
        let mut panel = Self {
            saved_auth: draft.account()
                || draft.provider.as_ref().is_some_and(|provider| {
                    provider.credential.is_some() || !provider.api_key.is_empty()
                }),
            service,
            query: Field::text("筛选模型", "", "搜索模型"),
            models: Vec::new(),
            expanded: None,
            default: None,
            focus: Target::Field(FieldId::Service(0)),
            offset: 0,
            pointer: None,
            message: String::new(),
            config,
        };

        for profile in &draft.models {
            panel.add(profile.clone(), true);
        }
        panel.default = (!panel.models.is_empty()).then_some(0);

        Ok(panel)
    }

    fn add(&mut self, profile: ModelProfile, enabled: bool) {
        self.models.push(ModelDraft {
            fields: model::fields(&profile),
            reasoning: model::reasoning_fields(&profile.reasoning),
            entry: models::Entry { profile, enabled },
        });
    }

    fn field(&self, id: FieldId) -> &Field {
        match id {
            FieldId::Service(i) => &self.service[i],
            FieldId::Search => &self.query,
            FieldId::Model(i, j) => &self.models[i].fields[j],
            FieldId::Reasoning(i, j) => &self.models[i].reasoning[j],
        }
    }

    fn field_mut(&mut self, id: FieldId) -> &mut Field {
        match id {
            FieldId::Service(i) => &mut self.service[i],
            FieldId::Search => &mut self.query,
            FieldId::Model(i, j) => &mut self.models[i].fields[j],
            FieldId::Reasoning(i, j) => &mut self.models[i].reasoning[j],
        }
    }

    fn choice_visible(&self, id: FieldId, index: usize) -> bool {
        let FieldId::Reasoning(model, 8) = id else {
            return true;
        };
        let value = &self.field(id).choices[index];
        self.models[model].reasoning[1..=7]
            .iter()
            .any(|field| field.label == *value && field.value() == "启用")
    }

    fn field_rows(&self, rows: &mut Vec<Row>, id: FieldId, indent: u16) {
        let field = self.field(id);
        rows.push(Row {
            label: field.label.clone(),
            target: Some(Target::Field(id)),
            indent,
        });
    }

    fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        for i in 0..self.service.len() {
            if i == 3 && self.service[3].readonly {
                continue;
            }
            self.field_rows(&mut rows, FieldId::Service(i), 0);
        }

        rows.push(Row {
            label: String::new(),
            target: None,
            indent: 0,
        });
        rows.push(Row {
            label: String::new(),
            target: Some(Target::Action(Action::Load)),
            indent: 0,
        });
        rows.push(Row {
            label: String::new(),
            target: None,
            indent: 0,
        });
        self.field_rows(&mut rows, FieldId::Search, 0);

        let indices = search::ranked(0..self.models.len(), |i| {
            search::rank(self.models[*i].fields[0].value(), self.query.value())
        });
        for i in indices {
            let model = &self.models[i];
            rows.push(Row {
                label: format!(
                    "{} {}{}",
                    icons::checkbox(model.entry.enabled),
                    if model.fields[0].value().is_empty() {
                        "新模型"
                    } else {
                        model.fields[0].value()
                    },
                    if self.default == Some(i) {
                        " · 默认"
                    } else {
                        ""
                    }
                ),
                target: Some(Target::Action(Action::Toggle(i))),
                indent: 0,
            });
            // Settings share the model's row; their hit boxes are built during drawing.
            if self.expanded == Some(i) {
                for j in 0..model.fields.len() {
                    self.field_rows(&mut rows, FieldId::Model(i, j), 2);
                }
                rows.push(Row {
                    label: "思考强度".into(),
                    target: Some(Target::Action(Action::Effort(i, 1))),
                    indent: 2,
                });
                if (1..=7).any(|j| model.reasoning[j].value() == "启用") {
                    self.field_rows(&mut rows, FieldId::Reasoning(i, 8), 2);
                }
                rows.push(Row {
                    label: String::new(),
                    target: None,
                    indent: 0,
                });
            }
        }

        rows
    }

    fn toggle_effort(&mut self, i: usize, j: usize) {
        let enabled = self.models[i].reasoning[j].value() != "启用";
        self.models[i].reasoning[j]
            .input
            .set(if enabled { "启用" } else { "关闭" }.into());
        let first = (1..=7).find(|j| self.models[i].reasoning[*j].value() == "启用");
        self.models[i].reasoning[0].input.set(
            if first.is_some() {
                "自定义"
            } else {
                "继承服务端"
            }
            .into(),
        );
        let current = self.models[i].reasoning[8].value().to_owned();
        if !(1..=7).any(|j| {
            self.models[i].reasoning[j].label == current
                && self.models[i].reasoning[j].value() == "启用"
        }) {
            let value = first
                .map(|j| self.models[i].reasoning[j].label.clone())
                .unwrap_or_default();
            self.models[i].reasoning[8].input.set(value);
        }
    }

    fn navigate(&mut self, key: KeyCode, height: u16) {
        let rows = self.rows();
        let mut targets = Vec::new();
        for (line, row) in rows.iter().enumerate() {
            if let Some(target) = row.target {
                if let Target::Field(id) = target
                    && self.field(id).readonly
                {
                    continue;
                }

                if let Target::Field(id) = target
                    && !self.field(id).choices.is_empty()
                {
                    for index in 0..self.field(id).choices.len() {
                        if !self.choice_visible(id, index) {
                            continue;
                        }
                        targets.push((line, Target::Choice(id, index)));
                    }
                } else {
                    targets.push((line, target));
                }

                if target == Target::Action(Action::Load) {
                    targets.push((line, Target::Action(Action::Add)));
                    targets.push((line, Target::Action(Action::All)));
                }

                if let Target::Action(Action::Effort(i, 1)) = target {
                    for j in 2..=7 {
                        targets.push((line, Target::Action(Action::Effort(i, j))));
                    }
                }

                if let Target::Action(Action::Toggle(i)) = target {
                    targets.push((line, Target::Action(Action::Expand(i))));
                    targets.push((line, Target::Action(Action::Default(i))));
                }
            }
        }

        targets.push((rows.len(), Target::Action(Action::Save)));
        targets.push((rows.len(), Target::Action(Action::Cancel)));
        let current = targets
            .iter()
            .position(|(_, t)| *t == self.focus)
            .unwrap_or(0);

        let current_line = targets[current].0;
        let row_start = targets
            .iter()
            .position(|(line, _)| *line == current_line)
            .unwrap_or(current);
        let row_end = targets
            .iter()
            .rposition(|(line, _)| *line == current_line)
            .unwrap_or(current);
        let next = match key {
            KeyCode::Left => current.saturating_sub(1).max(row_start),
            KeyCode::Right => (current + 1).min(row_end),
            KeyCode::Up | KeyCode::Down => {
                let adjacent = if key == KeyCode::Up {
                    row_start.checked_sub(1)
                } else {
                    (row_end + 1 < targets.len()).then_some(row_end + 1)
                };

                if let Some(adjacent) = adjacent {
                    let line = targets[adjacent].0;
                    let start = targets
                        .iter()
                        .position(|(candidate, _)| *candidate == line)
                        .unwrap_or(adjacent);
                    let end = targets
                        .iter()
                        .rposition(|(candidate, _)| *candidate == line)
                        .unwrap_or(adjacent);
                    (start + current - row_start).min(end)
                } else {
                    current
                }
            }

            KeyCode::BackTab => (current + targets.len() - 1) % targets.len(),
            _ => (current + 1) % targets.len(),
        };
        let (line, target) = targets[next];
        self.focus = target;
        if line < rows.len() {
            if line < self.offset {
                self.offset = line;
            }
            if line >= self.offset + height as usize {
                self.offset = line.saturating_sub(height.saturating_sub(1) as usize);
            }
        }
    }

    fn discovered(&mut self, profiles: Vec<ModelProfile>) {
        let mut entries = self
            .models
            .iter()
            .map(|model| models::Entry {
                profile: model::blank(model.fields[0].value()),
                enabled: model.entry.enabled,
            })
            .collect::<Vec<_>>();
        let old = entries.len();
        models::merge(&mut entries, profiles);

        for entry in entries.into_iter().skip(old) {
            self.add(entry.profile, false);
        }
        self.message.clear();
    }

    fn key(&self) -> KeyChange<'_> {
        if self.saved_auth && self.service[3].value().is_empty() {
            KeyChange::Keep
        } else {
            KeyChange::Replace(self.service[3].value())
        }
    }

    fn service_draft(&self, draft: &mut ConnectionDraft) -> Result<()> {
        draft.name = self.service[0].value().into();
        draft.endpoint = self.service[1].value().into();
        if !draft.account() {
            draft.format = match self.service[2].value() {
                "OpenAI Responses" => ApiFormat::OpenaiResponses,
                "Anthropic Messages" => ApiFormat::AnthropicMessages,
                _ => ApiFormat::OpenaiChatCompletions,
            };
        }

        ensure!(!draft.name.is_empty(), "请填写连接名称");
        ensure!(
            url::Url::parse(&draft.endpoint).is_ok(),
            "请填写完整的服务地址"
        );
        ensure!(
            !matches!(self.key(), KeyChange::Replace("")),
            "请输入 API Key"
        );
        // A retained key must never be sent to an edited endpoint during discovery.
        if matches!(self.key(), KeyChange::Keep)
            && let Some(provider) = &draft.provider
        {
            ensure!(
                provider.base_url == draft.endpoint,
                "更改地址后请重新填写 API Key"
            );
        }

        Ok(())
    }

    fn apply_models(&self, draft: &mut ConnectionDraft) -> Result<()> {
        let mut profiles = Vec::new();
        for (i, entry) in self
            .models
            .iter()
            .enumerate()
            .filter(|(_, m)| m.entry.enabled)
        {
            let mut profile = model::read(
                &entry.fields,
                &entry.entry.profile,
                self.config.max_output_tokens,
                self.config.context_tokens,
            )?;

            profile.reasoning = model::read_reasoning(
                &entry.reasoning,
                &entry.entry.profile.reasoning,
                draft.format,
            )?;
            ensure!(
                !profiles
                    .iter()
                    .any(|(_, p): &(usize, ModelProfile)| p.id == profile.id),
                "模型 ID 重复：{}",
                profile.id
            );
            profiles.push((i, profile));
        }

        profiles.sort_by_key(|(i, _)| Some(*i) != self.default);
        draft.models = profiles.into_iter().map(|(_, p)| p).collect();

        Ok(())
    }
}

pub(super) async fn run(
    terminal: &mut terminal::AppTerminal,
    events: &mut EventStream,
    options: &crate::WorkspaceOptions,
    draft: &mut ConnectionDraft,
    service: Vec<Field>,
) -> Result<Option<Vec<ModelOption>>> {
    let mut panel = Panel::new(service, draft)?;
    let mut task: Option<AbortOnDropHandle<Result<Vec<ModelProfile>>>> = None;

    loop {
        let mut layout = None;
        terminal::draw_synchronized(terminal, |frame| {
            layout = Some(view::draw(&mut panel, frame, task.is_some()));
        })?;
        let (viewport, hits, max_offset) = layout.expect("draw produces layout");

        let event = if let Some(handle) = task.as_mut() {
            tokio::select! {
                result = handle => {
                    task = None;
                    match result? {
                        Ok(profiles) => {
                            panel.discovered(profiles);
                        }
                        Err(error) => panel.message = format!("无法读取模型：{error}"),
                    }
                    continue;
                }
                event = events.next() => event,
            }
        } else {
            events.next().await
        };
        let Some(event) = event else {
            return Ok(None);
        };

        let mut activate = None;
        match event? {
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                panel.pointer = None;
                match key.code {
                    KeyCode::Esc => return Ok(None),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(None);
                    }
                    KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => {
                        panel.navigate(key.code, viewport.height);
                    }
                    KeyCode::Left | KeyCode::Right if !matches!(panel.focus, Target::Field(_)) => {
                        panel.navigate(key.code, viewport.height);
                    }
                    KeyCode::Enter => activate = Some(panel.focus),
                    code => {
                        if let Target::Field(id) = panel.focus {
                            let field = panel.field_mut(id);
                            if !field.readonly && field.choices.is_empty() {
                                form::edit(&mut field.input, code, key.modifiers);
                                if matches!(id, FieldId::Service(_)) {
                                    task = None;
                                }
                            }
                        }
                    }
                }
            }

            Event::Paste(text) => {
                if let Target::Field(id) = panel.focus {
                    let field = panel.field_mut(id);
                    if !field.readonly && field.choices.is_empty() {
                        form::paste(&mut field.input, &text);
                        if matches!(id, FieldId::Service(_)) {
                            task = None;
                        }
                    }
                }
            }

            Event::Mouse(mouse) => {
                let point = Position::new(mouse.column, mouse.row);
                panel.pointer = Some(point);
                match mouse.kind {
                    MouseEventKind::ScrollDown if viewport.contains(point) => {
                        panel.offset = (panel.offset + 3).min(max_offset)
                    }
                    MouseEventKind::ScrollUp if viewport.contains(point) => {
                        panel.offset = panel.offset.saturating_sub(3)
                    }
                    MouseEventKind::Down(MouseButton::Left) => {
                        activate = hits
                            .iter()
                            .find(|(r, _)| r.contains(point))
                            .map(|(_, t)| *t)
                    }

                    _ => {}
                }
            }

            _ => {}
        }

        let Some(target) = activate else {
            continue;
        };
        panel.focus = target;
        match target {
            Target::Field(_) => {}
            Target::Choice(id, index) => {
                if !panel.choice_visible(id, index) {
                    continue;
                }
                let value = panel.field(id).choices[index].clone();
                panel.field_mut(id).input.set(value);
                if matches!(id, FieldId::Service(_)) {
                    task = None;
                }
            }

            Target::Action(action) => match action {
                Action::Cancel => return Ok(None),
                Action::Expand(i) => {
                    panel.expanded = if panel.expanded == Some(i) {
                        None
                    } else {
                        Some(i)
                    };
                }

                Action::Effort(i, j) => panel.toggle_effort(i, j),
                Action::Toggle(i) => panel.models[i].entry.enabled = !panel.models[i].entry.enabled,
                Action::Default(i) => {
                    panel.default = Some(i);
                    panel.models[i].entry.enabled = true;
                }

                Action::All => {
                    let enabled = panel.models.iter().any(|m| {
                        !m.entry.enabled
                            && search::rank(m.fields[0].value(), panel.query.value()).is_some()
                    });
                    let query = panel.query.value().to_owned();
                    for m in &mut panel.models {
                        if search::rank(m.fields[0].value(), &query).is_some() {
                            m.entry.enabled = enabled;
                        }
                    }
                }

                Action::Add => {
                    panel.query.input.set(String::new());
                    panel.add(model::blank(""), true);
                    let i = panel.models.len() - 1;
                    panel.expanded = Some(i);
                    panel.focus = Target::Field(FieldId::Model(i, 0));
                    panel.offset = panel
                        .rows()
                        .iter()
                        .position(|r| r.target == Some(panel.focus))
                        .unwrap_or(0)
                        .saturating_sub(2);
                }

                Action::Load | Action::Save => {
                    if let Err(error) = panel.service_draft(draft) {
                        panel.message = error.to_string();
                        continue;
                    }

                    if action == Action::Load {
                        let copy = draft.clone();
                        let secret = match panel.key() {
                            KeyChange::Replace(secret) => Some(secret.to_owned()),
                            _ => None,
                        };
                        let keep = matches!(panel.key(), KeyChange::Keep);
                        let directory = options.auth_directory.clone();
                        panel.message.clear();
                        task = Some(AbortOnDropHandle::new(tokio::spawn(async move {
                            let key = if keep {
                                KeyChange::Keep
                            } else if let Some(secret) = &secret {
                                KeyChange::Replace(secret)
                            } else {
                                KeyChange::None
                            };
                            models::discover(&copy, &key, &directory).await
                        })));
                    } else {
                        if let Err(error) = panel.apply_models(draft) {
                            panel.message = error.to_string();
                            continue;
                        }

                        match crate::connections::edit::save(
                            &options.config,
                            &options.auth_directory,
                            draft,
                            panel.key(),
                        )
                        .await
                        {
                            Ok(models) => return Ok(Some(models)),
                            Err(error) => panel.message = error.to_string(),
                        }
                    }
                }
            },
        }
    }
}
