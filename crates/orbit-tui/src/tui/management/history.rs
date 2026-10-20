//! Keyboard-first Git history using the same read-only service contract as the desktop.
use super::{Action, Panel};
use crate::tui::{input::InputState, render::safe_output};
use crate::workspace::protocol::{Request, Response};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use orbit_protocol::review::{Action as ReviewAction, Commit, Snapshot};
use ratatui::{Frame, layout::Rect, text::Line, widgets::Paragraph};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Page {
    Commits,
    Files,
    Content,
    Diff,
}

#[derive(Debug)]
struct Detail {
    commit: Commit,
    message: String,
    files: Vec<String>,
}

#[derive(Clone, Copy, Debug)]
enum Pending {
    History { append: bool },
    Commit,
    File,
}

#[derive(Debug)]
pub(super) struct State {
    session: String,
    workspace: String,
    revision: InputState,
    editing_revision: bool,
    pinned: String,
    commits: Vec<Commit>,
    next: Option<usize>,
    detail: Option<Detail>,
    content: String,
    patch: String,
    page: Page,
    commit_index: usize,
    file_index: usize,
    scroll: usize,
    pending: Option<Pending>,
    error: Option<String>,
}

impl State {
    pub(super) fn new(session: String, workspace: String, revision: &str) -> Self {
        let mut input = InputState::default();
        input.set(revision.into());
        Self {
            session,
            workspace,
            revision: input,
            editing_revision: false,
            pinned: revision.into(),
            commits: vec![],
            next: None,
            detail: None,
            content: String::new(),
            patch: String::new(),
            page: Page::Commits,
            commit_index: 0,
            file_index: 0,
            scroll: 0,
            pending: None,
            error: None,
        }
    }

    fn request(&mut self, action: ReviewAction, pending: Pending) -> Request {
        self.pending = Some(pending);
        self.error = None;
        Request::Review {
            session: self.session.clone(),
            expected_workspace: self.workspace.clone(),
            action,
        }
    }

    pub(super) fn initial(&mut self) -> Request {
        self.request(
            ReviewAction::History {
                revision: self.revision.text.clone(),
                offset: 0,
            },
            Pending::History { append: false },
        )
    }

    fn select(&mut self) -> Action {
        match self.page {
            Page::Commits => {
                let Some(commit) = self.commits.get(self.commit_index) else {
                    return Action::None;
                };
                let revision = commit.id.clone();
                Action::Request(self.request(ReviewAction::Commit { revision }, Pending::Commit))
            }
            Page::Files => {
                let Some(detail) = &self.detail else {
                    return Action::None;
                };
                let Some(path) = detail.files.get(self.file_index) else {
                    return Action::None;
                };
                let action = ReviewAction::CommitFile {
                    revision: detail.commit.id.clone(),
                    path: path.clone(),
                };
                Action::Request(self.request(action, Pending::File))
            }
            _ => Action::None,
        }
    }
}

impl Panel for State {
    fn handle(&mut self, event: &Event) -> Action {
        let Event::Key(key) = event else {
            return Action::None;
        };
        if key.kind == KeyEventKind::Release {
            return Action::None;
        }
        if key.code == KeyCode::Esc {
            return Action::Close;
        }
        if self.pending.is_some() {
            return Action::None;
        }
        if key.code == KeyCode::Char('l')
            && key
                .modifiers
                .contains(KeyModifiers::CONTROL)
        {
            self.editing_revision = true;
            self.revision.select_all();
            return Action::None;
        }
        if self.editing_revision {
            if key.code == KeyCode::Enter {
                self.editing_revision = false;
                self.commit_index = 0;
                return Action::Request(self.initial());
            }
            self.revision.edit(event, 60);
            return Action::None;
        }
        match key.code {
            KeyCode::Enter => self.select(),
            KeyCode::Char('b') | KeyCode::Backspace => {
                self.page = match self.page {
                    Page::Content | Page::Diff => Page::Files,
                    _ => Page::Commits,
                };
                self.scroll = 0;
                Action::None
            }
            KeyCode::Char('d') if matches!(self.page, Page::Content | Page::Diff) => {
                self.page = if self.page == Page::Content {
                    Page::Diff
                } else {
                    Page::Content
                };
                self.scroll = 0;
                Action::None
            }
            KeyCode::Char('n') if self.page == Page::Commits && self.next.is_some() => {
                Action::Request(self.request(
                    ReviewAction::History {
                        revision: self.pinned.clone(),
                        offset: self.next.unwrap(),
                    },
                    Pending::History { append: true },
                ))
            }
            KeyCode::F(5) => Action::Request(self.initial()),
            KeyCode::Up | KeyCode::Down | KeyCode::PageUp | KeyCode::PageDown | KeyCode::Home => {
                let index = match self.page {
                    Page::Commits => &mut self.commit_index,
                    Page::Files => &mut self.file_index,
                    _ => &mut self.scroll,
                };
                let step = if matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) {
                    10
                } else {
                    1
                };
                *index = match key.code {
                    KeyCode::Home => 0,
                    KeyCode::Up | KeyCode::PageUp => index.saturating_sub(step),
                    _ => index.saturating_add(step),
                };
                if self.page == Page::Commits {
                    self.commit_index = self
                        .commit_index
                        .min(self.commits.len().saturating_sub(1));
                }
                if self.page == Page::Files {
                    self.file_index = self.file_index.min(
                        self.detail
                            .as_ref()
                            .map_or(0, |detail| detail.files.len().saturating_sub(1)),
                    );
                }
                Action::None
            }
            _ => Action::None,
        }
    }

    fn receive(&mut self, response: Response) -> Action {
        let Some(pending) = self.pending.take() else {
            return Action::None;
        };
        match (pending, response) {
            (
                Pending::History { append },
                Response::Review(Snapshot::History {
                    revision,
                    commits,
                    next,
                    ..
                }),
            ) => {
                self.pinned = revision;
                if append {
                    self.commits.extend(commits);
                } else {
                    self.commits = commits;
                    self.commit_index = 0;
                }
                self.next = next;
                self.page = Page::Commits;
            }
            (
                Pending::Commit,
                Response::Review(Snapshot::Commit {
                    commit,
                    message,
                    files,
                }),
            ) => {
                self.detail = Some(Detail {
                    commit,
                    message,
                    files,
                });
                self.file_index = 0;
                self.page = Page::Files;
            }
            (Pending::File, Response::Review(Snapshot::CommitFile { content, patch, .. })) => {
                self.content =
                    content.unwrap_or_else(|| "此提交中不存在该文件，或文件不是文本".into());
                self.patch = patch;
                self.page = Page::Content;
                self.scroll = 0;
            }
            (_, Response::Error(error)) => self.error = Some(error),
            _ => self.error = Some("服务未返回提交历史，请更新服务".into()),
        }
        Action::None
    }

    fn error(&mut self, error: String) {
        self.pending = None;
        self.error = Some(error);
    }

    fn draw(&mut self, frame: &mut Frame<'_>, area: Rect) {
        let mut lines = vec![
            Line::from(format!("提交历史 · {}", safe_output(&self.revision.text))),
            Line::from(
                "↑↓ 选择/滚动  Enter 打开  b 文件列表/历史  d 文件/差异  Ctrl+L 分支  n 更多  Esc 关闭",
            ),
        ];
        if self.editing_revision {
            lines.push(Line::from(format!(
                "分支或提交：{}",
                safe_output(&self.revision.text)
            )));
        }
        if self.pending.is_some() {
            lines.push(Line::from("正在读取…"));
        }
        if let Some(error) = &self.error {
            lines.push(Line::from(safe_output(error)));
        }
        let header = lines.len();
        match self.page {
            Page::Commits => {
                let available = usize::from(area.height)
                    .saturating_sub(header)
                    .max(1);
                let start = self
                    .commit_index
                    .saturating_sub(available - 1);
                lines.extend(
                    self.commits
                        .iter()
                        .enumerate()
                        .skip(start)
                        .take(available)
                        .map(|(index, commit)| {
                            Line::from(format!(
                                "{} {}  {} · {}",
                                if index == self.commit_index {
                                    "›"
                                } else {
                                    " "
                                },
                                commit.short_id,
                                safe_output(&commit.subject),
                                safe_output(&commit.author)
                            ))
                        }),
                );
            }
            Page::Files => {
                if let Some(detail) = &self.detail {
                    lines.push(Line::from(format!(
                        "{} · {}",
                        detail.commit.short_id,
                        safe_output(&detail.commit.subject)
                    )));
                    lines.extend(
                        detail
                            .message
                            .lines()
                            .take(4)
                            .map(|line| Line::from(safe_output(line))),
                    );
                    let available = usize::from(area.height)
                        .saturating_sub(lines.len())
                        .max(1);
                    let start = self
                        .file_index
                        .saturating_sub(available - 1);
                    lines.extend(
                        detail
                            .files
                            .iter()
                            .enumerate()
                            .skip(start)
                            .take(available)
                            .map(|(index, path)| {
                                Line::from(format!(
                                    "{} {}",
                                    if index == self.file_index { "›" } else { " " },
                                    safe_output(path)
                                ))
                            }),
                    );
                }
            }
            Page::Content | Page::Diff => {
                let text = if self.page == Page::Diff {
                    &self.patch
                } else {
                    &self.content
                };
                let available = usize::from(area.height)
                    .saturating_sub(header)
                    .max(1);
                self.scroll = self.scroll.min(
                    text.lines()
                        .count()
                        .saturating_sub(available),
                );
                lines.extend(
                    text.lines()
                        .skip(self.scroll)
                        .take(available)
                        .map(|line| Line::from(safe_output(line))),
                );
            }
        }
        frame.render_widget(Paragraph::new(lines), area);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    #[test]
    fn history_pagination_uses_pinned_identity_and_escape_always_closes() {
        let mut panel = State::new("session".into(), "/generated".into(), "main");
        panel.initial();
        panel.receive(Response::Review(Snapshot::History {
            revision: "a".repeat(40),
            commits: vec![],
            branches: vec!["main".into()],
            next: Some(50),
        }));
        let action = panel.handle(&Event::Key(KeyEvent::new(
            KeyCode::Char('n'),
            KeyModifiers::NONE,
        )));
        assert!(
            matches!(action, Action::Request(Request::Review { action: ReviewAction::History { revision, offset: 50 }, .. }) if revision == "a".repeat(40))
        );
        assert!(matches!(
            panel.handle(&Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE))),
            Action::Close
        ));
    }
}
