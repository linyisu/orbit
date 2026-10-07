//! Agent questions: local drafts and a server-authoritative answer lifecycle.
use super::{input::InputState, theme::THEME};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use orbit_protocol::user_input::{Answer, History, Kind, Record, Status};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Paragraph},
};
use std::collections::{HashMap, HashSet};

mod render;
#[cfg(test)]
mod tests;

#[derive(Clone, Debug, Default)]
struct Draft {
    selected: Vec<String>,
    custom: bool,
    text: InputState,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Tab(usize),
    Option(usize),
    Text,
    Previous,
    Next,
    Later,
    Decline,
    History(usize),
    More,
}
#[derive(Debug)]
struct FormView {
    id: String,
    page: usize,
    focus: Focus,
    offset: u16,
    follow_focus: bool,
    sending: bool,
    error: Option<String>,
}
#[derive(Debug)]
enum Dialog {
    Form(FormView),
    History { cursor: usize, offset: usize },
}
#[derive(Debug, Default)]
pub(super) struct State {
    records: Vec<Record>,
    pending: Vec<String>,
    dismissed: HashSet<String>,
    drafts: HashMap<String, Vec<Draft>>,
    dialog: Option<Dialog>,
    hits: Vec<(Rect, Focus)>,
    hovered: Option<Focus>,
    pointer_active: bool,
    pointer_position: Option<Position>,
    badge: Rect,
    editor_area: Rect,
    editor_rows: Vec<(Rect, Focus, usize)>,
    next: Option<i64>,
    history_requested: bool,
}

pub(super) enum Action {
    None,
    CancelTask,
    Answer {
        id: String,
        answers: Option<Vec<Answer>>,
    },
    History(Option<i64>),
}
impl State {
    pub fn visible(&self) -> bool {
        self.dialog.is_some()
    }
    pub fn sync(&mut self, pending: Vec<Record>) -> bool {
        let removed = self
            .pending
            .iter()
            .any(|id| !pending.iter().any(|r| &r.id == id));
        self.pending = pending.iter().map(|r| r.id.clone()).collect();
        for r in pending {
            self.upsert(r);
        }
        // Closed requests are disabled immediately, before their final records arrive.
        if let Some(Dialog::Form(v)) = &mut self.dialog
            && !self.pending.contains(&v.id)
            && self
                .records
                .iter()
                .any(|r| r.id == v.id && r.status == Status::Pending)
        {
            v.sending = true;
        }
        removed
    }
    fn upsert(&mut self, record: Record) {
        if record.status != Status::Pending {
            let was_pending = self
                .records
                .iter()
                .any(|r| r.id == record.id && r.status == Status::Pending);
            if was_pending && matches!(&self.dialog, Some(Dialog::Form(v)) if v.id == record.id) {
                self.dialog = None;
                self.clear_geometry();
            }
            self.pending.retain(|id| id != &record.id);
        }
        if !self.drafts.contains_key(&record.id) || record.status != Status::Pending {
            self.drafts.insert(
                record.id.clone(),
                record
                    .form
                    .questions
                    .iter()
                    .map(|q| {
                        let mut d = Draft::default();
                        if let Some(a) = record.answers.iter().find(|a| a.question_id == q.id) {
                            d.selected = a.selected.clone();
                            d.custom = !a.text.is_empty();
                            d.text.set(a.text.clone());
                        }
                        d
                    })
                    .collect(),
            );
        }
        if let Some(old) = self.records.iter_mut().find(|r| r.id == record.id) {
            *old = record;
        } else {
            self.records.push(record);
        }
    }
    pub fn receive(&mut self, history: History) {
        let cursor_id = match &self.dialog {
            Some(Dialog::History { cursor, .. }) => self.records.get(*cursor).map(|r| r.id.clone()),
            _ => None,
        };
        let ordering = history
            .records
            .iter()
            .map(|r| r.id.clone())
            .collect::<Vec<_>>();
        let newest = history.before.is_none();
        self.next = history.next;
        for r in history.records {
            self.upsert(r);
        }
        if newest {
            self.records.sort_by_key(|r| {
                ordering
                    .iter()
                    .position(|id| id == &r.id)
                    .unwrap_or(usize::MAX)
            });
        }
        if let Some(id) = cursor_id
            && let Some(Dialog::History { cursor, .. }) = &mut self.dialog
        {
            *cursor = self.records.iter().position(|r| r.id == id).unwrap_or(0);
        }
        if self.history_requested {
            self.dialog = Some(Dialog::History {
                cursor: 0,
                offset: 0,
            });
            self.history_requested = false;
        }
    }
    pub fn history(&mut self) {
        self.history_requested = true;
    }
    pub fn open_next(&mut self) {
        if self.dialog.is_none()
            && let Some(id) = self
                .pending
                .iter()
                .find(|id| !self.dismissed.contains(*id))
                .cloned()
        {
            self.open(&id);
        }
    }
    pub fn open_tool(&mut self, call_id: &str, output: &str) -> bool {
        if let Ok(record) = serde_json::from_str::<Record>(output) {
            if record.form.validate().is_err() {
                return false;
            }
            let id = record.id.clone();
            self.upsert(record);
            self.open(&id);
            return true;
        }
        if let Some(id) = self
            .records
            .iter()
            .find(|r| r.call_id == call_id && r.source.is_none() && self.pending.contains(&r.id))
            .map(|r| r.id.clone())
        {
            self.open(&id);
            true
        } else {
            false
        }
    }
    fn clear_geometry(&mut self) {
        self.hits.clear();
        self.hovered = None;
        self.editor_rows.clear();
        self.editor_area = Rect::default();
    }
    fn open(&mut self, id: &str) {
        self.clear_geometry();
        self.pointer_active = false;
        if let Some(r) = self.records.iter().find(|r| r.id == id) {
            let page = if r.status == Status::Pending {
                0
            } else {
                r.form.questions.len()
            };
            let focus = if r.status != Status::Pending {
                Focus::Later
            } else if r.form.questions[0].kind == Kind::Text {
                Focus::Text
            } else {
                Focus::Option(0)
            };
            self.dialog = Some(Dialog::Form(FormView {
                id: id.into(),
                page,
                focus,
                offset: 0,
                follow_focus: true,
                sending: false,
                error: None,
            }));
        }
    }
    pub fn submission_succeeded(&mut self, id: &str) {
        self.dismissed.insert(id.into());
        self.pending.retain(|pending| pending != id);
        if matches!(&self.dialog, Some(Dialog::Form(v)) if v.id == id && v.sending) {
            self.dialog = None;
            self.clear_geometry();
        }
    }
    pub fn submission_error(&mut self, id: &str, error: String) {
        if let Some(Dialog::Form(v)) = &mut self.dialog
            && v.id == id
        {
            v.sending = false;
            v.error = Some(error);
        }
    }
    fn answers(&self, id: &str) -> Vec<Answer> {
        let r = self.records.iter().find(|r| r.id == id).unwrap();
        r.form
            .questions
            .iter()
            .zip(&self.drafts[id])
            .map(|(q, d)| Answer {
                question_id: q.id.clone(),
                selected: d.selected.clone(),
                text: if q.kind == Kind::Text || d.custom {
                    d.text.text.clone()
                } else {
                    String::new()
                },
                note: String::new(),
            })
            .collect()
    }
    fn controls(&self, v: &FormView) -> Vec<Focus> {
        let r = self.records.iter().find(|r| r.id == v.id).unwrap();
        let mut result = Vec::new();
        if v.page < r.form.questions.len() && r.status == Status::Pending {
            let q = &r.form.questions[v.page];
            let d = &self.drafts[&v.id][v.page];
            result.extend((0..q.options.len()).map(Focus::Option));
            if q.kind == Kind::Text || d.custom {
                result.push(Focus::Text);
            } else if q.allow_custom {
                result.push(Focus::Option(q.options.len()));
            }
        }
        if v.page > 0 {
            result.push(Focus::Previous);
        }
        if v.page < r.form.questions.len() || r.status == Status::Pending {
            result.push(Focus::Next);
        }
        result.push(Focus::Later);
        if r.status == Status::Pending {
            result.push(Focus::Decline);
        }
        result
    }
    fn activate(&mut self, focus: Focus) -> Action {
        if let Focus::History(i) = focus {
            if let Some(id) = self.records.get(i).map(|r| r.id.clone()) {
                self.open(&id);
            }
            return Action::None;
        }
        if focus == Focus::More {
            return Action::History(self.next);
        }
        self.clear_geometry();
        let Some(Dialog::Form(mut v)) = self.dialog.take() else {
            return Action::None;
        };
        let r = self.records.iter().find(|r| r.id == v.id).unwrap().clone();
        let editable = r.status == Status::Pending && self.pending.contains(&r.id) && !v.sending;
        let mut action = Action::None;
        let previous_page = v.page;
        match focus {
            Focus::Later => {
                self.dismissed.insert(v.id);
                return action;
            }
            Focus::Tab(i) => {
                v.page = i;
                v.offset = 0;
                v.focus = Focus::Next;
            }
            Focus::Previous => {
                v.page = v.page.saturating_sub(1);
                v.offset = 0;
                v.focus = Focus::Next;
            }
            Focus::Next if v.page < r.form.questions.len() => {
                v.page += 1;
                v.offset = 0;
                v.focus = Focus::Next;
            }
            Focus::Next if r.status != Status::Pending => {
                return action;
            }
            Focus::Next if editable => {
                let answers = self.answers(&v.id);
                match r.form.validate_answers(&answers) {
                    Ok(()) => {
                        v.sending = true;
                        v.error = None;
                        action = Action::Answer {
                            id: v.id.clone(),
                            answers: Some(answers),
                        };
                    }
                    Err(error) => {
                        v.error = Some(error);
                        if let Some(i) = r.form.questions.iter().zip(&answers).position(|(q, a)| {
                            q.required && a.selected.is_empty() && a.text.trim().is_empty()
                        }) {
                            v.page = i;
                            v.offset = 0;
                        }
                    }
                }
            }
            Focus::Decline if editable => {
                v.sending = true;
                action = Action::Answer {
                    id: v.id.clone(),
                    answers: None,
                };
            }
            Focus::Option(i) if editable && v.page < r.form.questions.len() => {
                let q = &r.form.questions[v.page];
                let d = &mut self.drafts.get_mut(&v.id).unwrap()[v.page];
                if i == q.options.len() {
                    d.custom = !d.custom;
                    if d.custom {
                        if q.kind == Kind::Single {
                            d.selected.clear();
                        }
                        v.focus = Focus::Text;
                    }
                } else if let Some(o) = q.options.get(i) {
                    if q.kind == Kind::Single {
                        d.selected = vec![o.id.clone()];
                        d.custom = false;
                    } else if d.selected.contains(&o.id) {
                        d.selected.retain(|id| id != &o.id);
                    } else {
                        d.selected.push(o.id.clone());
                    }
                }
            }
            _ => {}
        }
        if v.page != previous_page || matches!(focus, Focus::Tab(_) | Focus::Previous) {
            v.focus = if v.page < r.form.questions.len() && editable {
                if r.form.questions[v.page].kind == Kind::Text || self.drafts[&v.id][v.page].custom
                {
                    Focus::Text
                } else {
                    Focus::Option(0)
                }
            } else if editable {
                Focus::Next
            } else {
                Focus::Tab(v.page)
            };
        }
        v.follow_focus = true;
        self.dialog = Some(Dialog::Form(v));
        action
    }
    pub fn handle(&mut self, event: &Event) -> Action {
        if self.dialog.is_none() {
            if let Event::Mouse(m) = event
                && m.kind == MouseEventKind::Down(MouseButton::Left)
                && self.badge.contains(Position::new(m.column, m.row))
                && let Some(id) = self.pending.first().cloned()
            {
                self.open(&id);
            }
            return Action::None;
        }
        if let Event::Mouse(m) = event {
            let pos = Position::new(m.column, m.row);
            match m.kind {
                MouseEventKind::Moved => {
                    // Some terminals emit stationary motion after a click. A
                    // new page must retain its editor focus at those coordinates.
                    if self.pointer_position != Some(pos) {
                        self.pointer_position = Some(pos);
                        self.pointer_active = true;
                        self.hovered = self
                            .hits
                            .iter()
                            .find(|(rect, _)| rect.contains(pos))
                            .map(|(_, focus)| *focus);
                        if let (Some(Dialog::History { cursor, .. }), Some(Focus::History(i))) =
                            (&mut self.dialog, self.hovered)
                        {
                            *cursor = i;
                        }
                    }
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    self.pointer_position = Some(pos);
                    self.pointer_active = true;
                    self.hovered = None;
                    if let Some((rect, _, row)) = self
                        .editor_rows
                        .iter()
                        .find(|(rect, _, _)| rect.contains(pos))
                        .copied()
                        && let Some(Dialog::Form(v)) = &mut self.dialog
                    {
                        let editor = &mut self.drafts.get_mut(&v.id).unwrap()[v.page].text;
                        let index = editor.position_at(
                            (m.column - rect.x) as usize,
                            row,
                            rect.width as usize,
                        );
                        editor.set_cursor(index);
                        v.focus = Focus::Text;
                        v.follow_focus = true;
                        return Action::None;
                    }
                    if let Some((_, focus)) = self
                        .hits
                        .iter()
                        .find(|(rect, _)| rect.contains(pos))
                        .copied()
                    {
                        let advance = match &self.dialog {
                            Some(Dialog::Form(v)) => self.records.iter().find(|r| r.id == v.id).is_some_and(|r| {
                                r.status == Status::Pending && !v.sending && self.pending.contains(&v.id)
                                    && r.form.questions.get(v.page).is_some_and(|q| {
                                        q.kind == Kind::Single && matches!(focus, Focus::Option(i) if i < q.options.len())
                                    })
                            }),
                            _ => false,
                        };
                        if let Some(Dialog::Form(v)) = &mut self.dialog {
                            v.focus = focus;
                        }
                        let action = self.activate(focus);
                        return if advance {
                            self.activate(Focus::Next)
                        } else {
                            action
                        };
                    }
                }
                MouseEventKind::ScrollDown | MouseEventKind::ScrollUp => {
                    self.hovered = None;
                    self.pointer_active = true;
                    let down = m.kind == MouseEventKind::ScrollDown;
                    match &mut self.dialog {
                        Some(Dialog::Form(v)) => {
                            v.offset = if down {
                                v.offset.saturating_add(3)
                            } else {
                                v.offset.saturating_sub(3)
                            };
                            v.follow_focus = false;
                        }
                        Some(Dialog::History { offset, .. }) => {
                            *offset = if down {
                                offset
                                    .saturating_add(3)
                                    .min(self.records.len().saturating_sub(1))
                            } else {
                                offset.saturating_sub(3)
                            };
                        }
                        _ => {}
                    }
                }
                // Release and drag events belong to the original press; they
                // must never focus or activate controls laid out after it.
                _ => {}
            }
            return Action::None;
        }
        if matches!(event, Event::Paste(_))
            || matches!(event, Event::Key(key) if key.kind != KeyEventKind::Release)
        {
            self.pointer_active = false;
            self.hovered = None;
            self.pointer_position = None;
        }
        let key = match event {
            Event::Key(k) if k.kind == KeyEventKind::Release => return Action::None,
            Event::Key(k) => Some(k),
            _ => None,
        };
        if key.is_some_and(|k| {
            k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL)
        }) {
            return Action::CancelTask;
        }
        if key.is_some_and(|k| k.code == KeyCode::Esc) {
            self.clear_geometry();
            if let Some(Dialog::Form(v)) = self.dialog.take() {
                self.dismissed.insert(v.id);
            } else {
                self.dialog = None;
            }
            return Action::None;
        }
        if let Some(Dialog::History { cursor, offset }) = &mut self.dialog {
            if let Some(k) = key {
                match k.code {
                    KeyCode::Up => *cursor = cursor.saturating_sub(1),
                    KeyCode::Down => {
                        *cursor = (*cursor + 1).min(self.records.len().saturating_sub(1))
                    }
                    KeyCode::Enter => {
                        let i = *cursor;
                        return self.activate(Focus::History(i));
                    }
                    KeyCode::Char('n') if self.next.is_some() => return Action::History(self.next),
                    _ => {}
                }
                if *cursor < *offset {
                    *offset = *cursor;
                }
            }
            return Action::None;
        }
        let Some(Dialog::Form(v)) = &self.dialog else {
            return Action::None;
        };
        let controls = self.controls(v);
        let record = self.records.iter().find(|r| r.id == v.id).unwrap();
        let count = record.form.questions.len();
        let readonly = record.status != Status::Pending;
        let single = record
            .form
            .questions
            .get(v.page)
            .is_some_and(|q| q.kind == Kind::Single);
        let custom_option = record.form.questions.get(v.page).is_some_and(|q| {
            q.kind != Kind::Text && q.allow_custom && v.focus == Focus::Option(q.options.len())
        });
        let Some(Dialog::Form(v)) = &mut self.dialog else {
            return Action::None;
        };
        v.follow_focus = true;
        if let Some(k) = key {
            if matches!(k.code, KeyCode::Tab | KeyCode::BackTab) {
                let backward =
                    k.code == KeyCode::BackTab || k.modifiers.contains(KeyModifiers::SHIFT);
                let page = (v.page + if backward { count } else { 1 }) % (count + 1);
                return self.activate(Focus::Tab(page));
            }
            if matches!(k.code, KeyCode::PageUp | KeyCode::PageDown)
                || (readonly && matches!(k.code, KeyCode::Up | KeyCode::Down))
            {
                let amount = if matches!(k.code, KeyCode::PageUp | KeyCode::PageDown) {
                    8
                } else {
                    1
                };
                v.offset = if matches!(k.code, KeyCode::Up | KeyCode::PageUp) {
                    v.offset.saturating_sub(amount)
                } else {
                    v.offset.saturating_add(amount)
                };
                v.follow_focus = false;
                return Action::None;
            }
            if k.code == KeyCode::Enter
                && !k
                    .modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL)
            {
                if let Focus::Option(i) = v.focus {
                    if custom_option && self.drafts[&v.id][v.page].custom {
                        v.focus = Focus::Text;
                        return Action::None;
                    }
                    if single || custom_option {
                        self.activate(Focus::Option(i));
                        if matches!(&self.dialog, Some(Dialog::Form(v)) if v.focus == Focus::Text) {
                            return Action::None;
                        }
                    }
                    return self.activate(Focus::Next);
                }
                let focus = if matches!(v.focus, Focus::Text) {
                    Focus::Next
                } else {
                    v.focus
                };
                return self.activate(focus);
            }
            if !matches!(v.focus, Focus::Text) {
                match k.code {
                    KeyCode::Char(' ') => {
                        let f = v.focus;
                        return self.activate(f);
                    }
                    KeyCode::Up | KeyCode::Down => {
                        let i = controls.iter().position(|f| f == &v.focus).unwrap_or(0);
                        v.focus = controls[if k.code == KeyCode::Up {
                            i.saturating_sub(1)
                        } else {
                            (i + 1).min(controls.len() - 1)
                        }];
                    }
                    _ => {}
                }
                return Action::None;
            }
        }
        let record = self.records.iter().find(|r| r.id == v.id).unwrap();
        if record.status == Status::Pending
            && self.pending.contains(&v.id)
            && !v.sending
            && v.page < record.form.questions.len()
        {
            let d = &mut self.drafts.get_mut(&v.id).unwrap()[v.page];
            let editor = &mut d.text;
            if let Some(k) = key
                && k.modifiers.is_empty()
                && ((k.code == KeyCode::Up && editor.cursor == 0)
                    || (k.code == KeyCode::Down && editor.cursor == editor.text.len()))
            {
                let i = controls.iter().position(|f| f == &v.focus).unwrap_or(0);
                v.focus = controls[if k.code == KeyCode::Up {
                    i.saturating_sub(1)
                } else {
                    (i + 1).min(controls.len() - 1)
                }];
                return Action::None;
            }
            let limit = 16384;
            let previous = editor.clone();
            editor.edit(event, self.editor_area.width.max(1) as usize);
            if editor.text.len() > limit {
                *editor = previous;
                v.error = Some("文字已达到长度上限".into());
            }
        }
        Action::None
    }
}
