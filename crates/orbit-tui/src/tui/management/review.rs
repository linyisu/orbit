//! Git index operations and Orbit undo use the exact state the user reviewed.
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use orbit_protocol::review::{Action as ReviewAction, Checkpoint, Entry, Kind, Snapshot};
use ratatui::{Frame, layout::Rect, text::Line, widgets::Paragraph};

use super::{Action, Panel};
use crate::{
    tui::{
        code,
        components::{self, Control},
        input::InputState,
        render::safe_output,
    },
    workspace::protocol::{Request, Response},
};

const KINDS: &[(Kind, &str)] = &[
    (Kind::Unstaged, "未暂存"),
    (Kind::Staged, "已暂存"),
    (Kind::Untracked, "未跟踪"),
    (Kind::Checkpoint, "Orbit 写入"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Focus {
    List,
    Diff,
    Path,
}
#[derive(Debug)]
enum Pending {
    List { append: bool },
    Diff { path: String },
    Index,
    Undo,
}

#[derive(Debug)]
pub(super) struct State {
    session: String,
    workspace: String,
    kind: Kind,
    initial_path: Option<String>,
    entries: Vec<Entry>,
    selected: usize,
    list_offset: usize,
    next: Option<usize>,
    branch: Option<String>,
    path: InputState,
    reviewed_path: Option<String>,
    patch: String,
    checkpoint: Option<Checkpoint>,
    receipt: Option<String>,
    pending: Option<Pending>,
    confirm_undo: bool,
    message: Option<String>,
    focus: Focus,
    diff_offset: usize,
    list_area: Rect,
    diff_area: Rect,
    path_area: Rect,
    row_hits: Vec<(Rect, usize)>,
    kind_hits: Vec<(Rect, Kind)>,
    action_hits: Vec<(Rect, char)>,
    cache: Option<(u16, Vec<Line<'static>>)>,
}

impl State {
    pub(super) fn new(session: String, workspace: String) -> Self {
        Self {
            session,
            workspace,
            kind: Kind::Unstaged,
            initial_path: None,
            entries: Vec::new(),
            selected: 0,
            list_offset: 0,
            next: None,
            branch: None,
            path: InputState::default(),
            reviewed_path: None,
            patch: String::new(),
            checkpoint: None,
            receipt: None,
            pending: None,
            confirm_undo: false,
            message: None,
            focus: Focus::List,
            diff_offset: 0,
            list_area: Rect::default(),
            diff_area: Rect::default(),
            path_area: Rect::default(),
            row_hits: Vec::new(),
            kind_hits: Vec::new(),
            action_hits: Vec::new(),
            cache: None,
        }
    }
    pub(super) fn with_kind(mut self, kind: Kind) -> Self {
        self.kind = kind;
        self
    }
    pub(super) fn at(mut self, path: Option<String>) -> Self {
        self.initial_path = path;
        self
    }
    pub(super) fn initial(&mut self) -> Request {
        if let Some(path) = self.initial_path.take() {
            self.diff(path)
        } else {
            self.list(false)
        }
    }
    fn request(&self, action: ReviewAction) -> Request {
        Request::Review {
            session: self.session.clone(),
            expected_workspace: self.workspace.clone(),
            action,
        }
    }
    fn list(&mut self, append: bool) -> Request {
        self.pending = Some(Pending::List { append });
        self.message = None;
        self.request(ReviewAction::List {
            kind: self.kind,
            offset: if append { self.next.unwrap_or(0) } else { 0 },
        })
    }
    fn clear_diff(&mut self) {
        self.reviewed_path = None;
        self.patch.clear();
        self.checkpoint = None;
        self.receipt = None;
        self.cache = None;
        self.diff_offset = 0;
        self.confirm_undo = false;
    }
    fn diff(&mut self, path: String) -> Request {
        self.clear_diff();
        self.path.set(path.clone());
        self.pending = Some(Pending::Diff { path: path.clone() });
        self.message = None;
        self.request(ReviewAction::Diff {
            kind: self.kind,
            path,
        })
    }
    fn select_kind(&mut self, kind: Kind) -> Action {
        if self.pending.is_some() || self.kind == kind {
            return Action::None;
        }
        self.kind = kind;
        self.clear_diff();
        self.entries.clear();
        self.next = None;
        self.selected = 0;
        self.list_offset = 0;
        self.focus = Focus::List;
        Action::Request(self.list(false))
    }
    fn index(&mut self) -> Action {
        if self.pending.is_some() {
            return Action::None;
        }
        let (Some(path), Some(receipt)) = (self.reviewed_path.clone(), self.receipt.clone()) else {
            return Action::None;
        };
        let action = match self.kind {
            Kind::Staged => ReviewAction::Unstage { path, receipt },
            Kind::Unstaged | Kind::Untracked => ReviewAction::Stage { path, receipt },
            Kind::Checkpoint => return Action::None,
        };
        self.pending = Some(Pending::Index);
        self.message = None;
        Action::Request(self.request(action))
    }
    fn undo(&mut self) -> Action {
        if self.pending.is_some() {
            return Action::None;
        }
        let (Some(path), Some(checkpoint)) = (
            self.reviewed_path.clone(),
            self.checkpoint.as_ref().filter(|c| !c.undone),
        ) else {
            return Action::None;
        };
        let action = ReviewAction::Undo {
            path,
            run_id: checkpoint.run_id,
            call_id: checkpoint.call_id.clone(),
        };
        self.pending = Some(Pending::Undo);
        self.message = None;
        Action::Request(self.request(action))
    }
    fn selected(&mut self) -> Action {
        if self.pending.is_some() {
            return Action::None;
        }
        let Some(entry) = self.entries.get(self.selected) else {
            return Action::None;
        };
        let path = entry.path.clone();
        self.focus = Focus::Diff;
        Action::Request(self.diff(path))
    }
    fn activate(&mut self, key: char) -> Action {
        if key == 'c' {
            return Action::Close;
        }
        if self.pending.is_some() {
            return Action::None;
        }
        match key {
            's' => self.index(),
            'u' if self.checkpoint.as_ref().is_some_and(|c| !c.undone) => {
                self.confirm_undo = true;
                Action::None
            }
            'n' if self.next.is_some() => Action::Request(self.list(true)),
            'r' => {
                self.clear_diff();
                Action::Request(self.list(false))
            }
            'd' => self.selected(),
            'y' if self.reviewed_path.is_some() => Action::Copy(self.patch.clone()),
            _ => Action::None,
        }
    }
    fn rows(&mut self, width: u16) -> &[Line<'static>] {
        if !self
            .cache
            .as_ref()
            .is_some_and(|(cached_width, _)| *cached_width == width)
        {
            let mut renderer = code::Renderer::new("diff");
            let lines = self
                .patch
                .lines()
                .flat_map(|line| {
                    renderer.render(
                        line,
                        code::Options {
                            width: Some(width.max(2)),
                            ..Default::default()
                        },
                    )
                })
                .collect();
            self.cache = Some((width, lines));
        }
        &self.cache.as_ref().unwrap().1
    }
}

impl Panel for State {
    fn handle(&mut self, event: &Event) -> Action {
        if let Event::Key(key) = event {
            if key.kind == KeyEventKind::Release {
                return Action::None;
            }
            if self.confirm_undo {
                return match key.code {
                    KeyCode::Char('y' | 'Y') => {
                        self.confirm_undo = false;
                        self.undo()
                    }
                    KeyCode::Esc | KeyCode::Char('n' | 'N') => {
                        self.confirm_undo = false;
                        Action::None
                    }
                    _ => Action::None,
                };
            }
            if key.code == KeyCode::Esc {
                return Action::Close;
            }
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                return Action::Copy(self.patch.clone());
            }
            if key.code == KeyCode::Char('l') && key.modifiers.contains(KeyModifiers::CONTROL) {
                self.focus = Focus::Path;
                self.path.select_all();
                return Action::None;
            }
            if key.code == KeyCode::F(5) {
                return self.activate('r');
            }
            if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
                self.focus = if key.code == KeyCode::BackTab {
                    match self.focus {
                        Focus::List => Focus::Path,
                        Focus::Diff => Focus::List,
                        Focus::Path => Focus::Diff,
                    }
                } else {
                    match self.focus {
                        Focus::List => Focus::Diff,
                        Focus::Diff => Focus::Path,
                        Focus::Path => Focus::List,
                    }
                };
                return Action::None;
            }
            if self.focus != Focus::Path {
                if let KeyCode::Char(number @ '1'..='4') = key.code {
                    return self.select_kind(KINDS[number as usize - '1' as usize].0);
                }
                match key.code {
                    KeyCode::Char('s' | 'S') => return self.activate('s'),
                    KeyCode::Char('u' | 'U') => return self.activate('u'),
                    KeyCode::Char('n' | 'N') => return self.activate('n'),
                    KeyCode::Char('r' | 'R') => return self.activate('r'),
                    KeyCode::Char('y' | 'Y') => return self.activate('y'),
                    _ => {}
                }
            }
            match self.focus {
                Focus::Path => {
                    if key.code == KeyCode::Enter
                        && self.pending.is_none()
                        && !self.path.text.trim().is_empty()
                    {
                        let path = self.path.text.trim().into();
                        self.focus = Focus::Diff;
                        return Action::Request(self.diff(path));
                    }
                    self.path.edit(event, self.path_area.width.max(2) as usize);
                }
                Focus::List => match key.code {
                    KeyCode::Enter => return self.selected(),
                    KeyCode::Down => {
                        self.selected = self
                            .selected
                            .saturating_add(1)
                            .min(self.entries.len().saturating_sub(1))
                    }
                    KeyCode::Up => self.selected = self.selected.saturating_sub(1),
                    KeyCode::PageDown => {
                        self.selected = self
                            .selected
                            .saturating_add(self.list_area.height as usize)
                            .min(self.entries.len().saturating_sub(1))
                    }
                    KeyCode::PageUp => {
                        self.selected = self.selected.saturating_sub(self.list_area.height as usize)
                    }
                    KeyCode::Home => self.selected = 0,
                    KeyCode::End => self.selected = self.entries.len().saturating_sub(1),
                    _ => {}
                },
                Focus::Diff => match key.code {
                    KeyCode::Up => self.diff_offset = self.diff_offset.saturating_sub(1),
                    KeyCode::Down => self.diff_offset = self.diff_offset.saturating_add(1),
                    KeyCode::PageUp => {
                        self.diff_offset = self
                            .diff_offset
                            .saturating_sub(self.diff_area.height as usize)
                    }
                    KeyCode::PageDown => {
                        self.diff_offset = self
                            .diff_offset
                            .saturating_add(self.diff_area.height as usize)
                    }
                    KeyCode::Home => self.diff_offset = 0,
                    KeyCode::End => self.diff_offset = usize::MAX,
                    KeyCode::Left => self.focus = Focus::List,
                    _ => {}
                },
            }
        } else if self.confirm_undo {
            return Action::None;
        } else if let Event::Paste(_) = event {
            if self.focus == Focus::Path {
                self.path.edit(event, self.path_area.width.max(2) as usize);
            }
        } else if let Event::Mouse(mouse) = event {
            let point = (mouse.column, mouse.row).into();
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some((_, action)) = self
                        .action_hits
                        .iter()
                        .find(|(area, _)| area.contains(point))
                    {
                        return self.activate(*action);
                    }
                    if let Some((_, kind)) =
                        self.kind_hits.iter().find(|(area, _)| area.contains(point))
                    {
                        return self.select_kind(*kind);
                    }
                    if let Some((_, index)) =
                        self.row_hits.iter().find(|(area, _)| area.contains(point))
                    {
                        self.selected = *index;
                        return self.selected();
                    }
                    if self.path_area.contains(point) {
                        self.focus = Focus::Path;
                    } else if self.diff_area.contains(point) {
                        self.focus = Focus::Diff;
                    }
                }
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                    let delta = if mouse.kind == MouseEventKind::ScrollUp {
                        -3
                    } else {
                        3
                    };
                    if self.diff_area.contains(point) {
                        self.diff_offset = self.diff_offset.saturating_add_signed(delta);
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
            self.error(error);
            return Action::None;
        }
        let Some(pending) = self.pending.take() else {
            return Action::None;
        };
        match (pending, response) {
            (
                Pending::List { append },
                Response::Review(Snapshot::List {
                    entries,
                    next,
                    branch,
                }),
            ) => {
                if !append {
                    self.entries.clear();
                    self.selected = 0;
                    self.list_offset = 0;
                }
                self.entries
                    .extend(entries.into_iter().filter(|entry| entry.kind == self.kind));
                self.next = next;
                self.branch = branch;
            }
            (
                Pending::Diff { path: requested },
                Response::Review(Snapshot::Diff {
                    path,
                    patch,
                    note,
                    checkpoint,
                    index_receipt,
                }),
            ) if requested == path => {
                self.reviewed_path = Some(path);
                self.patch = patch;
                self.message = Some(note);
                self.checkpoint = checkpoint;
                self.receipt = index_receipt;
                self.cache = None;
                self.diff_offset = 0;
            }
            (Pending::Undo, Response::Review(Snapshot::Undone { detail, .. })) => {
                self.clear_diff();
                let request = self.list(false);
                self.message = Some(detail);
                return Action::Request(request);
            }
            (Pending::Index, Response::Review(Snapshot::IndexUpdated { staged, .. })) => {
                self.clear_diff();
                let request = self.list(false);
                self.message = Some(
                    if staged {
                        "已暂存；刷新列表完成后选择文件继续审阅"
                    } else {
                        "已取消暂存；刷新列表完成后选择文件继续审阅"
                    }
                    .into(),
                );
                return Action::Request(request);
            }
            _ => {
                self.receipt = None;
                self.checkpoint = None;
                self.message = Some("服务返回了不匹配的审阅响应，请刷新后重试".into());
            }
        }
        Action::None
    }
    fn error(&mut self, error: String) {
        let mutating = matches!(self.pending.take(), Some(Pending::Index | Pending::Undo));
        if mutating {
            self.receipt = None;
            self.checkpoint = None;
        }
        self.message = Some(error);
        self.confirm_undo = false;
    }
    fn invalidate_geometry(&mut self) {
        self.list_area = Rect::default();
        self.diff_area = Rect::default();
        self.path_area = Rect::default();
        self.row_hits.clear();
        self.kind_hits.clear();
        self.action_hits.clear();
    }
    fn draw(&mut self, frame: &mut Frame<'_>, area: Rect) {
        self.invalidate_geometry();
        if area.is_empty() {
            return;
        }
        components::title(
            frame,
            area,
            format!(
                "文件变更{}{}",
                self.branch.as_ref().map_or(String::new(), |branch| format!(
                    " · {}",
                    safe_output(branch)
                )),
                if self.pending.is_some() {
                    " · 处理中…"
                } else {
                    ""
                }
            ),
        );
        if area.height < 4 {
            return;
        }
        let tabs = Rect::new(area.x, area.y + 1, area.width, 1);
        let mut bar = components::ActionBar::new(tabs);
        for (index, (kind, label)) in KINDS.iter().enumerate() {
            if let Some(target) = bar.button(
                frame,
                &format!("{} {label}", index + 1),
                Control {
                    selected: self.kind == *kind,
                    disabled: self.pending.is_some(),
                    ..Default::default()
                },
            ) {
                self.kind_hits.push((target, *kind));
            }
        }
        self.path_area = Rect::new(area.x, area.y + 2, area.width, 1);
        frame.render_widget(
            Paragraph::new(format!(
                "{} {}",
                if self.focus == Focus::Path {
                    "路径 >"
                } else {
                    "路径 ^L"
                },
                safe_output(&self.path.text)
            ))
            .style(components::muted()),
            self.path_area,
        );
        let footer_height = 3.min(area.height.saturating_sub(3));
        let footer = Rect::new(
            area.x,
            area.bottom() - footer_height,
            area.width,
            footer_height,
        );
        let body = Rect::new(
            area.x,
            area.y + 3,
            area.width,
            area.height.saturating_sub(3 + footer_height),
        );
        let (list, diff) = if body.width >= 64 {
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
        } else if self.focus == Focus::List || self.reviewed_path.is_none() {
            (body, Rect::default())
        } else {
            (Rect::default(), body)
        };
        self.list_area = list;
        self.diff_area = diff;
        if !list.is_empty() {
            if self.selected < self.list_offset {
                self.list_offset = self.selected;
            }
            if self.selected >= self.list_offset + list.height as usize {
                self.list_offset = self
                    .selected
                    .saturating_sub(list.height.saturating_sub(1) as usize);
            }
            if self.entries.is_empty() {
                frame.render_widget(
                    Paragraph::new(if self.pending.is_some() {
                        "正在读取…"
                    } else {
                        "没有匹配的文件变更"
                    })
                    .style(components::muted()),
                    list,
                );
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
                frame.render_widget(
                    Paragraph::new(format!(
                        "{} {}{}",
                        safe_output(&entry.status),
                        safe_output(&entry.path),
                        if entry.undone { " · 已撤销" } else { "" }
                    ))
                    .style(if index == self.selected {
                        components::highlighted()
                    } else {
                        components::normal()
                    }),
                    target,
                );
                self.row_hits.push((target, index));
            }
        }
        if !diff.is_empty() {
            let offset = self.diff_offset;
            let max = self
                .rows(diff.width)
                .len()
                .saturating_sub(diff.height as usize);
            self.diff_offset = offset.min(max);
            let lines = self
                .cache
                .as_ref()
                .unwrap()
                .1
                .iter()
                .skip(self.diff_offset)
                .take(diff.height as usize)
                .cloned()
                .collect::<Vec<_>>();
            frame.render_widget(Paragraph::new(lines).style(components::normal()), diff);
            if self.reviewed_path.is_none() {
                frame.render_widget(
                    Paragraph::new(
                        "选择文件查看差异。\n1–4 切换变更来源；Enter 打开；Tab 切换焦点。",
                    )
                    .style(components::muted()),
                    diff,
                );
            }
        }
        let message = if self.confirm_undo {
            "撤销审阅过的这次 Orbit 写入？Y 撤销 · N / Esc 取消。其他修改会由服务校验。".into()
        } else {
            self.message.clone().unwrap_or_else(|| {
                format!(
                    "{} 个文件{} · ↑↓ 选择 · Enter 差异 · Tab 焦点 · 滚轮仅操作当前区域",
                    self.entries.len(),
                    if self.next.is_some() {
                        " · N 加载更多"
                    } else {
                        ""
                    }
                )
            })
        };
        frame.render_widget(
            Paragraph::new(safe_output(&message)).style(if self.confirm_undo {
                components::accent()
            } else {
                components::muted()
            }),
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
        let index_label = if self.kind == Kind::Staged {
            "取消暂存 S"
        } else {
            "暂存 S"
        };
        for (label, key) in [
            (index_label, 's'),
            ("撤销写入 U", 'u'),
            ("复制差异 Y", 'y'),
            ("更多 N", 'n'),
            ("刷新 F5", 'r'),
            ("关闭 Esc", 'c'),
        ] {
            let disabled = (self.pending.is_some() && key != 'c')
                || (key == 's' && (self.kind == Kind::Checkpoint || self.receipt.is_none()))
                || (key == 'u' && !self.checkpoint.as_ref().is_some_and(|c| !c.undone))
                || (key == 'y' && self.reviewed_path.is_none())
                || (key == 'n' && self.next.is_none());
            if let Some(target) = bar.button(
                frame,
                label,
                Control {
                    disabled,
                    ..Default::default()
                },
            ) {
                self.action_hits.push((target, key));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;
    use ratatui::{Terminal, backend::TestBackend};

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }
    fn reviewed(kind: Kind) -> State {
        let mut state = State::new("session".into(), "/workspace".into())
            .with_kind(kind)
            .at(Some("src/main.rs".into()));
        state.initial();
        state.receive(Response::Review(Snapshot::Diff {
            path: "src/main.rs".into(),
            patch: "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1 @@\n-old\n+new\n".into(),
            note: "reviewed state".into(),
            checkpoint: (kind == Kind::Checkpoint).then(|| Checkpoint {
                run_id: 7,
                call_id: "write-42".into(),
                undone: false,
            }),
            index_receipt: (kind != Kind::Checkpoint).then(|| "opaque-reviewed-receipt".into()),
        }));
        state
    }
    #[test]
    fn index_operations_use_the_reviewed_receipt_and_refresh_after_acknowledgement() {
        for kind in [Kind::Unstaged, Kind::Staged, Kind::Untracked] {
            let mut state = reviewed(kind);
            let Action::Request(Request::Review {
                expected_workspace,
                action,
                ..
            }) = state.handle(&key(KeyCode::Char('s')))
            else {
                panic!("index request expected");
            };
            assert_eq!(expected_workspace, "/workspace");
            match action {
                ReviewAction::Stage { path, receipt } => {
                    assert_ne!(kind, Kind::Staged);
                    assert_eq!(path, "src/main.rs");
                    assert_eq!(receipt, "opaque-reviewed-receipt");
                }
                ReviewAction::Unstage { receipt, .. } => {
                    assert_eq!(kind, Kind::Staged);
                    assert_eq!(receipt, "opaque-reviewed-receipt");
                }
                _ => panic!("index action expected"),
            }
            assert!(matches!(
                state.receive(Response::Review(Snapshot::IndexUpdated {
                    path: "src/main.rs".into(),
                    staged: kind != Kind::Staged
                })),
                Action::Request(Request::Review {
                    action: ReviewAction::List { .. },
                    ..
                })
            ));
            assert!(state.receipt.is_none());
        }
    }
    #[test]
    fn undo_requires_confirmation_and_the_exact_checkpoint_identity() {
        let mut state = reviewed(Kind::Checkpoint);
        assert!(matches!(
            state.handle(&key(KeyCode::Char('u'))),
            Action::None
        ));
        assert!(state.confirm_undo);
        state.handle(&key(KeyCode::Char('n')));
        assert!(!state.confirm_undo);
        state.handle(&key(KeyCode::Char('u')));
        let Action::Request(Request::Review {
            action:
                ReviewAction::Undo {
                    path,
                    run_id,
                    call_id,
                },
            ..
        }) = state.handle(&key(KeyCode::Char('y')))
        else {
            panic!("undo expected");
        };
        assert_eq!(
            (path.as_str(), run_id, call_id.as_str()),
            ("src/main.rs", 7, "write-42")
        );
        state.error("checkpoint changed".into());
        assert!(state.checkpoint.is_none());
        assert!(matches!(
            state.handle(&key(KeyCode::Char('u'))),
            Action::None
        ));
        assert!(!state.confirm_undo);
    }
    #[test]
    fn paging_and_kind_switch_never_reuse_a_diff_receipt() {
        let mut state = reviewed(Kind::Unstaged);
        state.next = Some(500);
        assert!(matches!(
            state.handle(&key(KeyCode::Char('n'))),
            Action::Request(Request::Review {
                action: ReviewAction::List {
                    kind: Kind::Unstaged,
                    offset: 500
                },
                ..
            })
        ));
        state.receive(Response::Review(Snapshot::List {
            entries: vec![],
            next: None,
            branch: Some("main".into()),
        }));
        assert!(matches!(
            state.handle(&key(KeyCode::Char('2'))),
            Action::Request(Request::Review {
                action: ReviewAction::List {
                    kind: Kind::Staged,
                    offset: 0
                },
                ..
            })
        ));
        assert!(state.receipt.is_none());
        assert!(state.patch.is_empty());
    }
    #[test]
    fn small_layouts_preserve_diff_and_reset_hidden_hit_targets() {
        let mut state = reviewed(Kind::Checkpoint);
        let patch = state.patch.clone();
        for (width, height) in [(100, 30), (30, 10), (10, 5), (1, 1)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| state.draw(frame, frame.area()))
                .unwrap();
            assert_eq!(state.patch, patch);
            for (area, _) in &state.row_hits {
                assert!(area.right() <= width && area.bottom() <= height);
            }
            for (area, _) in &state.action_hits {
                assert!(area.right() <= width && area.bottom() <= height);
            }
        }
    }
}
