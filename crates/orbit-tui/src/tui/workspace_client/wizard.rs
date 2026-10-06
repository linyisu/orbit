use super::Spec;
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
};

use crate::{
    tui::{connect_custom::form, input::InputState, overlay, render, theme::THEME},
    workspace::directories::Listing,
};

use super::{configuration::Config, controls};

pub(super) const CANDIDATE_PANEL: usize = 9;
const CLOSE: usize = 96;

#[derive(Debug)]
pub(crate) struct Wizard {
    pub form: Option<Config>,
    pub listing: Option<Listing>,
    pub logs: Vec<String>,
    pub status: String,
    pub failed: bool,
    pub busy: bool,
    scroll: Option<usize>,
    last_offset: usize,
    selected: usize,
    directory_offset: usize,
    hidden: bool,
    path: InputState,
    focus: usize,
    hover: Option<usize>,
    areas: Vec<(Rect, usize)>,
}

pub(super) enum Action {
    None,
    Start(Box<Spec>),
    Browse(String),
    Open(String),
    Cancel,
    Back,
    Retry,
}

impl Wizard {
    pub fn new(spec: Option<Spec>) -> Self {
        Self {
            form: spec.map(Config::new),
            listing: None,
            logs: Vec::new(),
            status: "准备连接".into(),
            failed: false,
            busy: false,
            scroll: None,
            last_offset: 0,
            selected: 0,
            directory_offset: 0,
            hidden: false,
            path: InputState::default(),
            focus: 8,
            hover: None,
            areas: Vec::new(),
        }
    }

    pub fn set_listing(&mut self, listing: Listing) {
        self.path.set(listing.path.clone());
        self.listing = Some(listing);
        self.selected = 0;
        self.directory_offset = 0;
        self.focus = 8;
        self.scroll = None;
        self.busy = false;
    }

    fn entries(&self) -> Vec<String> {
        ["打开当前目录".into(), "..".into()]
            .into_iter()
            .chain(
                self.listing
                    .iter()
                    .flat_map(|listing| listing.directories.iter())
                    .filter(|name| self.hidden || !name.starts_with('.'))
                    .cloned(),
            )
            .collect()
    }

    fn open(&self) -> Action {
        Action::Open(self.path.text.trim().to_owned())
    }

    fn browse(&self) -> Action {
        if self.selected == 0 {
            return self.open();
        }
        let Some(listing) = &self.listing else {
            return Action::None;
        };
        let entries = self.entries();
        let Some(name) = entries.get(self.selected) else {
            return Action::None;
        };
        Action::Browse(if name == ".." {
            std::path::Path::new(&listing.path)
                .parent()
                .unwrap_or(std::path::Path::new("/"))
                .to_string_lossy()
                .into_owned()
        } else {
            format!("{}/{name}", listing.path.trim_end_matches('/'))
        })
    }

    pub(super) fn handle(&mut self, event: &Event) -> Action {
        if let Event::Mouse(mouse) = event {
            self.hover = self
                .areas
                .iter()
                .find(|(area, _)| area.contains((mouse.column, mouse.row).into()))
                .map(|(_, id)| *id);
            if mouse.kind == MouseEventKind::Moved
                && let Some(choice) = self.hover.filter(|id| *id >= 100)
            {
                if let Some(config) = &mut self.form {
                    config.suggestion = choice - 100;
                } else if self.listing.is_some() && !self.busy && !self.failed {
                    self.selected = choice - 100;
                }
            }
            if let Some(config) = &mut self.form
                && config.has_candidates()
            {
                let key = match mouse.kind {
                    MouseEventKind::ScrollUp => Some(KeyCode::Up),
                    MouseEventKind::ScrollDown => Some(KeyCode::Down),
                    _ => None,
                };
                if let Some(key) = key {
                    return config
                        .handle(&crossterm::event::KeyEvent::new(key, KeyModifiers::NONE));
                }
            }
            if matches!(
                mouse.kind,
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            ) && self.form.is_none()
            {
                if self.listing.is_some() {
                    self.selected = if mouse.kind == MouseEventKind::ScrollUp {
                        self.selected.saturating_sub(3)
                    } else {
                        (self.selected + 3).min(self.entries().len().saturating_sub(1))
                    };
                } else if mouse.kind == MouseEventKind::ScrollUp {
                    self.scroll = Some(self.last_offset.saturating_sub(3));
                } else {
                    self.scroll = Some(self.last_offset.saturating_add(3));
                }
                return Action::None;
            }
            if mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && let Some((_, choice)) = self
                    .areas
                    .iter()
                    .find(|(area, _)| area.contains((mouse.column, mouse.row).into()))
            {
                let choice = *choice;
                if matches!(choice, 99 | CLOSE) {
                    return Action::Cancel;
                }
                if choice == 98 {
                    return Action::Back;
                }
                if choice == 97 {
                    return Action::Retry;
                }
                if let Some(config) = &mut self.form {
                    if choice == 6 {
                        config.open_picker();
                        return Action::None;
                    }
                    if choice < 5 {
                        config.picker = false;
                        config.focus = choice;
                        return Action::None;
                    }
                    if choice == 5 {
                        return config.start();
                    }
                    if choice >= 100 {
                        config.suggestion = choice - 100;
                        config.choose();
                    }
                } else if self.listing.is_some() && !self.busy {
                    if choice == 5 {
                        return self.open();
                    }
                    if choice == 6 {
                        self.hidden = !self.hidden;
                        self.selected = 0;
                    }
                    if choice == 7 {
                        self.focus = 7;
                    }
                    if choice >= 100 {
                        self.focus = 8;
                        self.selected = choice - 100;
                        return self.browse();
                    }
                }
            }
        }
        let Event::Key(key) = event else {
            if let Event::Paste(text) = event {
                if let Some(config) = &mut self.form {
                    config.paste(text);
                } else if self.focus == 7 {
                    form::paste(&mut self.path, text);
                }
            }
            return Action::None;
        };
        if key.code == KeyCode::Enter && !key.modifiers.is_empty() {
            return Action::None;
        }
        if key.kind == KeyEventKind::Release {
            return Action::None;
        }
        self.hover = None;
        if key.code == KeyCode::Esc && self.form.as_ref().is_some_and(|config| config.picker) {
            return self.form.as_mut().unwrap().handle(key);
        }
        if key.code == KeyCode::Esc
            || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            return Action::Cancel;
        }
        if let Some(config) = &mut self.form {
            return config.handle(key);
        }
        if key.code == KeyCode::Tab || key.code == KeyCode::BackTab {
            let order: &[usize] = if self.failed {
                &[97, 98, 99]
            } else if self.listing.is_some() && !self.busy {
                &[8, 5, 6, 99, 7]
            } else {
                &[99]
            };
            let index = order
                .iter()
                .position(|focus| *focus == self.focus)
                .unwrap_or(0);
            let back = key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT);
            self.focus = order[if back {
                (index + order.len() - 1) % order.len()
            } else {
                (index + 1) % order.len()
            }];
            return Action::None;
        }
        if self.failed {
            return match key.code {
                KeyCode::Char('r') => Action::Retry,
                KeyCode::Char('b') => Action::Back,
                KeyCode::Enter => match self.focus {
                    98 => Action::Back,
                    99 => Action::Cancel,
                    _ => Action::Retry,
                },
                _ => Action::None,
            };
        }
        if self.listing.is_some() && !self.busy {
            match key.code {
                KeyCode::Enter => {
                    return match self.focus {
                        5 => self.open(),
                        7 => Action::Browse(self.path.text.clone()),
                        99 => Action::Cancel,
                        6 => {
                            self.hidden = !self.hidden;
                            self.selected = 0;
                            Action::None
                        }
                        _ => self.browse(),
                    };
                }
                KeyCode::Up if self.focus == 8 => self.selected = self.selected.saturating_sub(1),
                KeyCode::Down if self.focus == 8 => {
                    self.selected = (self.selected + 1).min(self.entries().len().saturating_sub(1))
                }
                _ if self.focus == 7 => form::edit(&mut self.path, key.code, key.modifiers),
                _ => {}
            }
        } else {
            match key.code {
                KeyCode::Enter if self.focus == 99 => return Action::Cancel,
                KeyCode::PageUp => self.scroll = Some(self.last_offset.saturating_sub(8)),
                KeyCode::PageDown => self.scroll = Some(self.last_offset.saturating_add(8)),
                KeyCode::End => self.scroll = None,
                _ => {}
            }
        }
        Action::None
    }

    pub fn draw(&mut self, frame: &mut Frame<'_>) {
        let bounds = frame.area();
        let width = bounds.width.saturating_sub(4).min(88);
        let desired = if self.form.is_some() {
            if width >= 68 { 23 } else { 29 }
        } else if self.listing.is_some() && !self.failed && !self.busy {
            (self.entries().len() as u16 + 14).clamp(18, 24)
        } else {
            24
        };
        let height = bounds.height.saturating_sub(4).min(desired);
        let area = Rect::new(
            bounds.x + (bounds.width - width) / 2,
            bounds.y + (bounds.height - height) / 2,
            width,
            height,
        );
        overlay::clear(frame, area);
        self.areas.clear();
        frame.render_widget(
            controls::panel().title(Span::styled(
                " 远程工作区 ",
                Style::default()
                    .fg(THEME.dialog_accent)
                    .add_modifier(Modifier::BOLD),
            )),
            area,
        );
        let inner = area.inner(ratatui::layout::Margin::new(2, 1));
        if inner.height < 10 || inner.width < 20 {
            return;
        }
        let stage = if self.form.is_some() {
            0
        } else if self.listing.is_some() {
            2
        } else {
            1
        };
        let line = Line::from(
            ["1 连接配置", "2 连接日志", "3 选择目录"]
                .iter()
                .enumerate()
                .map(|(index, text)| {
                    Span::styled(
                        format!("{text}   "),
                        if index == stage {
                            Style::default()
                                .fg(THEME.dialog_accent)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(THEME.muted)
                        },
                    )
                })
                .collect::<Vec<_>>(),
        );
        frame.render_widget(
            Paragraph::new(line),
            Rect::new(inner.x, inner.y, inner.width, 1),
        );
        let close = Rect::new(area.right().saturating_sub(4), area.y, 3, 1);
        controls::button(frame, close, "×", self.hover == Some(CLOSE));
        self.areas.push((close, CLOSE));
        let body = Rect::new(
            inner.x,
            inner.y + 2,
            inner.width,
            inner.height.saturating_sub(7),
        );
        if let Some(config) = &mut self.form {
            config.draw(frame, body, &mut self.areas, self.hover);
        } else if self.listing.is_some() && !self.failed && !self.busy {
            let path = Rect::new(body.x, body.y, body.width, 3);
            controls::field(
                frame,
                path,
                "当前目录",
                &self.path,
                self.focus == 7,
                self.hover == Some(7),
                "输入路径后按 Enter 前往",
            );
            self.areas.push((path, 7));
            let list = Rect::new(
                body.x,
                body.y + 3,
                body.width,
                body.height.saturating_sub(3),
            );

            let entries = self.entries();
            let count = list.height as usize;
            self.directory_offset = self
                .directory_offset
                .min(self.selected)
                .max(self.selected.saturating_sub(count.saturating_sub(1)))
                .min(entries.len().saturating_sub(count));
            let start = self.directory_offset;
            for (row, (index, name)) in entries
                .iter()
                .enumerate()
                .skip(start)
                .take(count)
                .enumerate()
            {
                let rect = Rect::new(list.x, list.y + row as u16, list.width, 1);
                let name = if index == 0 {
                    "打开当前目录  ↵".into()
                } else if name == ".." {
                    "..  上一级".into()
                } else {
                    format!("{}/", render::safe_output(name))
                };
                frame.render_widget(
                    Paragraph::new(format!(
                        " {} {name}",
                        if index == self.selected { "›" } else { " " }
                    ))
                    .style(if index == self.selected {
                        Style::default().fg(THEME.on_accent).bg(THEME.dialog_accent)
                    } else {
                        Style::default()
                    }),
                    rect,
                );
                self.areas.push((rect, 100 + index));
            }
        } else {
            let mut lines = self
                .logs
                .iter()
                .map(|line| Line::raw(render::safe_output(line)))
                .collect::<Vec<_>>();
            let style = Style::default().fg(if self.failed {
                THEME.error
            } else {
                THEME.dialog_accent
            });
            lines.extend(
                self.status
                    .lines()
                    .map(|line| Line::styled(render::safe_output(line), style)),
            );
            let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
            let bottom = paragraph
                .line_count(body.width)
                .saturating_sub(body.height as usize);
            let offset = self.scroll.unwrap_or(bottom).min(bottom);
            self.last_offset = offset;
            if offset == bottom {
                self.scroll = None;
            }
            frame.render_widget(
                paragraph.scroll((offset.min(u16::MAX as usize) as u16, 0)),
                body,
            );
        }
        let footer = Rect::new(inner.x, inner.bottom() - 3, inner.width, 1);
        let buttons: Vec<(&str, usize)> = if self.form.is_some() {
            vec![("连接", 5), ("取消", 99)]
        } else if self.failed {
            if !matches!(self.focus, 97..=99) {
                self.focus = 97;
            }
            vec![("重试", 97), ("修改配置", 98), ("关闭", 99)]
        } else if self.listing.is_some() && !self.busy {
            vec![
                ("打开此目录", 5),
                (
                    if self.hidden {
                        "隐藏项 开"
                    } else {
                        "隐藏项 关"
                    },
                    6,
                ),
                ("取消", 99),
            ]
        } else {
            vec![("取消连接", 99)]
        };
        let total: u16 = buttons
            .iter()
            .map(|(text, _)| unicode_width::UnicodeWidthStr::width(*text) as u16 + 2)
            .sum::<u16>()
            + (buttons.len().saturating_sub(1) * 2) as u16;
        let compact = total > footer.width;
        let compact_width = footer
            .width
            .saturating_sub((buttons.len().saturating_sub(1) * 2) as u16)
            / buttons.len() as u16;
        let mut x = footer.x;
        for (text, id) in buttons {
            let width = if compact {
                compact_width
            } else {
                unicode_width::UnicodeWidthStr::width(text) as u16 + 2
            };
            let rect = Rect::new(x, footer.y, width.min(footer.right().saturating_sub(x)), 1);
            let focused = self
                .form
                .as_ref()
                .map_or(self.focus == id, |form| form.focus == id);
            controls::button(frame, rect, text, focused || self.hover == Some(id));
            self.areas.push((rect, id));
            x = x.saturating_add(width + 2);
        }
        frame.render_widget(
            Paragraph::new(if self.listing.is_some() && !self.busy {
                "↑↓ 选择 · Enter 打开 · Tab 切换 · Esc 返回"
            } else {
                "Tab 切换 · Enter 确认 · Esc 返回"
            })
            .style(Style::default().fg(THEME.muted)),
            Rect::new(inner.x, inner.bottom() - 1, inner.width, 1),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    #[test]
    fn configuration_reports_errors_and_opens_the_selected_directory() {
        let mut wizard = Wizard::new(Some(Spec::parse("/ssh").unwrap()));
        let config = wizard.form.as_mut().unwrap();
        assert!(matches!(config.start(), Action::None));
        assert!(config.error.as_deref().unwrap().contains("主机"));
        config.fields[0].set("user@example".into());
        config.fields[2].set("70000".into());
        assert!(matches!(config.start(), Action::None));
        assert!(config.error.as_deref().unwrap().contains("端口"));
        config.fields[2].set("2222".into());
        assert!(matches!(config.start(), Action::Start(spec) if spec.fields()[2] == "2222"));
        wizard.form = None;
        wizard.set_listing(Listing {
            path: "/home/me".into(),
            directories: vec![".hidden".into(), "a b".into()],
        });
        let enter = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        wizard.selected = 2;
        assert!(matches!(wizard.handle(&enter), Action::Browse(path) if path == "/home/me/a b"));
        assert!(matches!(
            wizard.handle(&Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::CONTROL
            ))),
            Action::None
        ));
        wizard.handle(&Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)));
        assert!(matches!(wizard.handle(&enter), Action::Open(path) if path == "/home/me"));
    }

    #[test]
    fn directory_buttons_are_keyboard_accessible_without_modified_enter() {
        let mut wizard = Wizard::new(None);
        wizard.set_listing(Listing {
            path: "/work".into(),
            directories: vec!["child".into()],
        });
        let key = |code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE));
        wizard.handle(&key(KeyCode::Tab));
        assert!(
            matches!(wizard.handle(&key(KeyCode::Enter)), Action::Open(path) if path == "/work")
        );
        wizard.handle(&key(KeyCode::Tab));
        wizard.handle(&key(KeyCode::Enter));
        assert!(wizard.hidden);
        wizard.handle(&key(KeyCode::Tab));
        assert!(matches!(
            wizard.handle(&key(KeyCode::Enter)),
            Action::Cancel
        ));
        wizard.handle(&key(KeyCode::Tab));
        wizard.path.set("/another".into());
        assert!(
            matches!(wizard.handle(&key(KeyCode::Enter)), Action::Browse(path) if path == "/another")
        );
    }

    fn mouse(kind: MouseEventKind, area: Rect) -> Event {
        Event::Mouse(crossterm::event::MouseEvent {
            kind,
            column: area.x,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        })
    }

    #[test]
    fn mouse_hover_and_click_share_candidate_hits_and_do_not_fall_through() {
        let mut wizard = Wizard::new(Some(
            Spec::parse("/ssh server.example --ssh-user alice").unwrap(),
        ));
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 34)).unwrap();
        wizard.form.as_mut().unwrap().suggestion = usize::MAX;
        terminal.draw(|frame| wizard.draw(frame)).unwrap();
        let (candidate, candidate_id) = *wizard
            .areas
            .iter()
            .filter(|(_, id)| *id >= 100)
            .max_by_key(|(_, id)| *id)
            .unwrap();
        let blank = Rect::new(candidate.right(), candidate.y, 1, 1);
        wizard.handle(&mouse(MouseEventKind::Down(MouseButton::Left), blank));
        assert!(wizard.form.as_ref().unwrap().picker);
        assert_eq!(
            wizard.form.as_ref().unwrap().fields[0].text,
            "server.example"
        );
        wizard.handle(&mouse(MouseEventKind::Moved, candidate));
        assert_eq!(wizard.hover, Some(candidate_id));
        wizard.handle(&mouse(MouseEventKind::Down(MouseButton::Left), candidate));
        let config = wizard.form.as_ref().unwrap();
        assert!(!config.picker);
        assert_eq!(config.focus, 0);
        assert!(config.fields[..4].iter().all(|input| input.text.is_empty()));

        terminal.draw(|frame| wizard.draw(frame)).unwrap();
        let field = wizard.areas.iter().find(|(_, id)| *id == 1).unwrap().0;
        wizard.handle(&mouse(MouseEventKind::Moved, field));
        terminal.draw(|frame| wizard.draw(frame)).unwrap();
        assert_eq!(
            terminal.backend().buffer()[(field.x, field.y)].fg,
            THEME.dialog_accent
        );
        wizard.handle(&mouse(MouseEventKind::Down(MouseButton::Left), field));
        assert_eq!(wizard.form.as_ref().unwrap().focus, 1);
        let button = wizard.areas.iter().find(|(_, id)| *id == 99).unwrap().0;
        wizard.handle(&mouse(MouseEventKind::Moved, button));
        terminal.draw(|frame| wizard.draw(frame)).unwrap();
        assert_eq!(
            terminal.backend().buffer()[(button.x, button.y)].bg,
            THEME.dialog_accent
        );
        assert!(matches!(
            wizard.handle(&mouse(MouseEventKind::Down(MouseButton::Left), button)),
            Action::Cancel
        ));
    }

    #[test]
    fn directory_hover_preserves_the_viewport_and_clicks_the_highlighted_row() {
        let mut wizard = Wizard::new(None);
        wizard.set_listing(Listing {
            path: "/work".into(),
            directories: (0..40).map(|i| format!("child-{i}")).collect(),
        });
        wizard.selected = 30;
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 34)).unwrap();
        terminal.draw(|frame| wizard.draw(frame)).unwrap();
        let offset = wizard.directory_offset;
        let id = 100 + offset;
        let row = wizard
            .areas
            .iter()
            .find(|(_, value)| *value == id)
            .unwrap()
            .0;
        wizard.handle(&mouse(MouseEventKind::Moved, row));
        terminal.draw(|frame| wizard.draw(frame)).unwrap();
        assert_eq!(wizard.directory_offset, offset);
        assert_eq!(wizard.selected, offset);
        assert_eq!(
            terminal.backend().buffer()[(row.x, row.y)].bg,
            THEME.dialog_accent
        );
        let expected = format!("/work/child-{}", offset - 2);
        assert!(
            matches!(wizard.handle(&mouse(MouseEventKind::Down(MouseButton::Left), row)), Action::Browse(path) if path == expected)
        );
        wizard.handle(&Event::Key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE)));
        assert_eq!(wizard.hover, None);
    }

    #[test]
    fn directory_confirmation_works_from_the_list_button_and_edited_path() {
        use crossterm::event::MouseEvent;
        let enter = Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        for (width, height) in [(80, 24), (150, 40)] {
            let mut wizard = Wizard::new(None);
            wizard.set_listing(Listing {
                path: "/work".into(),
                directories: vec!["child".into()],
            });
            assert!(matches!(wizard.handle(&enter), Action::Open(path) if path == "/work"));
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| wizard.draw(frame)).unwrap();
            for id in [100, 5] {
                let (area, _) = wizard
                    .areas
                    .iter()
                    .find(|(_, target)| *target == id)
                    .unwrap();
                let click = Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: area.x + area.width / 2,
                    row: area.y,
                    modifiers: KeyModifiers::NONE,
                });
                assert!(matches!(wizard.handle(&click), Action::Open(path) if path == "/work"));
            }
            wizard.path.set("/work/typed folder".into());
            wizard.focus = 5;
            assert!(
                matches!(wizard.handle(&enter), Action::Open(path) if path == "/work/typed folder")
            );
        }
    }

    #[test]
    fn dialog_layout_keeps_actions_visible_and_exports_optional_previews() {
        for (name, spec) in [
            ("ssh", Some(Spec::parse("/ssh server.example").unwrap())),
            ("wsl", Some(Spec::parse("/wsl Ubuntu").unwrap())),
            ("directory", None),
        ] {
            let mut wizard = Wizard::new(spec);
            if wizard.form.is_none() {
                wizard.set_listing(Listing {
                    path: "/home/me/project".into(),
                    directories: vec!["backup".into(), "models".into(), "src".into()],
                });
            }
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 34)).unwrap();
            terminal.draw(|frame| wizard.draw(frame)).unwrap();
            let action = wizard.areas.iter().find(|(_, id)| *id == 5).unwrap().0;
            assert!(action.width > 0 && action.height == 1);
            assert!(
                wizard
                    .areas
                    .iter()
                    .all(|(area, _)| area.right() <= 100 && area.bottom() <= 34)
            );
            assert!(!wizard.areas.iter().any(|(_, id)| matches!(id, 80 | 81)));
            if let Ok(path) = std::env::var("ORBIT_WIZARD_PREVIEW") {
                let buffer = terminal.backend().buffer();
                let cells = buffer.content.iter().map(|cell| serde_json::json!({"text": cell.symbol(), "fg": format!("{:?}", cell.fg), "bg": format!("{:?}", cell.bg)})).collect::<Vec<_>>();
                std::fs::write(
                    format!("{path}-{name}.json"),
                    serde_json::to_vec(&cells).unwrap(),
                )
                .unwrap();
            }
        }
    }

    #[test]
    fn multiline_failure_is_rendered_on_separate_rows_without_escaped_newlines() {
        let mut wizard = Wizard::new(None);
        wizard.failed = true;
        wizard.status = "summary\nloader detail".into();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 34)).unwrap();
        terminal.draw(|frame| wizard.draw(frame)).unwrap();
        let rows = terminal
            .backend()
            .buffer()
            .content
            .chunks(100)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>();
        let summary = rows
            .iter()
            .position(|line| line.contains("summary"))
            .unwrap();
        assert!(rows[summary + 1].contains("loader detail"));
        assert!(rows.iter().all(|line| !line.contains("\\n")));
    }

    #[test]
    fn popup_fits_small_terminals_and_logs_keep_the_reading_position() {
        let mut wizard = Wizard::new(Some(
            Spec::parse("/ssh host --ssh-user alice --ssh-port 2222").unwrap(),
        ));
        assert_eq!(wizard.form.as_ref().unwrap().fields[1].text, "alice");
        for (width, height) in [(1, 1), (20, 10), (80, 24), (140, 40)] {
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| wizard.draw(frame)).unwrap();
        }
        wizard.form = None;
        wizard.logs = (0..80).map(|index| format!("log {index}")).collect();
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
        terminal.draw(|frame| wizard.draw(frame)).unwrap();
        wizard.handle(&Event::Key(KeyEvent::new(
            KeyCode::PageUp,
            KeyModifiers::NONE,
        )));
        terminal.draw(|frame| wizard.draw(frame)).unwrap();
        let offset = wizard.last_offset;
        wizard.logs.push("another line".into());
        terminal.draw(|frame| wizard.draw(frame)).unwrap();
        assert_eq!(wizard.last_offset, offset);
    }
}
