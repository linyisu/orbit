//! Project tasks use real task IDs and preserve unsaved fields across service failures.
use super::resources::{editor_area, field, row};
use super::{Action, Panel};
use crate::{
    tui::{components, connect_custom::form, input::InputState, render::safe_output, theme::THEME},
    workspace::protocol::{Request, Response},
};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use orbit_protocol::project::{ProjectTask, TaskAction, TaskStatus};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::Style,
    widgets::{Paragraph, Wrap},
};
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug)]
enum Pending {
    List,
    Save,
    Status,
    Release,
}

#[derive(Debug)]
struct Editor {
    id: Option<i64>,
    title: InputState,
    details: InputState,
    priority: InputState,
    dependencies: BTreeSet<i64>,
    focus: usize,
    offset: usize,
    follow: bool,
    choosing: bool,
    dependency_cursor: usize,
    dependency_offset: usize,
}

#[derive(Clone, Copy, Debug)]
enum Hit {
    Row(usize),
    Search,
    New,
    Edit,
    Refresh,
    Completed,
    Status,
    Release,
    Save,
    Back,
    Field(usize),
    Dependency(usize),
    DependencyDone,
    StatusChoice(TaskStatus),
}

#[derive(Debug)]
pub(super) struct State {
    session: String,
    workspace: String,
    tasks: Vec<ProjectTask>,
    selected: Option<i64>,
    offset: usize,
    query: InputState,
    searching: bool,
    show_completed: bool,
    editor: Option<Editor>,
    choosing_status: Option<usize>,
    pending: Option<Pending>,
    error: Option<String>,
    message: Option<String>,
    hits: Vec<(Rect, Hit)>,
    body_area: Rect,
    details_area: Rect,
}

impl State {
    pub(super) fn new(session: String, workspace: String) -> Self {
        Self {
            session,
            workspace,
            tasks: Vec::new(),
            selected: None,
            offset: 0,
            query: InputState::default(),
            searching: false,
            show_completed: false,
            editor: None,
            choosing_status: None,
            pending: None,
            error: None,
            message: None,
            hits: Vec::new(),
            body_area: Rect::default(),
            details_area: Rect::default(),
        }
    }

    pub(super) fn initial(&mut self) -> Request {
        self.pending = Some(Pending::List);
        self.request(TaskAction::List {
            include_completed: true,
        })
    }

    fn request(&self, action: TaskAction) -> Request {
        Request::Tasks {
            session: self.session.clone(),
            expected_workspace: Some(self.workspace.clone()),
            action,
        }
    }

    fn start(&mut self, pending: Pending, action: TaskAction) -> Action {
        if self.pending.is_some() {
            return Action::None;
        }
        self.pending = Some(pending);
        self.error = None;
        self.message = None;
        self.invalidate_geometry();
        Action::Request(self.request(action))
    }

    fn visible(&self) -> Vec<usize> {
        let query = self.query.text.to_lowercase();
        self.tasks
            .iter()
            .enumerate()
            .filter_map(|(i, task)| {
                (self.show_completed
                    || !matches!(task.status, TaskStatus::Completed | TaskStatus::Cancelled))
                .then_some(())
                .filter(|_| {
                    [
                        task.id.to_string(),
                        task.title.clone(),
                        task.details.clone(),
                    ]
                    .iter()
                    .any(|value| value.to_lowercase().contains(&query))
                })
                .map(|_| i)
            })
            .collect()
    }

    fn current(&self) -> Option<&ProjectTask> {
        self.selected
            .and_then(|id| self.tasks.iter().find(|task| task.id == id))
    }

    fn normalize_selection(&mut self) {
        let visible = self.visible();
        if self
            .selected
            .is_none_or(|id| !visible.iter().any(|index| self.tasks[*index].id == id))
        {
            self.selected = visible.first().map(|index| self.tasks[*index].id);
        }
    }

    fn move_selection(&mut self, amount: isize) {
        let visible = self.visible();
        let current = visible
            .iter()
            .position(|i| Some(self.tasks[*i].id) == self.selected)
            .unwrap_or(0);
        let position = current
            .saturating_add_signed(amount)
            .min(visible.len().saturating_sub(1));
        self.selected = visible.get(position).map(|i| self.tasks[*i].id);
    }

    fn edit(&mut self, create: bool) {
        if self.pending.is_some() {
            return;
        }
        let task = if create { None } else { self.current() };
        if !create && task.is_none() {
            return;
        }
        let mut title = InputState::default();
        let mut details = InputState::default();
        let mut priority = InputState::default();
        title.set(task.map_or_else(String::new, |task| task.title.clone()));
        details.set(task.map_or_else(String::new, |task| task.details.clone()));
        priority.set(task.map_or_else(|| "0".into(), |task| task.priority.to_string()));
        self.editor = Some(Editor {
            id: task.map(|task| task.id),
            title,
            details,
            priority,
            dependencies: task.map_or_else(BTreeSet::new, |task| {
                task.depends_on.iter().copied().collect()
            }),
            focus: 0,
            offset: 0,
            follow: true,
            choosing: false,
            dependency_cursor: 0,
            dependency_offset: 0,
        });
        self.choosing_status = None;
        self.error = None;
        self.message = None;
        self.invalidate_geometry();
    }

    fn dependency_candidates(&self) -> Vec<usize> {
        let id = self.editor.as_ref().and_then(|editor| editor.id);
        self.tasks
            .iter()
            .enumerate()
            .filter_map(|(index, task)| (Some(task.id) != id).then_some(index))
            .collect()
    }

    fn toggle_dependency(&mut self, position: usize) {
        let Some(index) = self.dependency_candidates().get(position).copied() else {
            return;
        };
        let id = self.tasks[index].id;
        if let Some(editor) = &mut self.editor {
            editor.dependency_cursor = position;
            if !editor.dependencies.remove(&id) {
                editor.dependencies.insert(id);
            }
        }
    }

    fn save(&mut self) -> Action {
        let Some(editor) = &self.editor else {
            return Action::None;
        };
        if self.pending.is_some() {
            return Action::None;
        }
        let title = editor.title.text.trim().to_owned();
        let priority = editor.priority.text.trim().parse::<i64>();
        let error = if title.is_empty() {
            Some("请填写任务标题。")
        } else if title.len() > 512 {
            Some("任务标题不能超过 512 字节。")
        } else if editor.details.text.len() > 8192 {
            Some("任务说明不能超过 8192 字节。")
        } else if !priority
            .as_ref()
            .is_ok_and(|value| (-100..=100).contains(value))
        {
            Some("优先级应为 -100～100 的整数。")
        } else if editor.dependencies.len() > 64 {
            Some("一个任务最多选择 64 个依赖。")
        } else {
            None
        };
        if let Some(error) = error {
            self.error = Some(error.into());
            return Action::None;
        }
        let depends_on = editor.dependencies.iter().copied().collect();
        let action = if let Some(id) = editor.id {
            TaskAction::Update {
                id,
                title: Some(title),
                details: Some(editor.details.text.clone()),
                status: None,
                priority: Some(priority.unwrap()),
                depends_on: Some(depends_on),
            }
        } else {
            TaskAction::Create {
                title,
                details: editor.details.text.clone(),
                priority: priority.unwrap(),
                parent_id: None,
                depends_on,
            }
        };
        self.start(Pending::Save, action)
    }

    fn activate(&mut self, hit: Hit) -> Action {
        if self.pending.is_some() {
            return Action::None;
        }
        match hit {
            Hit::Row(position) => {
                self.selected = self.visible().get(position).map(|i| self.tasks[*i].id);
            }
            Hit::Search => self.searching = true,
            Hit::New => self.edit(true),
            Hit::Edit => self.edit(false),
            Hit::Refresh => {
                return self.start(
                    Pending::List,
                    TaskAction::List {
                        include_completed: true,
                    },
                );
            }
            Hit::Completed => {
                self.show_completed = !self.show_completed;
                self.normalize_selection();
            }
            Hit::Status => {
                if let Some(task) = self.current() {
                    self.choosing_status = TaskStatus::ALL
                        .iter()
                        .position(|status| *status == task.status);
                }
            }
            Hit::Release => {
                if let Some(id) = self.selected {
                    return self.start(Pending::Release, TaskAction::Release { id });
                }
            }
            Hit::Save => return self.save(),
            Hit::Back => return Action::Close,
            Hit::Field(focus) => {
                if let Some(editor) = &mut self.editor {
                    editor.focus = focus;
                    editor.follow = true;
                    if focus == 3 {
                        editor.choosing = true;
                    }
                }
            }
            Hit::Dependency(position) => self.toggle_dependency(position),
            Hit::DependencyDone => {
                if let Some(editor) = &mut self.editor {
                    editor.choosing = false;
                }
            }
            Hit::StatusChoice(status) => {
                self.choosing_status = None;
                if let Some(id) = self.selected {
                    return self.start(Pending::Status, TaskAction::SetStatus { id, status });
                }
            }
        }
        Action::None
    }

    fn status_input(&mut self, key: &crossterm::event::KeyEvent) -> Action {
        let Some(selected) = self.choosing_status.as_mut() else {
            return Action::None;
        };
        match key.code {
            KeyCode::Esc => self.choosing_status = None,
            KeyCode::Up => *selected = selected.saturating_sub(1),
            KeyCode::Down => *selected = (*selected + 1).min(TaskStatus::ALL.len() - 1),
            KeyCode::Enter => {
                let status = TaskStatus::ALL[*selected];
                return self.activate(Hit::StatusChoice(status));
            }
            _ => {}
        }
        Action::None
    }
}

impl Panel for State {
    fn handle(&mut self, event: &Event) -> Action {
        if let Event::Key(key) = event {
            if key.kind == KeyEventKind::Release {
                return Action::None;
            }
            if self.choosing_status.is_some() {
                return self.status_input(key);
            }
            if key.code == KeyCode::Esc {
                if let Some(editor) = &mut self.editor
                    && editor.choosing
                {
                    editor.choosing = false;
                    return Action::None;
                }
                if self.searching {
                    self.searching = false;
                    return Action::None;
                }
                return Action::Close;
            }
            if self.pending.is_some() {
                return Action::None;
            }
            if let Some(editor) = &mut self.editor {
                if key.code == KeyCode::Char('s') && key.modifiers.contains(KeyModifiers::CONTROL) {
                    return self.save();
                }
                if editor.choosing {
                    let count = self.dependency_candidates().len();
                    let editor = self.editor.as_mut().unwrap();
                    match key.code {
                        KeyCode::Up => {
                            editor.dependency_cursor = editor.dependency_cursor.saturating_sub(1)
                        }
                        KeyCode::Down => {
                            editor.dependency_cursor =
                                (editor.dependency_cursor + 1).min(count.saturating_sub(1))
                        }
                        KeyCode::Char(' ') => {
                            let position = editor.dependency_cursor;
                            self.toggle_dependency(position);
                        }
                        KeyCode::Enter | KeyCode::Tab | KeyCode::BackTab => editor.choosing = false,
                        _ => {}
                    }
                    return Action::None;
                }
                if key.code == KeyCode::Tab || key.code == KeyCode::BackTab {
                    editor.focus = if key.code == KeyCode::BackTab
                        || key.modifiers.contains(KeyModifiers::SHIFT)
                    {
                        (editor.focus + 3) % 4
                    } else {
                        (editor.focus + 1) % 4
                    };
                    editor.follow = true;
                    return Action::None;
                }
                match editor.focus {
                    0 => {
                        if key.code == KeyCode::Enter {
                            editor.focus = 1;
                        } else {
                            form::edit(&mut editor.title, key.code, key.modifiers);
                        }
                    }
                    1 => {
                        editor.follow = true;
                        if key.code == KeyCode::Enter && key.modifiers.is_empty() {
                            editor.details.insert("\n");
                        } else {
                            editor
                                .details
                                .edit(event, self.details_area.width.max(1) as usize);
                        }
                    }
                    2 => form::edit(&mut editor.priority, key.code, key.modifiers),
                    3 => {
                        if matches!(key.code, KeyCode::Enter | KeyCode::Char(' ')) {
                            editor.choosing = true;
                        }
                    }
                    _ => {}
                }
                return Action::None;
            }
            if self.searching {
                if key.code == KeyCode::Enter {
                    self.searching = false;
                } else {
                    form::edit(&mut self.query, key.code, key.modifiers);
                    self.normalize_selection();
                    self.offset = 0;
                }
                return Action::None;
            }
            match key.code {
                KeyCode::Up => self.move_selection(-1),
                KeyCode::Down => self.move_selection(1),
                KeyCode::PageUp => {
                    self.move_selection(-(self.body_area.height as isize / 2).max(1))
                }
                KeyCode::PageDown => {
                    self.move_selection((self.body_area.height as isize / 2).max(1))
                }
                KeyCode::Enter | KeyCode::Char('e') => self.edit(false),
                KeyCode::Char('n') => self.edit(true),
                KeyCode::Char('r') => return self.activate(Hit::Refresh),
                KeyCode::Char('a') => return self.activate(Hit::Completed),
                KeyCode::Char('s') => return self.activate(Hit::Status),
                KeyCode::Char('u') => return self.activate(Hit::Release),
                KeyCode::Char('/') => self.searching = true,
                _ => {}
            }
        } else if let Event::Paste(text) = event {
            if self.pending.is_none() {
                if let Some(editor) = &mut self.editor {
                    if !editor.choosing {
                        match editor.focus {
                            0 => form::paste(&mut editor.title, text),
                            1 => {
                                editor.details.insert(text);
                                editor.follow = true;
                            }
                            2 => form::paste(&mut editor.priority, text),
                            _ => {}
                        }
                    }
                } else {
                    self.searching = true;
                    form::paste(&mut self.query, text);
                    self.normalize_selection();
                }
            }
        } else if let Event::Mouse(mouse) = event {
            let point = Position::new(mouse.column, mouse.row);
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some((_, hit)) = self
                        .hits
                        .iter()
                        .find(|(area, _)| area.contains(point))
                        .copied()
                    {
                        if matches!(hit, Hit::Field(1))
                            && self.pending.is_none()
                            && let Some(editor) = &mut self.editor
                        {
                            let offset = editor.details.position_at(
                                (point.x - self.details_area.x) as usize,
                                editor.offset + (point.y - self.details_area.y) as usize,
                                self.details_area.width.max(1) as usize,
                            );
                            editor.details.set_cursor(offset);
                            editor.follow = true;
                        }
                        return self.activate(hit);
                    }
                }
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                    if self.body_area.contains(point) =>
                {
                    let down = mouse.kind == MouseEventKind::ScrollDown;
                    if let Some(editor) = &mut self.editor {
                        if editor.choosing {
                            let count = self
                                .tasks
                                .len()
                                .saturating_sub(usize::from(editor.id.is_some()));
                            editor.dependency_cursor = if down {
                                (editor.dependency_cursor + 3).min(count.saturating_sub(1))
                            } else {
                                editor.dependency_cursor.saturating_sub(3)
                            };
                        } else {
                            editor.offset = if down {
                                editor.offset.saturating_add(3)
                            } else {
                                editor.offset.saturating_sub(3)
                            };
                            editor.follow = false;
                        }
                    } else {
                        self.move_selection(if down { 3 } else { -3 });
                    }
                }
                _ => {}
            }
        }
        Action::None
    }

    fn receive(&mut self, response: Response) -> Action {
        let pending = self.pending.take();
        match response {
            Response::Tasks(snapshot) if pending.is_some() => {
                self.tasks = snapshot.tasks;
                if let Some(id) = snapshot.changed {
                    self.selected = Some(id);
                }
                if matches!(pending, Some(Pending::Save)) {
                    self.editor = None;
                    self.message = Some("任务已保存。".into());
                }
                if matches!(pending, Some(Pending::Status)) {
                    self.message = Some("任务状态已更新。".into());
                }
                if matches!(pending, Some(Pending::Release)) {
                    self.message = Some("任务认领已释放。".into());
                }
                self.normalize_selection();
            }
            Response::Error(error) => self.error(error),
            _ => self.error("服务返回了不匹配的数据；任务草稿已保留，请核对连接后重试。".into()),
        }
        self.invalidate_geometry();
        Action::None
    }

    fn error(&mut self, error: String) {
        self.pending = None;
        self.error = Some(error);
        self.invalidate_geometry();
    }

    fn draw(&mut self, frame: &mut Frame<'_>, area: Rect) {
        self.invalidate_geometry();
        if area.width < 24 || area.height < 10 {
            frame.render_widget(
                Paragraph::new("请扩大窗口 · Esc 收起").style(components::muted()),
                area,
            );
            return;
        }
        frame.render_widget(
            Paragraph::new(if self.editor.is_some() {
                "项目任务 · 编辑"
            } else {
                "项目任务"
            })
            .style(components::accent()),
            row(area, 0),
        );
        let buttons = if self.editor.is_some() {
            vec![
                ("收起（保留草稿）", Hit::Back),
                ("保存 Ctrl+S", Hit::Save),
                ("刷新任务", Hit::Refresh),
            ]
        } else {
            vec![
                ("新建 N", Hit::New),
                ("编辑 E", Hit::Edit),
                ("状态 S", Hit::Status),
                ("释放 U", Hit::Release),
                ("刷新 R", Hit::Refresh),
                (
                    if self.show_completed {
                        "隐藏已结束 A"
                    } else {
                        "显示已结束 A"
                    },
                    Hit::Completed,
                ),
            ]
        };
        toolbar(frame, row(area, 1), &buttons, &mut self.hits);
        let status = self.error.as_deref().or(self.message.as_deref()).unwrap_or(
            if self.pending.is_some() {
                "正在请求工作区…"
            } else {
                ""
            },
        );
        frame.render_widget(
            Paragraph::new(safe_output(status)).style(Style::default().fg(
                if self.error.is_some() {
                    THEME.error
                } else {
                    THEME.muted
                },
            )),
            row(area, area.height - 2),
        );
        let hint = if self.editor.as_ref().is_some_and(|e| e.choosing) {
            "↑↓ 选择依赖 · Space 勾选 · Enter/Esc 返回编辑"
        } else if self.editor.is_some() {
            "Tab 切换字段 · 依赖 Enter 选择 · Ctrl+S 保存 · Esc 保留草稿"
        } else {
            "↑↓ 选择 · Enter 编辑 · / 搜索 · S 状态 · A 显示已结束 · Esc 收起"
        };
        frame.render_widget(
            Paragraph::new(hint).style(components::muted()),
            row(area, area.height - 1),
        );
        self.body_area = Rect::new(area.x, area.y + 3, area.width, area.height - 5);
        if self.choosing_status.is_some() {
            self.draw_status(frame);
        } else if self.editor.as_ref().is_some_and(|editor| editor.choosing) {
            self.draw_dependencies(frame);
        } else if self.editor.is_some() {
            self.draw_editor(frame);
        } else {
            self.draw_list(frame);
        }
    }

    fn invalidate_geometry(&mut self) {
        self.hits.clear();
        self.body_area = Rect::default();
        self.details_area = Rect::default();
    }
}

impl State {
    fn draw_status(&mut self, frame: &mut Frame<'_>) {
        let Some(selected) = self.choosing_status else {
            return;
        };
        frame.render_widget(
            Paragraph::new("选择任务状态 · Enter 确认 · Esc 返回").style(components::muted()),
            row(self.body_area, 0),
        );
        let height = self.body_area.height.saturating_sub(2) as usize;
        let offset = selected.saturating_sub(height.saturating_sub(1));
        for (index, status) in TaskStatus::ALL.iter().enumerate().skip(offset).take(height) {
            let rect = row(self.body_area, (index - offset) as u16 + 2);
            frame.render_widget(
                Paragraph::new(status_label(*status)).style(if selected == index {
                    components::highlighted()
                } else {
                    components::normal()
                }),
                rect,
            );
            self.hits.push((rect, Hit::StatusChoice(*status)));
        }
    }

    fn draw_dependencies(&mut self, frame: &mut Frame<'_>) {
        let candidates = self.dependency_candidates();
        let editor = self.editor.as_mut().unwrap();
        frame.render_widget(
            Paragraph::new(format!(
                "选择依赖 · 已选择 {} 项",
                editor.dependencies.len()
            ))
            .style(components::accent()),
            row(self.body_area, 0),
        );
        let done = row(self.body_area, 1);
        frame.render_widget(
            Paragraph::new("[ 完成选择 · Enter ]").style(components::accent()),
            done,
        );
        self.hits.push((done, Hit::DependencyDone));
        let height = self.body_area.height.saturating_sub(3) as usize;
        if height == 0 {
            return;
        }
        editor.dependency_cursor = editor
            .dependency_cursor
            .min(candidates.len().saturating_sub(1));
        editor.dependency_offset = editor
            .dependency_offset
            .min(editor.dependency_cursor)
            .max(editor.dependency_cursor.saturating_sub(height - 1));
        for (position, index) in candidates
            .iter()
            .enumerate()
            .skip(editor.dependency_offset)
            .take(height)
        {
            let task = &self.tasks[*index];
            let label = format!(
                "{} #{} {} · {}",
                if editor.dependencies.contains(&task.id) {
                    "[x]"
                } else {
                    "[ ]"
                },
                task.id,
                safe_output(&task.title),
                status_label(task.status)
            );
            let rect = row(
                self.body_area,
                (position - editor.dependency_offset) as u16 + 3,
            );
            frame.render_widget(
                Paragraph::new(label).style(if position == editor.dependency_cursor {
                    components::highlighted()
                } else {
                    components::normal()
                }),
                rect,
            );
            self.hits.push((rect, Hit::Dependency(position)));
        }
        if candidates.is_empty() {
            frame.render_widget(
                Paragraph::new("没有可选的其他任务。"),
                row(self.body_area, 3),
            );
        }
    }

    fn draw_editor(&mut self, frame: &mut Frame<'_>) {
        let editor = self.editor.as_mut().unwrap();
        let area = self.body_area;
        frame.render_widget(
            Paragraph::new("标题").style(components::muted()),
            row(area, 0),
        );
        field(
            frame,
            row(area, 1),
            &editor.title,
            editor.focus == 0 && self.pending.is_none(),
            "任务标题",
        );
        self.hits.push((row(area, 1), Hit::Field(0)));
        frame.render_widget(
            Paragraph::new("优先级（-100～100）").style(components::muted()),
            row(area, 2),
        );
        field(
            frame,
            row(area, 3),
            &editor.priority,
            editor.focus == 2 && self.pending.is_none(),
            "0",
        );
        self.hits.push((row(area, 3), Hit::Field(2)));
        let dependency_label = format!(
            "依赖：{}  [ Enter 选择任务 ]",
            if editor.dependencies.is_empty() {
                "无".into()
            } else {
                editor
                    .dependencies
                    .iter()
                    .map(|id| format!("#{id}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
        );
        frame.render_widget(
            Paragraph::new(dependency_label).style(if editor.focus == 3 {
                components::highlighted()
            } else {
                components::accent()
            }),
            row(area, 4),
        );
        self.hits.push((row(area, 4), Hit::Field(3)));
        frame.render_widget(
            Paragraph::new("说明与验收要求").style(components::muted()),
            row(area, 5),
        );
        self.details_area = Rect::new(
            area.x,
            area.y + 6,
            area.width,
            area.height.saturating_sub(6),
        );
        editor_area(
            frame,
            self.details_area,
            &editor.details,
            &mut editor.offset,
            editor.focus == 1 && self.pending.is_none(),
            std::mem::take(&mut editor.follow),
        );
        self.hits.push((self.details_area, Hit::Field(1)));
    }

    fn draw_list(&mut self, frame: &mut Frame<'_>) {
        self.normalize_selection();
        let area = self.body_area;
        field(
            frame,
            row(area, 0),
            &self.query,
            self.searching && self.pending.is_none(),
            "搜索标题、说明或任务编号…",
        );
        self.hits.push((row(area, 0), Hit::Search));
        let content = Rect::new(
            area.x,
            area.y + 2,
            area.width,
            area.height.saturating_sub(2),
        );
        let (list_area, detail_area) = if content.width >= 80 {
            let width = (content.width * 2 / 5).max(30);
            (
                Rect::new(content.x, content.y, width, content.height),
                Rect::new(
                    content.x + width + 2,
                    content.y,
                    content.width - width - 2,
                    content.height,
                ),
            )
        } else {
            let detail_height = if content.height >= 12 { 5 } else { 0 };
            (
                Rect::new(
                    content.x,
                    content.y,
                    content.width,
                    content.height - detail_height,
                ),
                Rect::new(
                    content.x,
                    content.bottom() - detail_height,
                    content.width,
                    detail_height,
                ),
            )
        };
        let visible = self.visible();
        let height = (list_area.height as usize / 2).max(1);
        let selected_position = visible
            .iter()
            .position(|index| Some(self.tasks[*index].id) == self.selected)
            .unwrap_or(0);
        self.offset = self
            .offset
            .min(selected_position)
            .max(selected_position.saturating_sub(height - 1));
        for (position, index) in visible.iter().enumerate().skip(self.offset).take(height) {
            let task = &self.tasks[*index];
            let y = ((position - self.offset) * 2) as u16;
            if y >= list_area.height {
                break;
            }
            let rect = Rect::new(
                list_area.x,
                list_area.y + y,
                list_area.width,
                2.min(list_area.height - y),
            );
            frame.render_widget(
                Paragraph::new(format!("#{} {}", task.id, safe_output(&task.title))).style(
                    if Some(task.id) == self.selected {
                        components::highlighted()
                    } else {
                        components::normal()
                    },
                ),
                row(rect, 0),
            );
            if rect.height > 1 {
                let status = format!(
                    "{} · 优先级 {}{}{}",
                    status_label(task.status),
                    task.priority,
                    if task.owner_session_id.is_some() {
                        " · 已认领"
                    } else {
                        ""
                    },
                    if task.blocked_by.is_empty() {
                        ""
                    } else {
                        " · 依赖未完成"
                    }
                );
                frame.render_widget(
                    Paragraph::new(status).style(components::muted()),
                    row(rect, 1),
                );
            }
            self.hits.push((rect, Hit::Row(position)));
        }
        if visible.is_empty() {
            frame.render_widget(
                Paragraph::new("没有匹配的任务 · N 新建 · A 查看已结束").style(components::muted()),
                list_area,
            );
        }
        if !detail_area.is_empty()
            && let Some(task) = self.current()
        {
            let text = format!(
                "#{} {}\n{} · 优先级 {}\n依赖：{}\n\n{}",
                task.id,
                safe_output(&task.title),
                status_label(task.status),
                task.priority,
                if task.depends_on.is_empty() {
                    "无".into()
                } else {
                    task.depends_on
                        .iter()
                        .map(|id| format!("#{id}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                },
                safe_output(&task.details)
            );
            frame.render_widget(
                Paragraph::new(text)
                    .wrap(Wrap { trim: false })
                    .style(components::normal()),
                detail_area,
            );
        }
    }
}

fn toolbar(
    frame: &mut Frame<'_>,
    area: Rect,
    buttons: &[(&str, Hit)],
    hits: &mut Vec<(Rect, Hit)>,
) {
    use unicode_width::UnicodeWidthStr;
    let mut x = area.x;
    for (label, hit) in buttons {
        let width = (label.width() as u16 + 2).min(area.right().saturating_sub(x));
        if width == 0 {
            break;
        }
        let rect = Rect::new(x, area.y, width, area.height);
        components::button(frame, rect, label, components::Control::default());
        hits.push((rect, *hit));
        x = x.saturating_add(width + 1);
    }
}

fn status_label(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "待处理",
        TaskStatus::InProgress => "进行中",
        TaskStatus::Blocked => "已阻塞",
        TaskStatus::Completed => "已完成",
        TaskStatus::Cancelled => "已取消",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn task(id: i64) -> ProjectTask {
        ProjectTask {
            id,
            project_id: "project".into(),
            parent_id: None,
            title: format!("task {id}"),
            details: "original".into(),
            status: TaskStatus::Pending,
            priority: 0,
            owner_session_id: None,
            depends_on: vec![],
            blocked_by: vec![],
            created_at: 0,
            updated_at: 0,
        }
    }
    fn state() -> State {
        let mut state = State::new("session".into(), "/project".into());
        state.tasks = vec![task(12), task(27), task(41)];
        state.selected = Some(27);
        state
    }

    #[test]
    fn editing_and_dependency_selection_send_server_ids_and_preserve_multiline_details() {
        let mut state = state();
        state.edit(false);
        assert_eq!(state.dependency_candidates(), [0, 2]);
        state.toggle_dependency(1);
        let editor = state.editor.as_mut().unwrap();
        editor.details.set("  acceptance\nnext step  \n".into());
        editor.priority.set("-7".into());
        let Action::Request(Request::Tasks {
            session,
            expected_workspace,
            action:
                TaskAction::Update {
                    id,
                    details,
                    priority,
                    depends_on,
                    ..
                },
        }) = state.save()
        else {
            panic!("update expected")
        };
        assert_eq!(session, "session");
        assert_eq!(expected_workspace.as_deref(), Some("/project"));
        assert_eq!(id, 27);
        assert_eq!(details.as_deref(), Some("  acceptance\nnext step  \n"));
        assert_eq!(priority, Some(-7));
        assert_eq!(depends_on, Some(vec![41]));
        state.error("connection lost".into());
        assert_eq!(
            state.editor.as_ref().unwrap().details.text,
            "  acceptance\nnext step  \n"
        );
    }

    #[test]
    fn create_and_clear_dependencies_use_distinct_structured_operations() {
        let mut state = state();
        state.edit(true);
        state.editor.as_mut().unwrap().title.set("new task".into());
        assert!(
            matches!(state.save(), Action::Request(Request::Tasks { action: TaskAction::Create { title, parent_id: None, depends_on, .. }, .. }) if title == "new task" && depends_on.is_empty())
        );
        state.error("validation failed".into());
        assert_eq!(state.editor.as_ref().unwrap().title.text, "new task");
        state.editor = None;
        state.edit(false);
        assert!(
            matches!(state.save(), Action::Request(Request::Tasks { action: TaskAction::Update { depends_on: Some(dependencies), .. }, .. }) if dependencies.is_empty())
        );
    }

    #[test]
    fn filtering_and_status_changes_keep_selected_id_stable() {
        let mut state = state();
        state.tasks[0].status = TaskStatus::Completed;
        state.normalize_selection();
        assert_eq!(state.selected, Some(27));
        state.query.set("41".into());
        state.normalize_selection();
        assert_eq!(state.selected, Some(41));
        assert!(matches!(
            state.activate(Hit::StatusChoice(TaskStatus::Blocked)),
            Action::Request(Request::Tasks {
                action: TaskAction::SetStatus {
                    id: 41,
                    status: TaskStatus::Blocked
                },
                ..
            })
        ));
    }

    #[test]
    fn short_status_picker_keeps_the_selected_bottom_status_visible() {
        use ratatui::{Terminal, backend::TestBackend};
        use unicode_width::UnicodeWidthStr;
        let mut state = state();
        state.choosing_status = Some(4);
        let mut terminal = Terminal::new(TestBackend::new(48, 10)).unwrap();
        terminal
            .draw(|frame| state.draw(frame, frame.area()))
            .unwrap();
        let (rect, _) = state
            .hits
            .iter()
            .find(|(_, hit)| matches!(hit, Hit::StatusChoice(TaskStatus::Cancelled)))
            .expect("the selected status must retain a visible hit target");
        assert_eq!(rect.height, 1);
        assert!(state.body_area.contains(Position::new(rect.x, rect.y)));
        let buffer = terminal.backend().buffer();
        let mut visible = String::new();
        let mut x = rect.x;
        // Wide glyphs occupy continuation cells; those cells are not extra spaces in the rendered label.
        while x < rect.right() {
            let symbol = buffer[(x, rect.y)].symbol();
            visible.push_str(symbol);
            x += symbol.width().max(1) as u16;
        }
        assert!(
            visible.starts_with(status_label(TaskStatus::Cancelled)),
            "{visible:?}"
        );
    }
}
