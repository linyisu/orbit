//! Workspace files stay in the session transport, including SSH and WSL edits.
use std::ops::Range;

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use orbit_protocol::files::{Action as FileAction, Entry, EntryKind, Snapshot};
use ratatui::{Frame, layout::Rect, style::Style, text::Line, widgets::Paragraph};

use super::{Action, Panel};
use crate::{
    tui::{
        code,
        components::{self, Control},
        input::InputState,
        render::safe_output,
        theme::THEME,
    },
    workspace::protocol::{Request, Response},
};

const MAX_EDIT_BYTES: usize = 1_024 * 1_024;

#[derive(Debug)]
struct Marker {
    range: Range<usize>,
    original: String,
}

/// InputState is intentionally a normalized prompt editor. File source keeps a
/// reversible map for existing tabs and line endings instead of saving its display.
#[derive(Debug, Default)]
pub(super) struct Source {
    pub(super) input: InputState,
    markers: Vec<Marker>,
    crlf: bool,
}

impl Source {
    pub(super) fn new(raw: &str) -> Self {
        let mut input = InputState::default();
        let mut markers = Vec::new();
        let mut chars = raw.chars().peekable();
        while let Some(ch) = chars.next() {
            let start = input.text.len();
            let original = match ch {
                '\t' => {
                    input.text.push_str("    ");
                    Some("\t".to_string())
                }
                '\r' => {
                    input.text.push('\n');
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                        Some("\r\n".to_string())
                    } else {
                        Some("\r".to_string())
                    }
                }
                '\n' => {
                    input.text.push('\n');
                    Some("\n".to_string())
                }
                _ => {
                    if ch.is_control() {
                        input.text.push('\u{fffd}');
                        Some(ch.to_string())
                    } else {
                        input.text.push(ch);
                        None
                    }
                }
            };
            if let Some(original) = original {
                markers.push(Marker {
                    range: start..input.text.len(),
                    original,
                });
            }
        }
        Self {
            input,
            markers,
            crlf: raw.contains("\r\n"),
        }
    }

    pub(super) fn raw(&self) -> String {
        let mut result = String::new();
        let mut start = 0;
        for marker in &self.markers {
            let plain = &self.input.text[start..marker.range.start];
            if self.crlf {
                result.push_str(&plain.replace('\n', "\r\n"));
            } else {
                result.push_str(plain);
            }
            result.push_str(&marker.original);
            start = marker.range.end;
        }
        let plain = &self.input.text[start..];
        if self.crlf {
            result.push_str(&plain.replace('\n', "\r\n"));
        } else {
            result.push_str(plain);
        }
        result
    }

    pub(super) fn edit(&mut self, event: &Event, width: usize) -> bool {
        let before = self.input.text.clone();
        let fragment = if let Event::Paste(raw) = event {
            Some(Self::new(raw))
        } else {
            None
        };
        let insert_at = self
            .input
            .selection()
            .map_or(self.input.cursor, |r| r.start);
        let replace_end = self
            .input
            .selection()
            .map_or(self.input.cursor, |r| r.end);
        let handled = match (&fragment, event) {
            (Some(fragment), _) => {
                self.input.insert(&fragment.input.text);
                true
            }
            (_, Event::Key(key)) if key.code == KeyCode::Enter => {
                self.input.insert("\n");
                true
            }
            _ => self.input.edit(event, width),
        };
        if before == self.input.text && fragment.is_none() {
            return handled;
        }
        let mut prefix = before
            .bytes()
            .zip(self.input.text.bytes())
            .take_while(|(a, b)| a == b)
            .count()
            .min(insert_at);
        while !before.is_char_boundary(prefix) || !self.input.text.is_char_boundary(prefix) {
            prefix -= 1;
        }
        let mut suffix = before[prefix..]
            .bytes()
            .rev()
            .zip(self.input.text[prefix..].bytes().rev())
            .take_while(|(a, b)| a == b)
            .count();
        while !before.is_char_boundary(before.len() - suffix)
            || !self
                .input
                .text
                .is_char_boundary(self.input.text.len() - suffix)
        {
            suffix -= 1;
        }
        let (prefix, old_end) = if fragment.is_some() {
            (insert_at, replace_end)
        } else {
            (prefix, before.len() - suffix)
        };
        let shift = self.input.text.len() as isize - before.len() as isize;
        self.markers.retain_mut(|marker| {
            if marker.range.end <= prefix {
                true
            } else if marker.range.start >= old_end {
                marker.range = marker
                    .range
                    .start
                    .saturating_add_signed(shift)
                    ..marker
                        .range
                        .end
                        .saturating_add_signed(shift);
                true
            } else {
                false
            }
        });
        if let Some(fragment) = fragment {
            self.markers.extend(
                fragment
                    .markers
                    .into_iter()
                    .map(|mut marker| {
                        marker.range = marker.range.start + insert_at..marker.range.end + insert_at;
                        marker
                    }),
            );
            self.markers
                .sort_by_key(|marker| marker.range.start);
        }
        handled
    }
}

#[derive(Debug)]
struct Document {
    path: String,
    original: String,
    source: Source,
}

#[derive(Debug)]
enum Pending {
    List { path: String, append: bool },
    Read { path: String },
    Compare,
    Save { content: String },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    Path,
    List,
    Editor,
    Disk,
}

#[derive(Debug)]
enum Confirm {
    Close,
    Open(String),
    Overwrite,
}

#[derive(Debug)]
pub(super) struct State {
    session: String,
    workspace: String,
    initial_path: Option<String>,
    directory: String,
    path: InputState,
    entries: Vec<Entry>,
    selected: usize,
    list_offset: usize,
    next: Option<usize>,
    hidden: bool,
    readonly: bool,
    truncated: bool,
    document: Option<Document>,
    disk: Option<String>,
    pending: Option<Pending>,
    unknown_save: bool,
    confirmation: Option<Confirm>,
    message: Option<String>,
    focus: Focus,
    editor_offset: usize,
    disk_offset: usize,
    follow_cursor: bool,
    list_area: Rect,
    editor_area: Rect,
    disk_area: Rect,
    path_area: Rect,
    hits: Vec<(Rect, Focus)>,
    actions: Vec<(Rect, char)>,
    row_hits: Vec<(Rect, usize)>,
    revision: u64,
    cached: Option<(u64, u16, Vec<Line<'static>>)>,
}

impl State {
    pub(super) fn new(session: String, workspace: String) -> Self {
        Self {
            session,
            workspace,
            initial_path: None,
            directory: ".".into(),
            path: InputState::default(),
            entries: Vec::new(),
            selected: 0,
            list_offset: 0,
            next: None,
            hidden: false,
            readonly: false,
            truncated: false,
            document: None,
            disk: None,
            pending: None,
            unknown_save: false,
            confirmation: None,
            message: None,
            focus: Focus::List,
            editor_offset: 0,
            disk_offset: 0,
            follow_cursor: true,
            list_area: Rect::default(),
            editor_area: Rect::default(),
            disk_area: Rect::default(),
            path_area: Rect::default(),
            hits: Vec::new(),
            actions: Vec::new(),
            row_hits: Vec::new(),
            revision: 0,
            cached: None,
        }
    }

    pub(super) fn at(mut self, path: Option<String>) -> Self {
        self.initial_path = path;
        self
    }

    pub(super) fn initial(&mut self) -> Request {
        if let Some(path) = self.initial_path.take() {
            if path.ends_with('/') || matches!(path.as_str(), "." | "..") {
                self.list(path, false)
            } else {
                self.read(path)
            }
        } else {
            self.list(self.directory.clone(), false)
        }
    }

    fn request(&self, action: FileAction) -> Request {
        Request::Files {
            session: self.session.clone(),
            expected_workspace: Some(self.workspace.clone()),
            action,
        }
    }

    fn list(&mut self, path: String, append: bool) -> Request {
        let offset = if append { self.next.unwrap_or(0) } else { 0 };
        self.pending = Some(Pending::List {
            path: path.clone(),
            append,
        });
        self.message = None;
        self.request(FileAction::List {
            path,
            offset,
            include_hidden: self.hidden,
        })
    }

    fn read(&mut self, path: String) -> Request {
        self.pending = Some(Pending::Read { path: path.clone() });
        self.message = None;
        Request::ReadFile {
            session: self.session.clone(),
            path,
        }
    }

    fn dirty(&self) -> bool {
        self.unknown_save
            || self
                .document
                .as_ref()
                .is_some_and(|d| d.source.raw() != d.original)
    }

    fn open(&mut self, path: String) -> Action {
        if self.pending.is_some() {
            return Action::None;
        }
        if self.dirty() {
            self.confirmation = Some(Confirm::Open(path));
            Action::None
        } else {
            Action::Request(self.read(path))
        }
    }

    fn save(&mut self, original: Option<String>) -> Action {
        if self.pending.is_some() {
            return Action::None;
        }
        let Some(document) = &self.document else {
            return Action::None;
        };
        if self.readonly {
            self.message = Some("当前会话只读，无法保存".into());
            return Action::None;
        }
        if self.unknown_save && original.is_none() {
            self.message = Some("上次保存结果未确认；先按 Ctrl+R 读取磁盘版本".into());
            return Action::None;
        }
        let content = document.source.raw();
        if content.len() > MAX_EDIT_BYTES {
            self.message = Some("文本超过 1 MiB 保存上限；草稿已保留".into());
            return Action::None;
        }
        if self.disk.is_some() && original.is_none() {
            self.message = Some("磁盘存在其他版本；Ctrl+Shift+S 确认使用编辑内容替换".into());
            return Action::None;
        }
        let request = self.request(FileAction::Save {
            path: document.path.clone(),
            original: original.unwrap_or_else(|| document.original.clone()),
            content: content.clone(),
        });
        self.pending = Some(Pending::Save { content });
        self.message = None;
        Action::Request(request)
    }

    fn compare(&mut self) -> Action {
        if self.pending.is_some() {
            return Action::None;
        }
        let Some(document) = &self.document else {
            return Action::None;
        };
        let request = Request::ReadFile {
            session: self.session.clone(),
            path: document.path.clone(),
        };
        self.pending = Some(Pending::Compare);
        Action::Request(request)
    }

    fn selected(&mut self) -> Action {
        if self.pending.is_some() {
            return Action::None;
        }
        let Some(entry) = self.entries.get(self.selected) else {
            return Action::None;
        };
        let path = entry.path.clone();
        if entry.kind == EntryKind::Directory {
            Action::Request(self.list(path, false))
        } else {
            self.open(path)
        }
    }

    fn activate(&mut self, key: char) -> Action {
        if key == 'c' {
            if self.dirty() {
                self.confirmation = Some(Confirm::Close);
                return Action::None;
            }
            return Action::Close;
        }
        if self.pending.is_some() {
            return Action::None;
        }
        match key {
            's' => self.save(None),
            'r' => self.compare(),
            'h' => {
                self.hidden = !self.hidden;
                Action::Request(self.list(self.directory.clone(), false))
            }
            'n' if self.next.is_some() => Action::Request(self.list(self.directory.clone(), true)),
            'p' => {
                let parent = std::path::Path::new(&self.directory)
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .unwrap_or(std::path::Path::new("."))
                    .to_string_lossy()
                    .into_owned();
                Action::Request(self.list(parent, false))
            }
            'f' => Action::Request(self.list(self.directory.clone(), false)),
            _ => Action::None,
        }
    }

    fn reject(&mut self, error: String, uncertain: bool) {
        if matches!(self.pending.take(), Some(Pending::Save { .. })) && uncertain {
            self.unknown_save = true;
        }
        self.message = Some(error);
    }

    fn file_bytes(&mut self, encoded: String, pending: Pending) -> Action {
        let raw = match orbit_client::protocol::file_bytes(encoded)
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
        {
            Some(raw)
                if !raw
                    .chars()
                    .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')) =>
            {
                raw
            }
            _ => {
                self.message = Some("二进制或非 UTF-8 文件无法在文本编辑器中编辑".into());
                return Action::None;
            }
        };
        if raw.len() > MAX_EDIT_BYTES {
            self.message = Some("文件超过 1 MiB 编辑上限，请使用外部编辑器".into());
            return Action::None;
        }
        match pending {
            Pending::Read { path } => {
                let parent = std::path::Path::new(&path)
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                    .unwrap_or(std::path::Path::new("."))
                    .to_string_lossy()
                    .into_owned();
                self.path.set(path.clone());
                self.document = Some(Document {
                    path,
                    source: Source::new(&raw),
                    original: raw,
                });
                self.disk = None;
                self.unknown_save = false;
                self.editor_offset = 0;
                self.focus = Focus::Editor;
                self.follow_cursor = true;
                self.revision += 1;
                if self.directory != parent || self.entries.is_empty() {
                    return Action::Request(self.list(parent, false));
                }
            }
            Pending::Compare => {
                if let Some(document) = &mut self.document {
                    if self.unknown_save {
                        document.original = raw.clone();
                        self.unknown_save = false;
                    }
                    if raw == document.original {
                        self.disk = None;
                        self.message = Some(
                            if document.source.raw() == raw {
                                "磁盘与编辑内容一致"
                            } else {
                                "磁盘未变更，草稿可安全保存"
                            }
                            .into(),
                        );
                    } else {
                        self.disk = Some(raw);
                        self.disk_offset = 0;
                        self.message = Some("磁盘已有其他修改；草稿保留在上方".into());
                    }
                }
            }
            _ => self.message = Some("文件响应与当前操作不匹配".into()),
        }
        Action::None
    }

    fn source_rows(&mut self, width: u16) -> &[Line<'static>] {
        if !self
            .cached
            .as_ref()
            .is_some_and(|(revision, cached_width, _)| {
                *revision == self.revision && *cached_width == width
            })
        {
            let mut rows = Vec::new();
            if let Some(document) = &self.document {
                let mut renderer = code::Renderer::for_path(&document.path);
                for line in document.source.input.text.split('\n') {
                    rows.extend(renderer.render(
                        line,
                        code::Options {
                            width: Some(width.max(2)),
                            ..Default::default()
                        },
                    ));
                }
            }
            self.cached = Some((self.revision, width, rows));
        }
        &self.cached.as_ref().unwrap().2
    }

    fn draw_editor(&mut self, frame: &mut Frame<'_>, area: Rect) {
        self.editor_area = area;
        if area.is_empty() {
            return;
        }
        let Some(document) = &self.document else {
            frame.render_widget(
                Paragraph::new(
                    "选择文件查看和编辑。\nCtrl+L 输入路径；支持本地、SSH 与 WSL 工作区。",
                )
                .style(components::muted()),
                area,
            );
            return;
        };
        let cursor = document
            .source
            .input
            .visual_lines(area.width.max(2) as usize)
            .1;
        if self.follow_cursor {
            if cursor.row < self.editor_offset {
                self.editor_offset = cursor.row;
            }
            if cursor.row >= self.editor_offset + area.height as usize {
                self.editor_offset = cursor
                    .row
                    .saturating_sub(area.height.saturating_sub(1) as usize);
            }
        }
        let offset = self.editor_offset;
        let rows = self.source_rows(area.width);
        self.editor_offset = offset.min(
            rows.len()
                .saturating_sub(area.height as usize),
        );
        let lines = self
            .cached
            .as_ref()
            .unwrap()
            .2
            .iter()
            .skip(self.editor_offset)
            .take(area.height as usize)
            .cloned()
            .collect::<Vec<_>>();
        frame.render_widget(Paragraph::new(lines), area);
        if self.focus == Focus::Editor {
            if let Some(document) = &self.document {
                for (row, columns) in document
                    .source
                    .input
                    .selection_cells(area.width.max(2) as usize)
                {
                    if let Some(y) = row
                        .checked_sub(self.editor_offset)
                        .filter(|y| *y < area.height as usize)
                    {
                        for x in columns.filter(|x| *x < area.width as usize) {
                            frame.buffer_mut()[(area.x + x as u16, area.y + y as u16)].set_style(
                                Style::default()
                                    .fg(THEME.selection_foreground)
                                    .bg(THEME.selection),
                            );
                        }
                    }
                }
            }
            if let Some(position) = cursor.in_viewport(area, self.editor_offset) {
                frame.set_cursor_position(position);
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
            if let Some(confirmation) = self.confirmation.take() {
                return match key.code {
                    KeyCode::Char('y' | 'Y') => match confirmation {
                        Confirm::Close => {
                            // Management surfaces are retained when hidden. An
                            // explicit discard must therefore clear this draft.
                            self.document = None;
                            self.disk = None;
                            self.unknown_save = false;
                            self.focus = Focus::List;
                            self.cached = None;
                            self.revision += 1;
                            Action::Close
                        }
                        Confirm::Open(path) => {
                            self.unknown_save = false;
                            Action::Request(self.read(path))
                        }
                        Confirm::Overwrite => self.save(self.disk.clone()),
                    },
                    KeyCode::Esc | KeyCode::Char('n' | 'N') => Action::None,
                    _ => {
                        self.confirmation = Some(confirmation);
                        Action::None
                    }
                };
            }
            let control = key
                .modifiers
                .contains(KeyModifiers::CONTROL);
            if control && matches!(key.code, KeyCode::Char('v' | 'V')) {
                match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.get_text()) {
                    Ok(text) => {
                        return self.handle(&Event::Paste(text));
                    }
                    Err(error) => self.message = Some(format!("无法读取剪贴板文字：{error}")),
                }
                return Action::None;
            }
            if control && matches!(key.code, KeyCode::Char('c' | 'C')) {
                return self
                    .document
                    .as_ref()
                    .map_or(Action::None, |document| {
                        Action::Copy(
                            document
                                .source
                                .input
                                .selected_text()
                                .map_or_else(|| document.source.raw(), str::to_owned),
                        )
                    });
            }
            if key.code == KeyCode::Esc {
                return self.activate('c');
            }
            if control && matches!(key.code, KeyCode::Char('l' | 'L')) {
                self.focus = Focus::Path;
                self.path.select_all();
                return Action::None;
            }
            if control && matches!(key.code, KeyCode::Char('s' | 'S')) {
                if key
                    .modifiers
                    .contains(KeyModifiers::SHIFT)
                    && self.disk.is_some()
                {
                    self.confirmation = Some(Confirm::Overwrite);
                    return Action::None;
                }
                return self.save(None);
            }
            if control && matches!(key.code, KeyCode::Char('r' | 'R')) {
                return self.compare();
            }
            if key.code == KeyCode::F(5) {
                return self.activate('f');
            }
            if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
                self.focus = if key.code == KeyCode::BackTab {
                    match self.focus {
                        Focus::Path if self.disk.is_some() => Focus::Disk,
                        Focus::Path if self.document.is_some() => Focus::Editor,
                        Focus::Path => Focus::List,
                        Focus::List => Focus::Path,
                        Focus::Editor => Focus::List,
                        Focus::Disk => Focus::Editor,
                    }
                } else {
                    match self.focus {
                        Focus::Path => Focus::List,
                        Focus::List if self.document.is_some() => Focus::Editor,
                        Focus::List => Focus::Path,
                        Focus::Editor if self.disk.is_some() => Focus::Disk,
                        Focus::Editor | Focus::Disk => Focus::Path,
                    }
                };
                return Action::None;
            }
            match self.focus {
                Focus::Path => {
                    if key.code == KeyCode::Enter {
                        if self.pending.is_some() {
                            return Action::None;
                        }
                        if key
                            .modifiers
                            .contains(KeyModifiers::ALT)
                            || self.path.text.trim().ends_with('/')
                            || matches!(self.path.text.trim(), "." | "..")
                        {
                            return Action::Request(self.list(self.path.text.trim().into(), false));
                        }
                        return self.open(self.path.text.trim().into());
                    }
                    self.path
                        .edit(event, self.path_area.width.max(2) as usize);
                }
                Focus::List => match key.code {
                    KeyCode::Enter => return self.selected(),
                    KeyCode::Up => self.selected = self.selected.saturating_sub(1),
                    KeyCode::Down => {
                        self.selected = self
                            .selected
                            .saturating_add(1)
                            .min(self.entries.len().saturating_sub(1))
                    }
                    KeyCode::PageUp => {
                        self.selected = self
                            .selected
                            .saturating_sub(self.list_area.height as usize)
                    }
                    KeyCode::PageDown => {
                        self.selected = self
                            .selected
                            .saturating_add(self.list_area.height as usize)
                            .min(self.entries.len().saturating_sub(1))
                    }
                    KeyCode::Backspace => return self.activate('p'),
                    KeyCode::Char('h' | 'H') => return self.activate('h'),
                    KeyCode::Char('n' | 'N') => return self.activate('n'),
                    _ => {}
                },
                Focus::Editor if self.pending.is_none() && !self.readonly => {
                    if let Some(document) = &mut self.document {
                        let before = document.source.input.text.clone();
                        document
                            .source
                            .edit(event, self.editor_area.width.max(2) as usize);
                        if document.source.input.text != before {
                            self.revision += 1;
                        }
                        self.follow_cursor = true;
                    }
                }
                Focus::Disk => match key.code {
                    KeyCode::Down | KeyCode::PageDown => {
                        self.disk_offset =
                            self.disk_offset
                                .saturating_add(if key.code == KeyCode::Down {
                                    1
                                } else {
                                    self.disk_area.height as usize
                                })
                    }
                    KeyCode::Up | KeyCode::PageUp => {
                        self.disk_offset =
                            self.disk_offset
                                .saturating_sub(if key.code == KeyCode::Up {
                                    1
                                } else {
                                    self.disk_area.height as usize
                                })
                    }
                    _ => {}
                },
                _ => {}
            }
        } else if self.confirmation.is_some() {
            return Action::None;
        } else if let Event::Paste(_) = event {
            if self.focus == Focus::Path {
                self.path
                    .edit(event, self.path_area.width.max(2) as usize);
            } else if self.focus == Focus::Editor
                && self.pending.is_none()
                && !self.readonly
                && let Some(document) = &mut self.document
            {
                document
                    .source
                    .edit(event, self.editor_area.width.max(2) as usize);
                self.revision += 1;
                self.follow_cursor = true;
            }
        } else if let Event::Mouse(mouse) = event {
            let point = (mouse.column, mouse.row).into();
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some((_, action)) = self
                        .actions
                        .iter()
                        .find(|(area, _)| area.contains(point))
                    {
                        return self.activate(*action);
                    }
                    if let Some((_, index)) = self
                        .row_hits
                        .iter()
                        .find(|(area, _)| area.contains(point))
                    {
                        self.selected = *index;
                        self.focus = Focus::List;
                        return self.selected();
                    }
                    if let Some((_, focus)) = self
                        .hits
                        .iter()
                        .find(|(area, _)| area.contains(point))
                    {
                        self.focus = *focus;
                    }
                    if self.focus == Focus::Editor
                        && self.editor_area.contains(point)
                        && let Some(document) = &mut self.document
                    {
                        let index = document.source.input.position_at(
                            (mouse.column - self.editor_area.x) as usize,
                            (mouse.row - self.editor_area.y) as usize + self.editor_offset,
                            self.editor_area.width.max(2) as usize,
                        );
                        document.source.input.set_cursor(index);
                        self.follow_cursor = true;
                    }
                }
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    let delta = if mouse.kind == MouseEventKind::ScrollUp {
                        -3
                    } else {
                        3
                    };
                    if self.editor_area.contains(point) {
                        self.editor_offset = self
                            .editor_offset
                            .saturating_add_signed(delta);
                        self.follow_cursor = false;
                    } else if self.disk_area.contains(point) {
                        self.disk_offset = self
                            .disk_offset
                            .saturating_add_signed(delta);
                    } else if self.list_area.contains(point) {
                        self.selected = self
                            .selected
                            .saturating_add_signed(delta)
                            .min(self.entries.len().saturating_sub(1));
                    }
                }
                _ => {}
            }
        }
        Action::None
    }

    fn receive(&mut self, response: Response) -> Action {
        if let Response::Error(error) = response {
            self.reject(error, false);
            return Action::None;
        }
        let Some(pending) = self.pending.take() else {
            return Action::None;
        };
        match (pending, response) {
            (
                Pending::List {
                    path: requested,
                    append,
                },
                Response::Files(Snapshot::Directory {
                    path,
                    entries,
                    next_offset,
                    scan_truncated,
                    readonly,
                }),
            ) if requested == path => {
                if !append {
                    self.entries.clear();
                    self.selected = 0;
                    self.list_offset = 0;
                }
                self.entries.extend(entries);
                self.directory = path.clone();
                self.next = next_offset;
                self.truncated = scan_truncated;
                self.readonly = readonly;
                if self.document.is_none() {
                    self.path.set(path);
                }
            }
            (pending @ (Pending::Read { .. } | Pending::Compare), Response::File(encoded)) => {
                return self.file_bytes(encoded, pending);
            }
            (
                Pending::Save { content: requested },
                Response::Files(Snapshot::Saved { path, content }),
            ) => {
                if let Some(document) = self
                    .document
                    .as_mut()
                    .filter(|d| d.path == path)
                {
                    if requested != content {
                        self.unknown_save = true;
                        self.message = Some("保存回执与请求内容不一致；请比较磁盘版本".into());
                    } else {
                        document.original = content;
                        self.unknown_save = false;
                        self.disk = None;
                        self.message = Some("已保存".into());
                    }
                }
            }
            (Pending::Save { .. }, _) => {
                self.unknown_save = true;
                self.message = Some("保存结果未确认；草稿已保留，请比较磁盘版本".into());
            }
            _ => self.message = Some("服务返回了不匹配的文件响应，请刷新后重试".into()),
        }
        Action::None
    }

    fn error(&mut self, error: String) {
        self.reject(error, true);
    }

    fn invalidate_geometry(&mut self) {
        self.list_area = Rect::default();
        self.editor_area = Rect::default();
        self.disk_area = Rect::default();
        self.path_area = Rect::default();
        self.hits.clear();
        self.actions.clear();
        self.row_hits.clear();
    }

    fn draw(&mut self, frame: &mut Frame<'_>, area: Rect) {
        self.invalidate_geometry();
        if area.is_empty() {
            return;
        }
        let title = format!(
            "文件  {}{}{}",
            self.document
                .as_ref()
                .map_or(self.directory.as_str(), |d| d.path.as_str()),
            if self.dirty() { " · 未保存" } else { "" },
            if self.pending.is_some() {
                " · 处理中…"
            } else {
                ""
            }
        );
        components::title(frame, area, title);
        if area.height < 3 {
            return;
        }
        self.path_area = Rect::new(area.x, area.y + 1, area.width, 1);
        let label = if self.focus == Focus::Path {
            format!("路径 > {}", safe_output(&self.path.text))
        } else {
            format!("路径   {}", safe_output(&self.path.text))
        };
        frame.render_widget(
            Paragraph::new(label).style(if self.focus == Focus::Path {
                components::accent()
            } else {
                components::muted()
            }),
            self.path_area,
        );
        self.hits
            .push((self.path_area, Focus::Path));
        let footer_height = 3.min(area.height.saturating_sub(2));
        let footer = Rect::new(
            area.x,
            area.bottom() - footer_height,
            area.width,
            footer_height,
        );
        let body = Rect::new(
            area.x,
            area.y + 2,
            area.width,
            area.height
                .saturating_sub(2 + footer_height),
        );
        let (list, document) = if body.width >= 64 {
            let width = 28.min(body.width / 3);
            (
                Rect::new(body.x, body.y, width, body.height),
                Rect::new(
                    body.x + width + 1,
                    body.y,
                    body.width.saturating_sub(width + 1),
                    body.height,
                ),
            )
        } else if self.focus == Focus::List || self.document.is_none() {
            (body, Rect::default())
        } else {
            (Rect::default(), body)
        };
        self.list_area = list;
        self.hits.push((list, Focus::List));
        if !list.is_empty() {
            if self.selected < self.list_offset {
                self.list_offset = self.selected;
            }
            if self.selected >= self.list_offset + list.height as usize {
                self.list_offset = self
                    .selected
                    .saturating_sub(list.height.saturating_sub(1) as usize);
            }
            for (row, (index, entry)) in self
                .entries
                .iter()
                .enumerate()
                .skip(self.list_offset)
                .take(list.height as usize)
                .enumerate()
            {
                let target = Rect::new(list.x, list.y + row as u16, list.width, 1);
                let name = std::path::Path::new(&entry.path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy();
                let label = format!(
                    "{} {}",
                    if entry.kind == EntryKind::Directory {
                        "+"
                    } else {
                        " "
                    },
                    safe_output(&name)
                );
                frame.render_widget(
                    Paragraph::new(label).style(if index == self.selected {
                        components::highlighted()
                    } else {
                        components::normal()
                    }),
                    target,
                );
                self.row_hits.push((target, index));
            }
        }
        if let Some(disk) = &self.disk {
            let editor_height = document.height / 2;
            let editor = Rect::new(document.x, document.y, document.width, editor_height);
            let label = Rect::new(
                document.x,
                document.y + editor_height,
                document.width,
                1.min(
                    document
                        .height
                        .saturating_sub(editor_height),
                ),
            );
            self.disk_area = Rect::new(
                document.x,
                label.bottom(),
                document.width,
                document
                    .bottom()
                    .saturating_sub(label.bottom()),
            );
            frame.render_widget(
                Paragraph::new("磁盘版本 · 只读（Ctrl+Shift+S 使用编辑内容替换）")
                    .style(components::accent()),
                label,
            );
            let lines = code::file_lines(
                self.document
                    .as_ref()
                    .map_or("", |d| &d.path),
                disk,
            );
            let wrapped: Vec<_> = lines
                .iter()
                .flat_map(|line| {
                    code::render_line(
                        line,
                        code::Options {
                            width: Some(self.disk_area.width.max(2)),
                            ..Default::default()
                        },
                    )
                })
                .collect();
            self.disk_offset = self.disk_offset.min(
                wrapped
                    .len()
                    .saturating_sub(self.disk_area.height as usize),
            );
            frame.render_widget(
                Paragraph::new(
                    wrapped
                        .into_iter()
                        .skip(self.disk_offset)
                        .take(self.disk_area.height as usize)
                        .collect::<Vec<_>>(),
                ),
                self.disk_area,
            );
            self.hits
                .push((self.disk_area, Focus::Disk));
            self.draw_editor(frame, editor);
        } else {
            self.draw_editor(frame, document);
        }
        self.hits
            .push((self.editor_area, Focus::Editor));
        let status = match &self.confirmation {
            Some(Confirm::Close) => "放弃未保存或未确认的编辑并关闭？Y 放弃 · N / Esc 保留".into(),
            Some(Confirm::Open(_)) => "放弃当前编辑并打开另一文件？Y 放弃 · N / Esc 保留".into(),
            Some(Confirm::Overwrite) => {
                "用编辑内容替换刚读取的磁盘版本？Y 保存 · N / Esc 取消".into()
            }
            None => self.message.clone().unwrap_or_else(|| {
                format!(
                    "{} 项{}{}{}",
                    self.entries.len(),
                    if self.next.is_some() {
                        " · N 加载更多"
                    } else {
                        ""
                    },
                    if self.truncated {
                        " · 扫描已截断"
                    } else {
                        ""
                    },
                    if self.readonly { " · 只读" } else { "" }
                )
            }),
        };
        frame.render_widget(
            Paragraph::new(safe_output(&status)).style(
                if self.message.is_some() || self.confirmation.is_some() {
                    components::accent()
                } else {
                    components::muted()
                },
            ),
            Rect {
                height: 1,
                ..footer
            },
        );
        let mut bar = components::ActionBar::new(Rect::new(
            footer.x,
            footer.y + 1,
            footer.width,
            footer.height.saturating_sub(1),
        ));
        for (label, key) in [
            ("保存 ^S", 's'),
            ("比较 ^R", 'r'),
            ("上级", 'p'),
            ("隐藏 H", 'h'),
            ("更多 N", 'n'),
            ("刷新 F5", 'f'),
            ("关闭 Esc", 'c'),
        ] {
            let disabled = (self.pending.is_some() && key != 'c')
                || (matches!(key, 's' | 'r') && self.document.is_none())
                || (key == 's' && self.readonly)
                || (key == 'n' && self.next.is_none());
            if let Some(target) = bar.button(
                frame,
                label,
                Control {
                    disabled,
                    ..Default::default()
                },
            ) {
                self.actions.push((target, key));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use crossterm::event::KeyEvent;
    use ratatui::{Terminal, backend::TestBackend};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    fn loaded(raw: &str) -> State {
        let mut state =
            State::new("session".into(), "/workspace".into()).at(Some("src/main.rs".into()));
        state.initial();
        state.receive(Response::File(STANDARD.encode(raw)));
        state.receive(Response::Files(Snapshot::Directory {
            path: "src".into(),
            entries: vec![],
            next_offset: None,
            scan_truncated: false,
            readonly: false,
        }));
        state
    }

    #[test]
    fn edits_preserve_existing_tabs_mixed_line_endings_and_pasted_source() {
        let mut source = Source::new("\tlet old = 1;\r\nnext\nlast\r");
        assert_eq!(source.raw(), "\tlet old = 1;\r\nnext\nlast\r");
        source
            .input
            .set_cursor(source.input.text.find("old").unwrap());
        source.edit(&Event::Paste("新\t值".into()), 80);
        assert_eq!(source.raw(), "\tlet 新\t值old = 1;\r\nnext\nlast\r");
        source
            .input
            .set_cursor(source.input.text.len());
        source.edit(&key(KeyCode::Enter, KeyModifiers::NONE), 80);
        assert!(source.raw().ends_with("last\r\r\n"));
    }

    #[test]
    fn untouched_controls_are_visible_and_round_trip_without_terminal_escapes() {
        let raw = "before\u{1b}[31m\u{7}after\r\n";
        let mut source = Source::new(raw);
        assert_eq!(source.raw(), raw);
        assert!(!source.input.text.contains('\u{1b}'));
        assert!(!source.input.text.contains('\u{7}'));
        source
            .input
            .set_cursor(source.input.text.len());
        source.edit(&Event::Paste("\tend\u{1b}".into()), 80);
        assert_eq!(source.raw(), format!("{raw}\tend\u{1b}"));
    }

    #[test]
    fn save_uses_original_source_and_unknown_reply_requires_comparison() {
        let mut state = loaded("\told\r\n");
        state.handle(&Event::Paste("new ".into()));
        let Action::Request(Request::Files {
            expected_workspace,
            action: FileAction::Save {
                original, content, ..
            },
            ..
        }) = state.save(None)
        else {
            panic!("save expected");
        };
        assert_eq!(expected_workspace.as_deref(), Some("/workspace"));
        assert_eq!(original, "\told\r\n");
        assert_eq!(content, "new \told\r\n");
        state.error("connection lost".into());
        assert!(state.dirty());
        assert!(state.unknown_save);
        assert!(matches!(state.save(None), Action::None));
        state.compare();
        state.receive(Response::File(STANDARD.encode(content)));
        assert!(!state.unknown_save);
        assert!(!state.dirty());
    }

    #[test]
    fn conflict_and_dirty_close_keep_the_draft_until_explicit_confirmation() {
        let mut state = loaded("old");
        state.handle(&Event::Paste("draft ".into()));
        state.save(None);
        state.receive(Response::Error("file changed".into()));
        assert!(!state.unknown_save);
        assert!(state.dirty());
        state.compare();
        state.receive(Response::File(STANDARD.encode("external")));
        assert_eq!(state.disk.as_deref(), Some("external"));
        assert_eq!(
            state
                .document
                .as_ref()
                .unwrap()
                .source
                .raw(),
            "draft old"
        );
        assert!(matches!(
            state.handle(&key(KeyCode::Esc, KeyModifiers::NONE)),
            Action::None
        ));
        assert!(matches!(
            state.handle(&key(KeyCode::Char('n'), KeyModifiers::NONE)),
            Action::None
        ));
        state.handle(&key(KeyCode::Esc, KeyModifiers::NONE));
        assert!(matches!(
            state.handle(&key(KeyCode::Char('y'), KeyModifiers::NONE)),
            Action::Close
        ));
        assert!(!state.dirty());
        assert!(state.document.is_none());
    }

    #[test]
    fn binary_is_not_editable_and_resizing_never_changes_raw_source() {
        let mut state = State::new("session".into(), "/workspace".into()).at(Some("binary".into()));
        state.initial();
        state.receive(Response::File(STANDARD.encode([0, 255, 1])));
        assert!(state.document.is_none());
        let raw = "\tlet 中文 = 1;\r\n".repeat(30);
        let mut state = loaded(&raw);
        for (width, height) in [(100, 30), (28, 10), (10, 5), (1, 1)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| state.draw(frame, frame.area()))
                .unwrap();
        }
        assert_eq!(state.document.unwrap().source.raw(), raw);
    }
}
