//! The durable inbox is authoritative; short local sending previews are not.
use super::{Action, Panel};
use crate::{
    tui::{components, input::InputState, render::safe_output, theme::THEME},
    workspace::protocol::{Request, Response},
};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use orbit_protocol::input_queue::{
    Action as QueueAction, Entry, Snapshot, State as QueueState, valid_entry, valid_page,
};
use ratatui::{Frame, layout::Rect, style::Style, widgets::Paragraph};

#[derive(Debug)]
enum Pending {
    List(Option<i64>),
    Edit,
    Remove,
    Steer,
}

#[derive(Debug)]
struct Editor {
    entry: Entry,
    input: InputState,
    offset: usize,
    stale: bool,
    follow: bool,
}

#[derive(Clone, Copy, Debug)]
enum Hit {
    Row(usize),
    Refresh,
    More,
    Edit,
    Remove,
    Steer,
    Copy,
    Back,
    Save,
}

#[derive(Debug)]
pub(super) struct State {
    session: String,
    workspace: String,
    entries: Vec<Entry>,
    next: Option<i64>,
    run: Option<i64>,
    selected: usize,
    offset: usize,
    editor: Option<Editor>,
    pending: Option<Pending>,
    removing: bool,
    error: Option<String>,
    message: Option<String>,
    body: Rect,
    hits: Vec<(Rect, Hit)>,
}

impl State {
    pub(super) fn new(session: String, workspace: String) -> Self {
        Self {
            session,
            workspace,
            entries: Vec::new(),
            next: None,
            run: None,
            selected: 0,
            offset: 0,
            editor: None,
            pending: None,
            removing: false,
            error: None,
            message: None,
            body: Rect::default(),
            hits: Vec::new(),
        }
    }

    pub(super) fn initial(&mut self) -> Request {
        self.pending = Some(Pending::List(None));
        self.request(QueueAction::List { after: None })
    }

    fn request(&self, action: QueueAction) -> Request {
        Request::InputQueue {
            session: self.session.clone(),
            expected_workspace: self.workspace.clone(),
            action,
        }
    }

    fn start(&mut self, pending: Pending, action: QueueAction) -> Action {
        if self.pending.is_some() {
            return Action::None;
        }
        self.pending = Some(pending);
        self.error = None;
        self.message = None;
        self.removing = false;
        self.invalidate_geometry();
        Action::Request(self.request(action))
    }

    fn edit(&mut self) {
        let Some(entry) = self.entries.get(self.selected).cloned() else {
            return;
        };
        // Queue CAS samples the complete stored bytes, including whitespace.
        let mut input = InputState::default();
        input.text = entry.content.clone();
        input.cursor = entry.content.len();
        self.editor = Some(Editor {
            entry,
            input,
            offset: 0,
            stale: false,
            follow: true,
        });
        self.error = None;
        self.invalidate_geometry();
    }

    fn save(&mut self) -> Action {
        let Some(editor) = &self.editor else {
            return Action::None;
        };
        if !editor.entry.editable() || editor.stale {
            return Action::None;
        }
        let action = QueueAction::Edit {
            id: editor.entry.id,
            expected: editor.entry.content.clone(),
            content: editor.input.text.clone(),
            run: self.run,
        };
        self.start(Pending::Edit, action)
    }

    fn remove(&mut self) -> Action {
        let entry = self
            .editor
            .as_ref()
            .map(|editor| &editor.entry)
            .or_else(|| self.entries.get(self.selected));
        let Some(entry) = entry.filter(|entry| entry.editable()) else {
            return Action::None;
        };
        if self
            .editor
            .as_ref()
            .is_some_and(|editor| editor.stale)
        {
            return Action::None;
        }
        if !self.removing {
            self.removing = true;
            self.message = Some(format!("移除待发送消息 #{}？Y 确认，N 取消。", entry.id));
            return Action::None;
        }
        let action = QueueAction::Remove {
            id: entry.id,
            expected: entry.content.clone(),
            run: self.run,
        };
        self.start(Pending::Remove, action)
    }

    fn steer(&mut self) -> Action {
        let entry = self
            .editor
            .as_ref()
            .map(|editor| &editor.entry)
            .or_else(|| self.entries.get(self.selected));
        let Some(entry) = entry.filter(|entry| {
            entry.state == QueueState::Queued
                && entry.delivery == orbit_protocol::input_queue::Delivery::NextRun
        }) else {
            return Action::None;
        };
        self.start(
            Pending::Steer,
            QueueAction::Steer {
                id: entry.id,
                expected: entry.content.clone(),
                run: self.run,
            },
        )
    }

    fn activate(&mut self, hit: Hit) -> Action {
        if self.pending.is_some() {
            return Action::None;
        }
        match hit {
            Hit::Row(index) => {
                self.selected = index;
                self.edit();
                Action::None
            }
            Hit::Edit => {
                self.edit();
                Action::None
            }
            Hit::Save => self.save(),
            Hit::Steer => self.steer(),
            Hit::Remove => self.remove(),
            Hit::Copy => self
                .editor
                .as_ref()
                .map(|e| &e.input.text)
                .or_else(|| {
                    self.entries
                        .get(self.selected)
                        .map(|e| &e.content)
                })
                .map(|text| Action::Copy(text.clone()))
                .unwrap_or(Action::None),
            Hit::Refresh => self.start(Pending::List(None), QueueAction::List { after: None }),
            Hit::More => {
                let Some(after) = self.next else {
                    return Action::None;
                };
                self.start(
                    Pending::List(Some(after)),
                    QueueAction::List { after: Some(after) },
                )
            }
            Hit::Back => {
                if self
                    .editor
                    .as_ref()
                    .is_some_and(|e| e.input.text != e.entry.content)
                {
                    self.message = Some("草稿已保留；再次输入 /queue 可继续编辑。".into());
                    Action::Close
                } else if self.editor.take().is_some() {
                    self.invalidate_geometry();
                    Action::None
                } else {
                    Action::Close
                }
            }
        }
    }
}

impl Panel for State {
    fn handle(&mut self, event: &Event) -> Action {
        if let Event::Key(key) = event {
            if key.kind == KeyEventKind::Release {
                return Action::None;
            }
            if key.code == KeyCode::Esc {
                self.removing = false;
                return Action::Close;
            }
            if self.pending.is_some() {
                return Action::None;
            }
            if self.removing {
                match key.code {
                    KeyCode::Char('y' | 'Y') => return self.remove(),
                    KeyCode::Char('n' | 'N') => {
                        self.removing = false;
                        self.message = None;
                    }
                    _ => {}
                }
                return Action::None;
            }
            if key
                .modifiers
                .contains(KeyModifiers::CONTROL)
            {
                match key.code {
                    KeyCode::Char('s') => return self.save(),
                    KeyCode::Char('r') => return self.activate(Hit::Refresh),
                    KeyCode::Char('y') => return self.activate(Hit::Copy),
                    KeyCode::Char('g') => return self.steer(),
                    KeyCode::Char('p') => {
                        if let Some(editor) = &self.editor {
                            return Action::Draft(editor.input.text.clone());
                        }
                    }
                    KeyCode::Char('b') => return self.activate(Hit::Back),
                    _ => {}
                }
            }
            if let Some(editor) = &mut self.editor {
                if editor.entry.editable() {
                    if key.code == KeyCode::Delete && key.modifiers == KeyModifiers::ALT {
                        return self.remove();
                    }
                    editor.follow = true;
                    if key.code == KeyCode::Enter && key.modifiers.is_empty() {
                        editor.input.insert("\n");
                    } else {
                        editor
                            .input
                            .edit(event, self.body.width.max(1) as usize);
                    }
                } else {
                    match key.code {
                        KeyCode::Up => editor.offset = editor.offset.saturating_sub(1),
                        KeyCode::Down => editor.offset = editor.offset.saturating_add(1),
                        _ => {}
                    }
                }
            } else {
                match key.code {
                    KeyCode::Up => self.selected = self.selected.saturating_sub(1),
                    KeyCode::Down => {
                        self.selected =
                            (self.selected + 1).min(self.entries.len().saturating_sub(1))
                    }
                    KeyCode::Enter | KeyCode::Char('e') => self.edit(),
                    KeyCode::Delete => return self.remove(),
                    KeyCode::Char('r') => return self.activate(Hit::Refresh),
                    KeyCode::Char('n') => return self.activate(Hit::More),
                    KeyCode::Char('y') => return self.activate(Hit::Copy),
                    _ => {}
                }
            }
        } else if let Event::Paste(text) = event {
            if self.pending.is_none()
                && let Some(editor) = &mut self.editor
                && editor.entry.editable()
            {
                editor.input.insert(text);
                editor.follow = true;
            }
        } else if let Event::Mouse(mouse) = event {
            let point = (mouse.column, mouse.row).into();
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some((_, hit)) = self
                        .hits
                        .iter()
                        .find(|(area, _)| area.contains(point))
                    {
                        return self.activate(*hit);
                    }
                }
                MouseEventKind::ScrollDown | MouseEventKind::ScrollUp
                    if self.body.contains(point) =>
                {
                    let down = mouse.kind == MouseEventKind::ScrollDown;
                    if let Some(editor) = &mut self.editor {
                        editor.offset = if down {
                            editor.offset.saturating_add(3)
                        } else {
                            editor.offset.saturating_sub(3)
                        };
                    } else {
                        self.selected = if down {
                            self.selected
                                .saturating_add(3)
                                .min(self.entries.len().saturating_sub(1))
                        } else {
                            self.selected.saturating_sub(3)
                        };
                    }
                }
                _ => {}
            }
        }
        Action::None
    }

    fn receive(&mut self, response: Response) -> Action {
        let pending = self.pending.take();
        let mutation = matches!(pending.as_ref(), Some(Pending::Edit | Pending::Remove));
        match (pending, response) {
            (
                Some(Pending::List(after)),
                Response::InputQueue(Snapshot::List { entries, next, run }),
            ) if valid_page(after, &entries, next)
                && entries
                    .iter()
                    .all(|entry| valid_entry(entry, run)) =>
            {
                if after.is_none() {
                    self.entries.clear();
                    self.offset = 0;
                }
                for entry in entries {
                    if let Some(old) = self
                        .entries
                        .iter_mut()
                        .find(|old| old.id == entry.id)
                    {
                        *old = entry;
                    } else {
                        self.entries.push(entry);
                    }
                }
                self.next = next;
                self.run = run;
                self.selected = self
                    .selected
                    .min(self.entries.len().saturating_sub(1));
                if let Some(editor) = &mut self.editor {
                    if let Some(entry) = self
                        .entries
                        .iter()
                        .find(|entry| entry.id == editor.entry.id)
                    {
                        editor.stale = entry.content != editor.entry.content;
                        editor.entry.state = entry.state;
                        if editor.stale {
                            self.error = Some(
                                "服务端内容已变化。草稿已保留；请复制后返回列表重新读取。".into(),
                            );
                        }
                    } else {
                        editor.stale = true;
                        self.error =
                            Some("该消息已离开队列。草稿已保留，可复制后重新提交。".into());
                    }
                }
            }
            (Some(Pending::Edit), Response::InputQueue(Snapshot::Edited { entry })) => {
                if let Some(old) = self
                    .entries
                    .iter_mut()
                    .find(|old| old.id == entry.id)
                {
                    *old = entry.clone();
                }
                if let Some(editor) = &mut self.editor
                    && editor.entry.id == entry.id
                {
                    editor.entry = entry;
                    editor.stale = false;
                }
                self.message = Some("已更新待发送消息。".into());
            }
            (Some(Pending::Steer), Response::InputQueue(Snapshot::Steered { id })) => {
                if let Some(entry) = self
                    .entries
                    .iter_mut()
                    .find(|entry| entry.id == id)
                {
                    entry.delivery = orbit_protocol::input_queue::Delivery::AtBoundary;
                }
                if let Some(editor) = self
                    .editor
                    .as_mut()
                    .filter(|editor| editor.entry.id == id)
                {
                    editor.entry.delivery = orbit_protocol::input_queue::Delivery::AtBoundary;
                }
                self.message = Some("将在合适的输入边界引导当前任务。".into());
            }
            (Some(Pending::Remove), Response::InputQueue(Snapshot::Removed { id })) => {
                self.entries
                    .retain(|entry| entry.id != id);
                if self
                    .editor
                    .as_ref()
                    .is_some_and(|e| e.entry.id == id)
                {
                    self.editor = None;
                }
                self.selected = self
                    .selected
                    .min(self.entries.len().saturating_sub(1));
                self.message = Some("已移除待发送消息。".into());
            }
            (_, Response::Error(error)) => self.error = Some(error),
            _ => {
                if mutation && let Some(editor) = &mut self.editor {
                    editor.stale = true;
                }
                self.error = Some("队列响应不匹配。草稿已保留，请刷新确认服务端状态。".into());
            }
        }
        self.invalidate_geometry();
        Action::None
    }

    fn error(&mut self, error: String) {
        if matches!(self.pending.take(), Some(Pending::Edit | Pending::Remove))
            && let Some(editor) = &mut self.editor
        {
            editor.stale = true;
        }
        self.error = Some(format!("{error}；草稿已保留，请刷新确认。"));
    }

    fn draw(&mut self, frame: &mut Frame<'_>, area: Rect) {
        self.hits.clear();
        if area.width < 12 || area.height < 6 {
            frame.render_widget(Paragraph::new("请扩大终端查看输入队列"), area);
            return;
        }
        frame.render_widget(
            Paragraph::new(if self.pending.is_some() {
                "输入队列 · 正在读取…"
            } else {
                "输入队列 · 服务端待发送消息"
            })
            .style(components::accent()),
            Rect::new(area.x, area.y, area.width, 1),
        );
        let actions = Rect::new(area.x, area.y + 1, area.width, 2);
        let mut bar = components::ActionBar::new(actions);
        let disabled = self.pending.is_some();
        let editable = self
            .editor
            .as_ref()
            .is_some_and(|e| e.entry.editable() && !e.stale);
        for (label, hit, unavailable) in [
            ("返回 Ctrl+B", Hit::Back, false),
            ("刷新 Ctrl+R", Hit::Refresh, false),
            ("下一页 N", Hit::More, self.next.is_none()),
            (
                "编辑 Enter",
                Hit::Edit,
                self.editor.is_some() || self.entries.is_empty(),
            ),
            ("保存 Ctrl+S", Hit::Save, !editable),
            (
                "引导 Ctrl+G",
                Hit::Steer,
                !self
                    .editor
                    .as_ref()
                    .map(|editor| &editor.entry)
                    .or_else(|| self.entries.get(self.selected))
                    .is_some_and(|entry| {
                        entry.state == QueueState::Queued
                            && entry.delivery == orbit_protocol::input_queue::Delivery::NextRun
                    }),
            ),
            (
                "移除 Alt+Del",
                Hit::Remove,
                !self
                    .editor
                    .as_ref()
                    .map(|e| e.entry.editable() && !e.stale)
                    .unwrap_or_else(|| {
                        self.entries
                            .get(self.selected)
                            .is_some_and(Entry::editable)
                    }),
            ),
            ("复制 Ctrl+Y", Hit::Copy, false),
        ] {
            if let Some(rect) = bar.button(
                frame,
                label,
                components::Control {
                    disabled: disabled || unavailable,
                    ..Default::default()
                },
            ) {
                self.hits.push((rect, hit));
            }
        }
        self.body = Rect::new(
            area.x,
            area.y + 3,
            area.width,
            area.height.saturating_sub(5),
        );
        if let Some(editor) = &mut self.editor {
            super::resources::editor_area(
                frame,
                self.body,
                &editor.input,
                &mut editor.offset,
                editor.entry.editable() && !disabled,
                std::mem::take(&mut editor.follow),
            );
        } else {
            if self.selected < self.offset {
                self.offset = self.selected;
            }
            self.offset = self.offset.min(
                self.entries
                    .len()
                    .saturating_sub(self.body.height as usize),
            );
            if self.selected >= self.offset + self.body.height as usize {
                self.offset = self.selected + 1 - self.body.height as usize;
            }
            if self.entries.is_empty() {
                frame.render_widget(
                    Paragraph::new("没有待发送消息。任务运行时发送的文本会在这里排队。"),
                    self.body,
                );
            }
            for (row, (index, entry)) in self
                .entries
                .iter()
                .enumerate()
                .skip(self.offset)
                .take(self.body.height as usize)
                .enumerate()
            {
                let rect = Rect::new(self.body.x, self.body.y + row as u16, self.body.width, 1);
                let state = match entry.state {
                    QueueState::Queued => {
                        if entry.delivery == orbit_protocol::input_queue::Delivery::NextRun {
                            "本次结束后发送"
                        } else {
                            "等待引导"
                        }
                    }
                    QueueState::Claimed => "已接收",
                    QueueState::Finishing => "收尾中",
                    QueueState::Idle => "已暂停",
                    QueueState::Unavailable => "不可编辑",
                };
                let text = format!(
                    "#{} · {}  {}",
                    entry.id,
                    state,
                    entry
                        .content
                        .replace(['\n', '\r', '\t'], " ")
                );
                frame.render_widget(
                    Paragraph::new(safe_output(&text)).style(if index == self.selected {
                        components::highlighted()
                    } else {
                        components::normal()
                    }),
                    rect,
                );
                self.hits.push((rect, Hit::Row(index)));
            }
        }
        let note = self
            .error
            .as_ref()
            .or(self.message.as_ref())
            .map(String::as_str)
            .unwrap_or("Enter 编辑 · Esc 收起并保留草稿 · 已接收的消息不能修改");
        frame.render_widget(
            Paragraph::new(safe_output(note)).style(Style::default().fg(if self.error.is_some() {
                THEME.danger
            } else {
                THEME.muted
            })),
            Rect::new(area.x, area.bottom() - 2, area.width, 2),
        );
    }

    fn invalidate_geometry(&mut self) {
        self.hits.clear();
        self.body = Rect::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> State {
        let mut state = State::new("s".into(), "/p".into());
        state.initial();
        state.receive(Response::InputQueue(Snapshot::List {
            entries: vec![Entry {
                delivery: orbit_protocol::input_queue::Delivery::AtBoundary,
                files: Vec::new(),
                images: vec![],
                id: 4,
                content: "  exact\ntext  ".into(),
                state: QueueState::Queued,
            }],
            next: Some(4),
            run: Some(7),
        }));
        state
    }

    #[test]
    fn edits_use_exact_sampled_content_and_run() {
        let mut state = state();
        state.edit();
        state
            .editor
            .as_mut()
            .unwrap()
            .input
            .insert("new");
        let Action::Request(Request::InputQueue {
            expected_workspace,
            action:
                QueueAction::Edit {
                    expected,
                    run,
                    content,
                    ..
                },
            ..
        }) = state.save()
        else {
            panic!("edit request");
        };
        assert_eq!(expected_workspace, "/p");
        assert_eq!(expected, "  exact\ntext  ");
        assert_eq!(run, Some(7));
        assert!(content.ends_with("new"));
    }

    #[test]
    fn unknown_write_keeps_draft_and_requires_refresh() {
        let mut state = state();
        state.edit();
        state
            .editor
            .as_mut()
            .unwrap()
            .input
            .insert("draft");
        state.save();
        state.error("connection lost".into());
        assert!(
            state
                .editor
                .as_ref()
                .unwrap()
                .input
                .text
                .ends_with("draft")
        );
        assert!(matches!(state.save(), Action::None));
        state.activate(Hit::Refresh);
        state.receive(Response::InputQueue(Snapshot::List {
            entries: state.entries.clone(),
            next: None,
            run: Some(8),
        }));
        assert!(matches!(
            state.save(),
            Action::Request(Request::InputQueue {
                action: QueueAction::Edit { run: Some(8), .. },
                ..
            })
        ));
    }

    #[test]
    fn claimed_entries_and_unconfirmed_removals_never_mutate() {
        let mut state = state();
        assert!(matches!(state.remove(), Action::None));
        assert!(state.removing);
        assert!(matches!(
            state.remove(),
            Action::Request(Request::InputQueue {
                action: QueueAction::Remove { id: 4, .. },
                ..
            })
        ));
        state.pending = None;
        state.entries[0].state = QueueState::Claimed;
        state.edit();
        assert!(matches!(state.save(), Action::None));
        assert!(matches!(state.remove(), Action::None));
    }
}
