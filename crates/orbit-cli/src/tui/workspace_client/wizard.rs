use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::{
    tui::{connect_custom::form, input::InputState, overlay, render, theme::THEME},
    workspace::{connect::Spec, directories::Listing},
};

use super::configuration::Config;

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
    editing_path: bool,
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
            editing_path: false,
            areas: Vec::new(),
        }
    }

    pub fn set_listing(&mut self, listing: Listing) {
        self.path.set(listing.path.clone());
        self.listing = Some(listing);
        self.selected = 0;
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
                    if choice == 80 || choice == 81 {
                        config.switch(choice == 81);
                        return Action::None;
                    }
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
                } else if self.listing.is_some() && !self.busy {
                    if choice == 5 {
                        return Action::Open(self.path.text.clone());
                    }
                    if choice == 6 {
                        self.hidden = !self.hidden;
                        self.selected = 0;
                    }
                    if choice == 7 {
                        self.editing_path = true;
                    }
                    if choice >= 100 {
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
                } else if self.editing_path {
                    form::paste(&mut self.path, text);
                }
            }
            return Action::None;
        };
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
        if self.failed {
            return match key.code {
                KeyCode::Char('r') => Action::Retry,
                KeyCode::Char('b') => Action::Back,
                _ => Action::None,
            };
        }
        if self.listing.is_some() && !self.busy {
            match key.code {
                KeyCode::Tab => self.editing_path = !self.editing_path,
                KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    return Action::Open(self.path.text.clone());
                }
                KeyCode::Enter if self.editing_path => {
                    return Action::Browse(self.path.text.clone());
                }
                KeyCode::Enter => return self.browse(),
                KeyCode::Up if !self.editing_path => {
                    self.selected = self.selected.saturating_sub(1)
                }
                KeyCode::Down if !self.editing_path => {
                    self.selected = (self.selected + 1).min(self.entries().len().saturating_sub(1))
                }
                KeyCode::Char('h') if !self.editing_path => {
                    self.hidden = !self.hidden;
                    self.selected = 0;
                }
                _ if self.editing_path => form::edit(&mut self.path, key.code, key.modifiers),
                _ => {}
            }
        } else {
            match key.code {
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
        let width = bounds.width.saturating_sub(4).min(110);
        let height = bounds.height.saturating_sub(4).min(30);
        let area = Rect::new(
            bounds.x + (bounds.width - width) / 2,
            bounds.y + (bounds.height - height) / 2,
            width,
            height,
        );
        overlay::clear(frame, area);
        self.areas.clear();
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(" 远程工作区 ")
                .border_style(Style::default().fg(THEME.dialog_accent)),
            area,
        );
        let inner = area.inner(ratatui::layout::Margin::new(2, 1));
        if inner.height < 5 || inner.width < 12 {
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
                            Style::default()
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
            inner.height.saturating_sub(4),
        );
        if let Some(config) = &mut self.form {
            config.draw(frame, body, &mut self.areas);
        } else if self.listing.is_some() && !self.failed && !self.busy {
            let path = Rect::new(body.x, body.y, body.width, 1);
            frame.render_widget(
                Paragraph::new(form::input_line(
                    &self.path,
                    path.width as usize,
                    false,
                    self.editing_path,
                    "目标目录",
                )),
                path,
            );
            self.areas.push((path, 7));
            let entries = self.entries();
            let count = body.height.saturating_sub(2) as usize;
            let start = self.selected.saturating_sub(count.saturating_sub(1));
            for (row, (index, name)) in entries
                .iter()
                .enumerate()
                .skip(start)
                .take(count)
                .enumerate()
            {
                let rect = Rect::new(body.x, body.y + 2 + row as u16, body.width, 1);
                frame.render_widget(
                    Paragraph::new(format!(
                        "{} {}",
                        if index == self.selected { "›" } else { " " },
                        render::safe_output(name)
                    ))
                    .style(if index == self.selected {
                        Style::default().fg(THEME.dialog_accent)
                    } else {
                        Style::default()
                    }),
                    rect,
                );
                self.areas.push((rect, 100 + index));
            }
            if self.editing_path {
                frame.set_cursor_position((
                    path.x
                        + (form::cursor_column(&self.path, path.width as usize, false) as u16)
                            .min(path.width.saturating_sub(1)),
                    path.y,
                ));
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
        let footer = Rect::new(inner.x, inner.bottom() - 1, inner.width, 1);
        let buttons: Vec<(&str, usize)> = if self.form.is_some() {
            vec![("[连接 Ctrl+Enter]", 5), ("[取消 Esc]", 99)]
        } else if self.failed {
            vec![("[重试 R]", 97), ("[修改配置 B]", 98), ("[关闭]", 99)]
        } else if self.listing.is_some() {
            vec![
                ("[打开此目录 Ctrl+Enter]", 5),
                ("[显示/隐藏目录 H]", 6),
                ("[取消]", 99),
            ]
        } else {
            vec![("[取消连接 Esc]", 99)]
        };
        let mut x = footer.x;
        for (text, id) in buttons {
            let width = unicode_width::UnicodeWidthStr::width(text) as u16;
            let rect = Rect::new(x, footer.y, width.min(footer.right().saturating_sub(x)), 1);
            frame.render_widget(
                Paragraph::new(text).style(
                    if id == 5 && self.form.as_ref().is_some_and(|form| form.focus == 5) {
                        Style::default().fg(THEME.on_accent).bg(THEME.dialog_accent)
                    } else {
                        Style::default().fg(THEME.dialog_accent)
                    },
                ),
                rect,
            );
            self.areas.push((rect, id));
            x = x.saturating_add(width + 2);
        }
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
        assert!(
            matches!(wizard.handle(&Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL))), Action::Open(path) if path == "/home/me")
        );
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
