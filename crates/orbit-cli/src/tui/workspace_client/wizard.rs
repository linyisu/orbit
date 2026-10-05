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
    workspace::{connect::Spec, directories::Listing},
};

use super::{configuration::Config, controls};

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
        self.focus = 8;
        self.scroll = None;
        self.busy = false;
    }

    fn entries(&self) -> Vec<String> {
        std::iter::once("..".into())
            .chain(
                self.listing
                    .iter()
                    .flat_map(|listing| listing.directories.iter())
                    .filter(|name| self.hidden || !name.starts_with('.'))
                    .cloned(),
            )
            .collect()
    }

    fn browse(&self) -> Action {
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
            if let Some(config) = &mut self.form
                && config.picker
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
                if choice == 99 {
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
                        config.toggle_picker();
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
                } else if let Some(listing) = &self.listing
                    && !self.busy
                {
                    if choice == 5 {
                        return Action::Open(listing.path.clone());
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
                    if config.focus < 5 {
                        config.edit_field(config.focus);
                        form::paste(&mut config.fields[config.focus], text);
                    }
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
        if let Some(listing) = &self.listing
            && !self.busy
        {
            match key.code {
                KeyCode::Enter => {
                    return match self.focus {
                        5 => Action::Open(listing.path.clone()),
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
            controls::panel(false).title(Span::styled(
                " 远程工作区 ",
                Style::default()
                    .fg(THEME.foreground)
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
        frame.render_widget(Paragraph::new(" × "), close);
        self.areas.push((close, 99));
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
                "输入路径后按 Enter 前往",
            );
            self.areas.push((path, 7));
            let list = Rect::new(
                body.x,
                body.y + 3,
                body.width,
                body.height.saturating_sub(3),
            );
            frame.render_widget(controls::panel(self.focus == 8).title(" 文件夹 "), list);
            let list = list.inner(ratatui::layout::Margin::new(1, 1));
            let entries = self.entries();
            let count = list.height as usize;
            let start = self.selected.saturating_sub(count.saturating_sub(1));
            for (row, (index, name)) in entries
                .iter()
                .enumerate()
                .skip(start)
                .take(count)
                .enumerate()
            {
                let rect = Rect::new(list.x, list.y + row as u16, list.width, 1);
                let name = if name == ".." {
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
                        Style::default()
                            .fg(THEME.foreground)
                            .bg(THEME.tab_hover_background)
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
            lines.push(Line::styled(
                render::safe_output(&self.status),
                Style::default().fg(if self.failed {
                    THEME.error
                } else {
                    THEME.dialog_accent
                }),
            ));
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
        let footer = Rect::new(inner.x, inner.bottom() - 4, inner.width, 3);
        let buttons: Vec<(&str, usize)> = if self.form.is_some() {
            vec![("取消", 99), ("连接", 5)]
        } else if self.failed {
            if !matches!(self.focus, 97..=99) {
                self.focus = 97;
            }
            vec![("关闭", 99), ("修改配置", 98), ("重试", 97)]
        } else if self.listing.is_some() && !self.busy {
            vec![
                (
                    if self.hidden {
                        "隐藏项 开"
                    } else {
                        "隐藏项 关"
                    },
                    6,
                ),
                ("取消", 99),
                ("打开此目录", 5),
            ]
        } else {
            vec![("取消连接", 99)]
        };
        let total: u16 = buttons
            .iter()
            .map(|(text, _)| unicode_width::UnicodeWidthStr::width(*text) as u16 + 6)
            .sum::<u16>()
            + (buttons.len().saturating_sub(1) * 2) as u16;
        let compact = total > footer.width;
        let compact_width = footer
            .width
            .saturating_sub((buttons.len().saturating_sub(1) * 2) as u16)
            / buttons.len() as u16;
        let mut x = footer.right().saturating_sub(total).max(footer.x);
        for (text, id) in buttons {
            let width = if compact {
                compact_width
            } else {
                unicode_width::UnicodeWidthStr::width(text) as u16 + 6
            };
            let rect = Rect::new(x, footer.y, width.min(footer.right().saturating_sub(x)), 3);
            let focused = self
                .form
                .as_ref()
                .map_or(self.focus == id, |form| form.focus == id);
            controls::button(
                frame,
                rect,
                text,
                focused || self.hover == Some(id),
                matches!(id, 5 | 97),
            );
            self.areas.push((rect, id));
            x = x.saturating_add(width + 2);
        }
        frame.render_widget(
            Paragraph::new("Tab 切换 · Enter 确认 · Esc 返回")
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
        wizard.selected = 1;
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
            assert!(action.width > 0 && action.height == 3);
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
