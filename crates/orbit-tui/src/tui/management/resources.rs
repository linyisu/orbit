//! Skills are edited against the exact server snapshot, including imported read-only skills.
use super::{Action, Panel, files::Source};
use crate::{
    tui::{components, connect_custom::form, input::InputState, render::safe_output, theme::THEME},
    workspace::protocol::{Request, Response},
};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use orbit_protocol::{
    SessionAction,
    resources::{Action as ResourceAction, Skill, Snapshot},
};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::Style,
    widgets::Paragraph,
};

#[derive(Clone, Copy, Debug)]
enum Pending {
    List,
    Read,
    Reload,
    Save,
    Load,
}

#[derive(Debug)]
struct Editor {
    name: InputState,
    body: Source,
    original: Option<String>,
    readonly: bool,
    focus: usize,
    offset: usize,
    follow: bool,
}

#[derive(Clone, Copy, Debug)]
enum Hit {
    Row(usize),
    Search,
    New,
    Refresh,
    Back,
    Save,
    Load,
    Name,
    Body,
}

#[derive(Debug)]
pub(super) struct State {
    session: String,
    workspace: String,
    skills: Vec<Skill>,
    selected: usize,
    offset: usize,
    query: InputState,
    searching: bool,
    editor: Option<Editor>,
    pending: Option<Pending>,
    message: Option<String>,
    error: Option<String>,
    hits: Vec<(Rect, Hit)>,
    body_area: Rect,
}

impl State {
    pub(super) fn new(session: String, workspace: String) -> Self {
        Self {
            session,
            workspace,
            skills: Vec::new(),
            selected: 0,
            offset: 0,
            query: InputState::default(),
            searching: false,
            editor: None,
            pending: None,
            message: None,
            error: None,
            hits: Vec::new(),
            body_area: Rect::default(),
        }
    }

    pub(super) fn initial(&mut self) -> Request {
        self.pending = Some(Pending::List);
        self.request(ResourceAction::Skills)
    }

    fn request(&self, action: ResourceAction) -> Request {
        Request::Resources {
            session: self.session.clone(),
            expected_workspace: Some(self.workspace.clone()),
            action,
        }
    }

    fn visible(&self) -> Vec<usize> {
        let query = self.query.text.to_lowercase();
        self.skills
            .iter()
            .enumerate()
            .filter_map(|(i, skill)| {
                [
                    skill.name.as_str(),
                    skill.title.as_str(),
                    skill.description.as_str(),
                    skill.source.as_str(),
                ]
                .iter()
                .any(|value| value.to_lowercase().contains(&query))
                .then_some(i)
            })
            .collect()
    }

    fn selected(&self) -> Option<&Skill> {
        self.visible()
            .get(self.selected)
            .and_then(|i| self.skills.get(*i))
    }

    fn start(&mut self, pending: Pending, request: Request) -> Action {
        if self.pending.is_some() {
            return Action::None;
        }
        self.pending = Some(pending);
        self.error = None;
        self.message = None;
        self.invalidate_geometry();
        Action::Request(request)
    }

    fn read(&mut self) -> Action {
        let Some(skill) = self.selected() else {
            return Action::None;
        };
        let request = self.request(ResourceAction::ReadSkill {
            name: skill.name.clone(),
        });
        self.start(Pending::Read, request)
    }

    fn load(&mut self) -> Action {
        let name = self
            .editor
            .as_ref()
            .map(|e| e.name.text.trim().to_owned())
            .or_else(|| self.selected().map(|s| s.name.clone()));
        let Some(name) = name.filter(|name| !name.is_empty()) else {
            return Action::None;
        };
        if self
            .editor
            .as_ref()
            .is_some_and(|e| e.original.as_ref() != Some(&e.body.raw()))
        {
            self.error = Some("请先保存技能，再应用到当前会话。".into());
            return Action::None;
        }
        self.start(
            Pending::Load,
            Request::SessionAction {
                session: self.session.clone(),
                action: SessionAction::LoadSkill { name },
            },
        )
    }

    fn create(&mut self) {
        if self.pending.is_some() {
            return;
        }
        let body = Source::new(
            "---\nname: 新技能\ndescription: 描述这个技能的用途\n---\n\n在这里编写 Agent 执行步骤。\n",
        );
        self.editor = Some(Editor {
            name: InputState::default(),
            body,
            original: None,
            readonly: false,
            focus: 0,
            offset: 0,
            follow: true,
        });
        self.error = None;
        self.message = None;
        self.invalidate_geometry();
    }

    fn save(&mut self) -> Action {
        let Some(editor) = &self.editor else {
            return Action::None;
        };
        if editor.readonly || self.pending.is_some() {
            return Action::None;
        }
        if editor.name.text.trim().is_empty() {
            self.error = Some("请填写技能名称，例如 code-review。".into());
            return Action::None;
        }
        let request = self.request(ResourceAction::SaveSkill {
            name: editor.name.text.trim().into(),
            original: editor.original.clone(),
            content: editor.body.raw(),
        });
        self.start(Pending::Save, request)
    }

    /// Keep the editor until a successful save; hiding never loses its draft.
    fn back(&mut self) -> Action {
        Action::Close
    }

    fn activate(&mut self, hit: Hit) -> Action {
        if self.pending.is_some() {
            return Action::None;
        }
        match hit {
            Hit::Row(index) => {
                self.selected = index;
                self.read()
            }
            Hit::Search => {
                self.searching = true;
                Action::None
            }
            Hit::New => {
                self.create();
                Action::None
            }
            Hit::Refresh => {
                if let Some(editor) = &self.editor {
                    if editor.original.is_none() {
                        self.error = Some("新技能草稿已保留，请填写名称并保存。".into());
                        return Action::None;
                    }
                    let request = self.request(ResourceAction::ReadSkill {
                        name: editor.name.text.trim().into(),
                    });
                    self.start(Pending::Reload, request)
                } else {
                    let request = self.request(ResourceAction::Skills);
                    self.start(Pending::List, request)
                }
            }
            Hit::Back => {
                if self
                    .editor
                    .as_ref()
                    .is_some_and(|editor| {
                        editor.readonly || editor.original.as_ref() == Some(&editor.body.raw())
                    })
                {
                    self.editor = None;
                    self.invalidate_geometry();
                    Action::None
                } else {
                    self.back()
                }
            }
            Hit::Save => self.save(),
            Hit::Load => self.load(),
            Hit::Name | Hit::Body => {
                if let Some(editor) = &mut self.editor {
                    editor.focus = usize::from(matches!(hit, Hit::Body));
                    editor.follow = true;
                }
                Action::None
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
                if self.searching {
                    self.searching = false;
                    return Action::None;
                }
                return self.back();
            }
            if self.pending.is_some() {
                return Action::None;
            }
            if let Some(editor) = &mut self.editor {
                if key
                    .modifiers
                    .contains(KeyModifiers::CONTROL)
                    && key.code == KeyCode::Char('s')
                {
                    return self.save();
                }
                if key.code == KeyCode::Tab || key.code == KeyCode::BackTab {
                    if editor.original.is_none() {
                        editor.focus = 1 - editor.focus;
                        editor.follow = true;
                    }
                    return Action::None;
                }
                if editor.readonly {
                    match key.code {
                        KeyCode::Up => editor.offset = editor.offset.saturating_sub(1),
                        KeyCode::Down => editor.offset = editor.offset.saturating_add(1),
                        KeyCode::PageUp => {
                            editor.offset = editor
                                .offset
                                .saturating_sub(self.body_area.height as usize)
                        }
                        KeyCode::PageDown => {
                            editor.offset = editor
                                .offset
                                .saturating_add(self.body_area.height as usize)
                        }
                        KeyCode::Char('l') => return self.load(),
                        KeyCode::Backspace => return self.activate(Hit::Back),
                        _ => {}
                    }
                } else if editor.focus == 0 && editor.original.is_none() {
                    form::edit(&mut editor.name, key.code, key.modifiers);
                } else {
                    editor.follow = true;
                    editor
                        .body
                        .edit(event, self.body_area.width.max(1) as usize);
                }
                return Action::None;
            }
            if self.searching {
                if key.code == KeyCode::Enter {
                    self.searching = false;
                } else {
                    form::edit(&mut self.query, key.code, key.modifiers);
                    self.selected = 0;
                    self.offset = 0;
                }
                return Action::None;
            }
            match key.code {
                KeyCode::Up => self.selected = self.selected.saturating_sub(1),
                KeyCode::Down => {
                    self.selected = (self.selected + 1).min(self.visible().len().saturating_sub(1))
                }
                KeyCode::PageUp => {
                    self.selected = self
                        .selected
                        .saturating_sub(self.body_area.height as usize / 2)
                }
                KeyCode::PageDown => {
                    self.selected = (self.selected + self.body_area.height as usize / 2)
                        .min(self.visible().len().saturating_sub(1))
                }
                KeyCode::Enter | KeyCode::Char('e') => return self.read(),
                KeyCode::Char('n') => self.create(),
                KeyCode::Char('l') => return self.load(),
                KeyCode::Char('r') => return self.activate(Hit::Refresh),
                KeyCode::Char('/') => self.searching = true,
                _ => {}
            }
        } else if let Event::Paste(text) = event {
            if self.pending.is_none() {
                if let Some(editor) = &mut self.editor {
                    if !editor.readonly {
                        if editor.focus == 0 && editor.original.is_none() {
                            form::paste(&mut editor.name, text);
                        } else {
                            editor
                                .body
                                .edit(event, self.body_area.width.max(1) as usize);
                            editor.follow = true;
                        }
                    }
                } else {
                    self.searching = true;
                    form::paste(&mut self.query, text);
                    self.selected = 0;
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
                        if matches!(hit, Hit::Body)
                            && self.pending.is_none()
                            && let Some(editor) = &mut self.editor
                            && !editor.readonly
                        {
                            let offset = editor.body.input.position_at(
                                (point.x - self.body_area.x) as usize,
                                editor.offset + (point.y - self.body_area.y) as usize,
                                self.body_area.width.max(1) as usize,
                            );
                            editor.body.input.set_cursor(offset);
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
                        editor.offset = if down {
                            editor.offset.saturating_add(3)
                        } else {
                            editor.offset.saturating_sub(3)
                        };
                        editor.follow = false;
                    } else {
                        self.selected = if down {
                            (self.selected + 3).min(self.visible().len().saturating_sub(1))
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
        match (pending, response) {
            (
                Some(Pending::List | Pending::Save),
                Response::Resources(Snapshot::Skills(skills)),
            ) => {
                self.skills = skills;
                if matches!(pending, Some(Pending::Save)) {
                    self.editor = None;
                    self.message = Some("技能已保存到全局库。".into());
                }
                self.selected = self
                    .selected
                    .min(self.visible().len().saturating_sub(1));
            }
            (
                Some(Pending::Read),
                Response::Resources(Snapshot::Skill {
                    name,
                    content,
                    editable,
                }),
            ) => {
                let mut name_input = InputState::default();
                name_input.set(name);
                let body = Source::new(&content);
                self.editor = Some(Editor {
                    name: name_input,
                    body,
                    original: Some(content),
                    readonly: !editable,
                    focus: 1,
                    offset: 0,
                    follow: true,
                });
            }
            (
                Some(Pending::Reload),
                Response::Resources(Snapshot::Skill {
                    name,
                    content,
                    editable,
                }),
            ) => {
                if let Some(editor) = &mut self.editor {
                    if editor.name.text.trim() == name {
                        editor.original = Some(content);
                        editor.readonly = !editable;
                        self.message =
                            Some("已刷新原稿，编辑草稿仍保留。请核对外部修改后再保存。".into());
                    } else {
                        self.error = Some("服务返回的技能名称不匹配，草稿已保留。".into());
                    }
                }
            }
            (Some(Pending::Load), Response::Report(report)) => self.message = Some(report.text),
            (_, Response::Error(error)) => self.error(error),
            _ => self.error("服务返回了不匹配的数据；草稿已保留，请核对连接后重试。".into()),
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
        if area.width < 20 || area.height < 8 {
            frame.render_widget(
                Paragraph::new("请扩大窗口 · Esc 收起").style(components::muted()),
                area,
            );
            return;
        }
        let title = if self.editor.is_some() {
            "技能 · 查看与编辑"
        } else {
            "全局技能"
        };
        frame.render_widget(
            Paragraph::new(title).style(components::accent()),
            row(area, 0),
        );
        let buttons: Vec<(&str, Hit)> = if let Some(editor) = &self.editor {
            if editor.readonly {
                vec![
                    ("返回", Hit::Back),
                    ("应用到会话 L", Hit::Load),
                    ("刷新原稿", Hit::Refresh),
                ]
            } else {
                vec![
                    ("收起（保留草稿）", Hit::Back),
                    ("保存 Ctrl+S", Hit::Save),
                    ("应用到会话", Hit::Load),
                    ("刷新原稿", Hit::Refresh),
                ]
            }
        } else {
            vec![
                ("新建 N", Hit::New),
                ("刷新 R", Hit::Refresh),
                ("搜索 /", Hit::Search),
                ("应用 L", Hit::Load),
            ]
        };
        toolbar(frame, row(area, 1), &buttons, &mut self.hits);
        let status = self
            .error
            .as_deref()
            .or(self.message.as_deref())
            .unwrap_or(if self.pending.is_some() {
                "正在读取设置…"
            } else {
                ""
            });
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
        frame.render_widget(
            Paragraph::new(if self.editor.is_some() {
                "Tab 切换字段 · 正文 Enter 换行 · Esc 收起并保留草稿"
            } else {
                "↑↓ 选择 · Enter 查看/编辑 · Esc 收起"
            })
            .style(components::muted()),
            row(area, area.height - 1),
        );
        let content = Rect::new(area.x, area.y + 3, area.width, area.height - 5);
        if let Some(editor) = &mut self.editor {
            frame.render_widget(
                Paragraph::new(format!(
                    "名称：{}{}",
                    safe_output(&editor.name.text),
                    if editor.readonly {
                        " · 只读来源"
                    } else {
                        ""
                    }
                ))
                .style(components::muted()),
                row(content, 0),
            );
            let name_area = row(content, 1);
            if editor.original.is_none() {
                field(
                    frame,
                    name_area,
                    &editor.name,
                    editor.focus == 0 && self.pending.is_none(),
                    "技能名称，例如 code-review",
                );
                self.hits.push((name_area, Hit::Name));
            } else {
                frame.render_widget(
                    Paragraph::new("正文（保留原始 Markdown）").style(components::muted()),
                    name_area,
                );
            }
            self.body_area = Rect::new(
                content.x,
                content.y + 2,
                content.width,
                content.height.saturating_sub(2),
            );
            editor_area(
                frame,
                self.body_area,
                &editor.body.input,
                &mut editor.offset,
                !editor.readonly && editor.focus == 1 && self.pending.is_none(),
                std::mem::take(&mut editor.follow),
            );
            self.hits
                .push((self.body_area, Hit::Body));
        } else {
            field(
                frame,
                row(content, 0),
                &self.query,
                self.searching && self.pending.is_none(),
                "搜索名称、说明或来源…",
            );
            self.hits
                .push((row(content, 0), Hit::Search));
            self.body_area = Rect::new(
                content.x,
                content.y + 2,
                content.width,
                content.height.saturating_sub(2),
            );
            let visible = self.visible();
            let height = (self.body_area.height as usize / 2).max(1);
            self.selected = self
                .selected
                .min(visible.len().saturating_sub(1));
            self.offset = self
                .offset
                .min(self.selected)
                .max(self.selected.saturating_sub(height - 1));
            for (position, index) in visible
                .iter()
                .enumerate()
                .skip(self.offset)
                .take(height)
            {
                let skill = &self.skills[*index];
                let y = ((position - self.offset) * 2) as u16;
                let text = format!(
                    "{}  /{}  {}",
                    safe_output(&skill.title),
                    safe_output(&skill.name),
                    if skill.editable { "" } else { "[只读]" }
                );
                let rect = Rect::new(
                    self.body_area.x,
                    self.body_area.y + y,
                    self.body_area.width,
                    2.min(self.body_area.height - y),
                );
                let style = if position == self.selected {
                    components::highlighted()
                } else {
                    components::normal()
                };
                frame.render_widget(Paragraph::new(text).style(style), row(rect, 0));
                if rect.height > 1 {
                    frame.render_widget(
                        Paragraph::new(safe_output(&format!(
                            "{} · {}",
                            skill.description, skill.source
                        )))
                        .style(components::muted()),
                        row(rect, 1),
                    );
                }
                self.hits
                    .push((rect, Hit::Row(position)));
            }
            if visible.is_empty() {
                frame.render_widget(
                    Paragraph::new("没有匹配的技能 · N 新建全局技能").style(components::muted()),
                    self.body_area,
                );
            }
        }
    }

    fn invalidate_geometry(&mut self) {
        self.hits.clear();
        self.body_area = Rect::default();
    }
}

pub(super) fn row(area: Rect, offset: u16) -> Rect {
    Rect::new(
        area.x,
        area.y.saturating_add(offset),
        area.width,
        u16::from(offset < area.height),
    )
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

pub(super) fn field(
    frame: &mut Frame<'_>,
    area: Rect,
    input: &InputState,
    focused: bool,
    placeholder: &str,
) {
    if area.is_empty() {
        return;
    }
    frame.render_widget(
        Paragraph::new(form::input_line(
            input,
            area.width as usize,
            false,
            focused,
            placeholder,
        ))
        .style(if focused {
            components::highlighted()
        } else {
            components::normal()
        }),
        area,
    );
    if focused {
        frame.set_cursor_position((
            area.x
                + (form::cursor_column(input, area.width as usize, false) as u16)
                    .min(area.width - 1),
            area.y,
        ));
    }
}

pub(super) fn editor_area(
    frame: &mut Frame<'_>,
    area: Rect,
    input: &InputState,
    offset: &mut usize,
    focused: bool,
    follow: bool,
) {
    if area.is_empty() {
        return;
    }
    let (lines, caret) = input.visual_lines(area.width.max(1) as usize);
    if focused && follow {
        *offset = (*offset).min(caret.row).max(
            caret
                .row
                .saturating_sub(area.height as usize - 1),
        );
    }
    *offset = (*offset).min(
        lines
            .len()
            .saturating_sub(area.height as usize),
    );
    let selected = input.selection_cells(area.width.max(1) as usize);
    for (index, line) in lines
        .iter()
        .enumerate()
        .skip(*offset)
        .take(area.height as usize)
    {
        let rect = Rect::new(area.x, area.y + (index - *offset) as u16, area.width, 1);
        frame.render_widget(
            Paragraph::new(safe_output(line)).style(components::normal()),
            rect,
        );
        if focused {
            for (_, range) in selected
                .iter()
                .filter(|(row, _)| *row == index)
            {
                for x in range.clone() {
                    if x < area.width as usize {
                        let cell = &mut frame.buffer_mut()[(area.x + x as u16, rect.y)];
                        cell.set_style(crate::tui::theme::selection_style(cell.style()));
                    }
                }
            }
        }
    }
    if focused && let Some(point) = caret.in_viewport(area, *offset) {
        frame.set_cursor_position(point);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn state() -> State {
        State::new("session".into(), "/project".into())
    }

    #[test]
    fn exact_original_and_multiline_draft_survive_unknown_save_result() {
        let mut state = state();
        state.pending = Some(Pending::Read);
        state.receive(Response::Resources(Snapshot::Skill {
            name: "review".into(),
            content: "  original\n\n".into(),
            editable: true,
        }));
        state.editor.as_mut().unwrap().body = Source::new("  changed\nsecond  \n");
        let Action::Request(Request::Resources {
            expected_workspace,
            action: ResourceAction::SaveSkill {
                original, content, ..
            },
            ..
        }) = state.save()
        else {
            panic!("save request expected")
        };
        assert_eq!(expected_workspace.as_deref(), Some("/project"));
        assert_eq!(original.as_deref(), Some("  original\n\n"));
        assert_eq!(content, "  changed\nsecond  \n");
        state.error("acknowledgement lost".into());
        assert_eq!(
            state
                .editor
                .as_ref()
                .unwrap()
                .body
                .raw(),
            content
        );
        assert!(matches!(
            state.handle(&Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE
            ))),
            Action::Close
        ));
        assert_eq!(
            state
                .editor
                .as_ref()
                .unwrap()
                .body
                .raw(),
            content
        );
    }

    #[test]
    fn imported_skill_is_readonly_but_loads_into_the_sampled_session() {
        let mut state = state();
        state.pending = Some(Pending::Read);
        state.receive(Response::Resources(Snapshot::Skill {
            name: "imported".into(),
            content: "body".into(),
            editable: false,
        }));
        state.handle(&Event::Paste("change".into()));
        assert_eq!(
            state
                .editor
                .as_ref()
                .unwrap()
                .body
                .raw(),
            "body"
        );
        assert!(matches!(state.save(), Action::None));
        assert!(matches!(
            state.load(),
            Action::Request(Request::SessionAction {
                session,
                action: SessionAction::LoadSkill { name },
            }) if session == "session" && name == "imported"
        ));
    }

    #[test]
    fn crlf_and_tabs_round_trip_without_blocking_load_or_rewriting_source() {
        let raw = "---\r\nname: review\r\n---\r\n\r\n\t保持原有缩进\r\n";
        let mut state = state();
        state.pending = Some(Pending::Read);
        state.receive(Response::Resources(Snapshot::Skill {
            name: "review".into(),
            content: raw.into(),
            editable: true,
        }));
        let editor = state.editor.as_ref().unwrap();
        assert_eq!(editor.body.raw(), raw);
        assert!(
            !editor
                .body
                .input
                .text
                .contains(['\r', '\t'])
        );
        assert!(matches!(
            state.load(),
            Action::Request(Request::SessionAction { .. })
        ));
        state.receive(Response::Report(orbit_protocol::Report {
            title: String::new(),
            text: "loaded".into(),
        }));
        let Action::Request(Request::Resources {
            action: ResourceAction::SaveSkill {
                original, content, ..
            },
            ..
        }) = state.save()
        else {
            panic!("save request expected")
        };
        assert_eq!(original.as_deref(), Some(raw));
        assert_eq!(content, raw);
        state.error("retry".into());
        state.handle(&Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::End,
            KeyModifiers::CONTROL,
        )));
        state.handle(&Event::Paste("\t新步骤\r\n".into()));
        let Action::Request(Request::Resources {
            action: ResourceAction::SaveSkill {
                original, content, ..
            },
            ..
        }) = state.save()
        else {
            panic!("save request expected")
        };
        assert_eq!(original.as_deref(), Some(raw));
        assert_eq!(content, format!("{raw}\t新步骤\r\n"));
    }

    #[test]
    fn failed_create_keeps_full_draft_and_does_not_enable_loading_unsaved_content() {
        let mut state = state();
        state.create();
        state
            .editor
            .as_mut()
            .unwrap()
            .name
            .set("new-skill".into());
        assert!(matches!(state.save(), Action::Request(_)));
        state.receive(Response::Error("duplicate skill".into()));
        assert_eq!(state.editor.as_ref().unwrap().name.text, "new-skill");
        assert!(matches!(state.load(), Action::None));
    }

    #[test]
    fn refreshing_external_original_keeps_the_unsaved_body() {
        let mut state = state();
        state.pending = Some(Pending::Read);
        state.receive(Response::Resources(Snapshot::Skill {
            name: "review".into(),
            content: "before".into(),
            editable: true,
        }));
        state.editor.as_mut().unwrap().body = Source::new("my draft");
        assert!(matches!(
            state.activate(Hit::Refresh),
            Action::Request(Request::Resources {
                action: ResourceAction::ReadSkill { .. },
                ..
            })
        ));
        state.receive(Response::Resources(Snapshot::Skill {
            name: "review".into(),
            content: "external change".into(),
            editable: true,
        }));
        let editor = state.editor.as_ref().unwrap();
        assert_eq!(editor.body.raw(), "my draft");
        assert_eq!(editor.original.as_deref(), Some("external change"));
    }

    #[test]
    fn editable_body_wheel_scroll_stays_put_until_keyboard_navigation() {
        use ratatui::{Terminal, backend::TestBackend};
        let mut state = state();
        state.create();
        state.editor.as_mut().unwrap().focus = 1;
        let text: String = (0..60)
            .map(|line| format!("line {line}\n"))
            .collect();
        let editor = state.editor.as_mut().unwrap();
        editor.body = Source::new(&text);
        editor
            .body
            .input
            .set_cursor(editor.body.input.text.len());
        let mut terminal = Terminal::new(TestBackend::new(100, 28)).unwrap();
        terminal
            .draw(|frame| state.draw(frame, frame.area()))
            .unwrap();
        let before = state.editor.as_ref().unwrap().offset;
        let body = state.body_area;
        state.handle(&Event::Mouse(crossterm::event::MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: body.x,
            row: body.y,
            modifiers: KeyModifiers::NONE,
        }));
        let manual = state.editor.as_ref().unwrap().offset;
        assert!(manual < before);
        terminal
            .draw(|frame| state.draw(frame, frame.area()))
            .unwrap();
        assert_eq!(state.editor.as_ref().unwrap().offset, manual);
        state.handle(&Event::Key(crossterm::event::KeyEvent::new(
            KeyCode::End,
            KeyModifiers::CONTROL,
        )));
        terminal
            .draw(|frame| state.draw(frame, frame.area()))
            .unwrap();
        assert!(state.editor.as_ref().unwrap().offset > manual);
    }
}
