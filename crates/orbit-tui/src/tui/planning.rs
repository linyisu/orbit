//! Plan review uses the same overlay, editor and theme as questionnaires.
use super::{
    components::{self, Control},
    input::InputState,
    theme::THEME,
};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use orbit_protocol::plan::{Action, State as Snapshot, Status, WorkMode};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Modifier, Style},
    text::Line,
    widgets::Paragraph,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Document,
    Feedback,
    Editor,
    Approve,
    Send,
    Close,
}
#[derive(Debug, Default)]
pub(super) struct State {
    pub snapshot: Snapshot,
    open: bool,
    feedback_page: bool,
    draft: InputState,
    focus: Option<Focus>,
    pointer: components::Pointer<Focus>,
    hits: Vec<(Rect, Focus)>,
    badge: Rect,
    editor: Rect,
    offset: usize,
    editor_offset: usize,
    sending: bool,
    error: Option<String>,
    cached: Option<(u64, u16, Vec<Line<'static>>)>,
}
impl State {
    pub(super) fn invalidate_geometry(&mut self) {
        self.hits.clear();
        self.pointer.invalidate();
        self.editor = Rect::default();
    }
    pub fn visible(&self) -> bool {
        self.open
    }
    pub fn mode(&self) -> WorkMode {
        self.snapshot.mode
    }
    pub fn open(&mut self) {
        self.open = true;
        self.error = None;
        self.feedback_page = false;
        self.focus = Some(Focus::Document);
        self.pointer.keyboard();
        self.hits.clear();
        self.offset = 0;
    }
    pub fn sync(&mut self, state: Snapshot) {
        if state.generation < self.snapshot.generation {
            return;
        }
        let changed = state.generation != self.snapshot.generation;
        if changed {
            let old_id = self.snapshot.plan.as_ref().map(|p| &p.id);
            if old_id != state.plan.as_ref().map(|p| &p.id) {
                self.draft.clear();
            }
            if self.open
                && state
                    .plan
                    .as_ref()
                    .is_some_and(|p| p.status == Status::Approved)
            {
                self.open = false;
            }
            self.hits.clear();
            self.pointer.invalidate();
        }
        self.snapshot = state;
    }
    pub fn succeeded(&mut self) {
        self.sending = false;
        self.open = false;
        self.draft.clear();
        self.hits.clear();
    }
    pub fn failed(&mut self, error: String) {
        self.sending = false;
        self.error = Some(error);
        self.open = true;
    }
    fn editable(&self) -> bool {
        self.snapshot.mode == WorkMode::Plan
            && self
                .snapshot
                .plan
                .as_ref()
                .is_some_and(|p| p.status == Status::Review)
    }
    fn activate(&mut self, focus: Focus) -> Option<Action> {
        match focus {
            Focus::Document => {
                self.feedback_page = false;
                self.focus = Some(Focus::Document);
                self.offset = 0;
                self.hits.clear();
            }
            Focus::Feedback if self.editable() => {
                self.feedback_page = true;
                self.focus = Some(Focus::Editor);
                self.pointer.invalidate();
                self.hits.clear();
            }
            Focus::Editor => self.focus = Some(Focus::Editor),
            Focus::Close => {
                self.open = false;
                self.hits.clear();
            }
            Focus::Approve if self.editable() && !self.sending => {
                self.sending = true;
                return Some(Action::Approve {
                    generation: self.snapshot.generation,
                });
            }
            Focus::Send if self.editable() && !self.sending => {
                let text = self.draft.text.trim().to_owned();
                if text.is_empty() {
                    self.error = Some("请填写修改意见".into());
                } else {
                    self.sending = true;
                    return Some(Action::Feedback {
                        generation: self.snapshot.generation,
                        text,
                    });
                }
            }
            _ => {}
        }
        None
    }
    pub fn handle(&mut self, event: &Event) -> Option<Action> {
        if !self.open {
            if let Event::Mouse(m) = event
                && m.kind == MouseEventKind::Down(MouseButton::Left)
                && self.badge.contains((m.column, m.row).into())
            {
                self.open();
            }
            return None;
        }
        if let Event::Mouse(m) = event {
            let point = (m.column, m.row).into();
            let hit = self
                .hits
                .iter()
                .find(|(r, _)| r.contains(point))
                .map(|(_, f)| *f);
            match m.kind {
                MouseEventKind::Moved => {
                    self.pointer.moved(point, &self.hits);
                }
                MouseEventKind::Down(MouseButton::Left) if !self.sending => {
                    self.pointer.press(point);
                    if let Some(focus) = hit {
                        if focus == Focus::Editor {
                            let index = self.draft.position_at(
                                (m.column - self.editor.x) as usize,
                                (m.row - self.editor.y) as usize + self.editor_offset,
                                self.editor.width as usize,
                            );
                            self.draft.set_cursor(index);
                        }
                        return self.activate(focus);
                    }
                }
                MouseEventKind::ScrollDown if !self.feedback_page => {
                    self.offset = self.offset.saturating_add(3)
                }
                MouseEventKind::ScrollUp if !self.feedback_page => {
                    self.offset = self.offset.saturating_sub(3)
                }
                _ => {}
            }
            return None;
        }
        if let Event::Key(k) = event {
            if k.kind == KeyEventKind::Release {
                return None;
            }
            self.pointer.keyboard();
            if k.code == KeyCode::Esc {
                self.open = false;
                return None;
            }
            if self.sending {
                return None;
            }
            if k.code == KeyCode::Tab || k.code == KeyCode::BackTab {
                let controls = if self.editable() {
                    if self.feedback_page {
                        vec![
                            Focus::Document,
                            Focus::Feedback,
                            Focus::Editor,
                            Focus::Send,
                            Focus::Close,
                        ]
                    } else {
                        vec![
                            Focus::Document,
                            Focus::Feedback,
                            Focus::Approve,
                            Focus::Close,
                        ]
                    }
                } else {
                    vec![Focus::Document, Focus::Close]
                };
                let i = controls
                    .iter()
                    .position(|f| Some(*f) == self.focus)
                    .unwrap_or(0);
                let step = if k.code == KeyCode::BackTab {
                    controls.len() - 1
                } else {
                    1
                };
                self.focus = Some(controls[(i + step) % controls.len()]);
                return None;
            }
            if self.feedback_page && self.focus == Some(Focus::Editor) {
                if k.code == KeyCode::Enter && k.modifiers.contains(KeyModifiers::CONTROL) {
                    return self.activate(Focus::Send);
                }
                if k.code == KeyCode::Enter {
                    self.draft.insert("\n");
                    return None;
                }
            } else {
                match k.code {
                    KeyCode::Enter if k.kind == KeyEventKind::Press => {
                        return self.focus.and_then(|f| self.activate(f));
                    }
                    KeyCode::Down | KeyCode::PageDown => {
                        self.offset =
                            self.offset
                                .saturating_add(if k.code == KeyCode::Down { 1 } else { 10 })
                    }
                    KeyCode::Up | KeyCode::PageUp => {
                        self.offset =
                            self.offset
                                .saturating_sub(if k.code == KeyCode::Up { 1 } else { 10 })
                    }
                    _ => {}
                }
            }
        }
        if self.feedback_page && self.focus == Some(Focus::Editor) && !self.sending {
            self.draft.edit(event, self.editor.width.max(1) as usize);
        }
        None
    }
    pub fn draw_badge(&mut self, frame: &mut Frame<'_>) {
        self.badge = Rect::default();
        if !self.editable() {
            return;
        }
        let area = frame.area();
        if area.height == 0 {
            return;
        }
        let text = "计划待确认 · 查看";
        self.badge = Rect::new(
            area.x + area.width.saturating_sub(42),
            area.bottom() - 1,
            24.min(area.width),
            1,
        );
        frame.render_widget(
            Paragraph::new(text).style(
                Style::default()
                    .fg(THEME.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            self.badge,
        );
    }
    fn button(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        label: &str,
        focus: Focus,
        active: bool,
    ) {
        let highlight = self.pointer.highlight(self.focus) == Some(focus);
        if let Some(area) = components::button(
            frame,
            area,
            label,
            Control {
                highlighted: highlight || active,
                disabled: self.sending,
                ..Control::default()
            },
        ) {
            self.hits.push((area, focus));
        }
    }
    pub fn draw(&mut self, frame: &mut Frame<'_>) -> Option<Position> {
        if !self.open {
            return None;
        }
        self.hits.clear();
        self.editor = Rect::default();
        let area = super::dialogs::centered_size(frame.area(), 92, 27);
        let inner = components::modal(frame, area);
        if inner.height < 9 || inner.width < 24 {
            frame.render_widget(Paragraph::new("请扩大窗口 · Esc 收起"), inner);
            return None;
        }
        let title = self.snapshot.plan.as_ref().map_or_else(
            || "Plan · 只读规划".to_owned(),
            |p| {
                format!(
                    "{} · 第 {} 版 · {}",
                    super::render::safe_output(&p.title),
                    p.revision,
                    match p.status {
                        Status::Draft => "草稿",
                        Status::Review => "待确认",
                        Status::Approved => "已批准",
                    }
                )
            },
        );
        components::title(frame, inner, title);
        self.button(
            frame,
            Rect::new(inner.x, inner.y + 1, 12, 1),
            "方案内容",
            Focus::Document,
            !self.feedback_page,
        );
        if self.editable() {
            self.button(
                frame,
                Rect::new(inner.x + 14, inner.y + 1, 12, 1),
                "修改意见",
                Focus::Feedback,
                self.feedback_page,
            );
        }
        let body = Rect::new(inner.x, inner.y + 3, inner.width, inner.height - 8);
        let mut cursor = None;
        if self.feedback_page && self.editable() {
            self.editor = body;
            self.hits.push((body, Focus::Editor));
            let (lines, caret) = self.draft.visual_lines(body.width as usize);
            let row = caret.row;
            if row < self.editor_offset {
                self.editor_offset = row;
            }
            if row >= self.editor_offset + body.height as usize {
                self.editor_offset = row.saturating_sub(body.height as usize - 1);
            }
            let text: Vec<Line<'static>> = if self.draft.text.is_empty() {
                vec![Line::styled(
                    "填写需要调整的内容…",
                    Style::default().fg(THEME.secondary),
                )]
            } else {
                lines
                    .into_iter()
                    .skip(self.editor_offset)
                    .take(body.height as usize)
                    .map(Line::raw)
                    .collect()
            };
            frame.render_widget(Paragraph::new(text), body);
            if self.focus == Some(Focus::Editor) && !self.sending {
                cursor = caret.in_viewport(body, self.editor_offset);
            }
        } else {
            let text = self.snapshot.plan.as_ref().map_or_else(|| "已进入只读规划。收起弹窗后发送任务，Agent 可以读取、搜索、调用探索子代理并向你提问。计划提交后会在这里等待确认。".into(),|p| format!("{}\n\n### 实施步骤\n{}",p.markdown,p.steps.iter().enumerate().map(|(i,s)|format!("{}. {}",i+1,s)).collect::<Vec<_>>().join("\n")));
            if self.cached.as_ref().is_none_or(|(generation, width, _)| {
                *generation != self.snapshot.generation || *width != body.width
            }) {
                let lines =
                    super::markdown::document(&super::render::safe_output(&text), body.width)
                        .into_iter()
                        .flat_map(|l| l.wrap(body.width))
                        .map(|l| l.line)
                        .collect();
                self.cached = Some((self.snapshot.generation, body.width, lines));
            }
            let lines = &self.cached.as_ref().unwrap().2;
            self.offset = self
                .offset
                .min(lines.len().saturating_sub(body.height as usize));
            frame.render_widget(
                Paragraph::new(
                    lines
                        .iter()
                        .skip(self.offset)
                        .take(body.height as usize)
                        .cloned()
                        .collect::<Vec<_>>(),
                ),
                body,
            );
        }
        let mut bar =
            components::ActionBar::new(Rect::new(inner.x, inner.bottom() - 3, inner.width, 1));
        let buttons = if self.editable() {
            if self.feedback_page {
                vec![("提交意见", Focus::Send), ("收起", Focus::Close)]
            } else {
                vec![
                    ("修改意见", Focus::Feedback),
                    ("开始执行", Focus::Approve),
                    ("收起", Focus::Close),
                ]
            }
        } else {
            vec![("关闭", Focus::Close)]
        };
        for (label, focus) in buttons {
            if let Some(area) = bar.button(
                frame,
                label,
                Control {
                    highlighted: self.pointer.highlight(self.focus) == Some(focus),
                    disabled: self.sending,
                    ..Control::default()
                },
            ) {
                self.hits.push((area, focus));
            }
        }
        let hint = if self.sending {
            "正在提交…"
        } else if self.feedback_page {
            "Enter 换行 · Ctrl+Enter 提交意见 · Tab 切换 · Esc 收起"
        } else {
            "↑↓ 滚动 · Tab 切换 · Enter 确认按钮 · Esc 收起"
        };
        frame.render_widget(
            Paragraph::new(hint).style(Style::default().fg(THEME.secondary)),
            Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
        );
        if let Some(error) = &self.error {
            frame.render_widget(
                Paragraph::new(super::render::safe_output(error))
                    .style(Style::default().fg(THEME.tab_error)),
                Rect::new(inner.x, inner.bottom() - 4, inner.width, 1),
            );
        }
        if let Some(position) = cursor {
            frame.set_cursor_position(position);
        }
        cursor
    }
}

#[cfg(test)]
mod tests;
