use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{Frame, layout::Rect, style::Style, widgets::Paragraph};

use super::wizard::Action;
use crate::{
    tui::{connect_custom::form, input::InputState, theme::THEME},
    workspace::{connect::Spec, discovery},
};

#[derive(Debug)]
pub(crate) struct Config {
    spec: Spec,
    pub(super) fields: [InputState; 5],
    pub(super) focus: usize,
    pub(super) suggestion: usize,
    pub(super) error: Option<String>,
}

impl Config {
    pub(super) fn new(spec: Spec) -> Self {
        let fields = spec.fields().map(|value| {
            let mut input = InputState::default();
            input.set(value);
            input
        });
        let mut config = Self {
            spec,
            fields,
            focus: 0,
            suggestion: 0,
            error: None,
        };
        let (hosts, _) = discovery::snapshot();
        if let Some(host) = hosts.iter().find(|host| host.name == config.fields[0].text) {
            let explicit = config.fields.clone();
            config.fill(host);
            for (index, input) in explicit.into_iter().enumerate().skip(1) {
                if !input.text.is_empty() {
                    config.fields[index] = input;
                }
            }
        }
        config
    }

    pub(super) fn switch(&mut self, wsl: bool) {
        if wsl != self.spec.is_wsl() {
            *self = Self::new(
                Spec::parse(if wsl { "/wsl" } else { "/ssh" }).expect("valid built-in command"),
            );
        }
    }

    fn fill(&mut self, host: &discovery::Host) {
        self.fields[0].set(host.name.clone());
        self.fields[1].set(host.user.clone());
        self.fields[2].set(host.port.clone());
        self.fields[3].set(host.identity.clone());
    }

    fn choices(&self) -> Vec<String> {
        let (hosts, wsl) = discovery::snapshot();
        let names = if self.spec.is_wsl() {
            wsl
        } else {
            hosts.into_iter().map(|host| host.name).collect()
        };
        names
            .into_iter()
            .filter(|name| name.contains(self.fields[0].text.trim()))
            .collect()
    }

    pub(super) fn choose(&mut self) {
        if let Some(name) = self.choices().get(self.suggestion) {
            if self.spec.is_wsl() {
                self.fields[0].set(name.clone());
            } else if let Some(host) = discovery::snapshot()
                .0
                .iter()
                .find(|host| &host.name == name)
            {
                self.fill(host);
            }
        }
        self.suggestion = 0;
    }

    pub(super) fn start(&mut self) -> Action {
        let values = self.fields.clone().map(|input| input.text);
        match self.spec.update(&values) {
            Ok(()) => Action::Start(Box::new(self.spec.clone())),
            Err(error) => {
                self.error = Some(error.to_string());
                Action::None
            }
        }
    }

    pub(super) fn handle(&mut self, key: &crossterm::event::KeyEvent) -> Action {
        let indices: &[usize] = if self.spec.is_wsl() {
            &[0, 1, 4, 5]
        } else {
            &[0, 1, 2, 3, 4, 5]
        };
        match key.code {
            KeyCode::F(2) => self.switch(!self.spec.is_wsl()),
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) || self.focus == 5 => {
                return self.start();
            }
            KeyCode::Enter if self.focus == 0 && !self.choices().is_empty() => {
                self.choose();
                self.focus = 1;
            }
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Enter => {
                let index = indices.iter().position(|i| *i == self.focus).unwrap_or(0);
                let back =
                    key.code == KeyCode::BackTab || key.modifiers.contains(KeyModifiers::SHIFT);
                self.focus = indices[if back {
                    (index + indices.len() - 1) % indices.len()
                } else {
                    (index + 1) % indices.len()
                }];
            }
            KeyCode::Down if self.focus == 0 => {
                self.suggestion = (self.suggestion + 1).min(self.choices().len().saturating_sub(1))
            }
            KeyCode::Up if self.focus == 0 => self.suggestion = self.suggestion.saturating_sub(1),
            _ if self.focus < 5 => {
                form::edit(&mut self.fields[self.focus], key.code, key.modifiers);
                self.suggestion = 0;
            }
            _ => {}
        }
        Action::None
    }

    pub(super) fn draw(
        &mut self,
        frame: &mut Frame<'_>,
        area: Rect,
        areas: &mut Vec<(Rect, usize)>,
    ) {
        let labels = if self.spec.is_wsl() {
            [
                "WSL 发行版",
                "用户名（可选）",
                "",
                "",
                "目录（留空则连接后选择）",
            ]
        } else {
            [
                "SSH 主机／配置别名",
                "用户名（可选，或 user@host）",
                "端口（留空沿用 SSH 配置）",
                "私钥路径（可选；密码将在认证时询问）",
                "目录（留空则连接后选择）",
            ]
        };
        let modes = if self.spec.is_wsl() {
            ["[ SSH ]", "[• WSL ]"]
        } else {
            ["[• SSH ]", "[ WSL ]"]
        };
        for (index, mode) in modes.into_iter().enumerate() {
            let rect = Rect::new(
                area.x + index as u16 * 12,
                area.y,
                11.min(area.width.saturating_sub(index as u16 * 12)),
                1,
            );
            frame.render_widget(
                Paragraph::new(mode).style(Style::default().fg(THEME.dialog_accent)),
                rect,
            );
            areas.push((rect, 80 + index));
        }
        let mut y = area.y + 2;
        if let Some(error) = &self.error {
            frame.render_widget(
                Paragraph::new(error.as_str()).style(Style::default().fg(THEME.error)),
                Rect::new(area.x, y, area.width, 1),
            );
            y += 1;
        }
        let visible = labels
            .iter()
            .enumerate()
            .filter(|(_, label)| !label.is_empty())
            .collect::<Vec<_>>();
        let capacity = (area.bottom().saturating_sub(y) as usize / 3).max(1);
        let focus = visible
            .iter()
            .position(|(index, _)| *index == self.focus)
            .unwrap_or(visible.len().saturating_sub(1));
        let start = focus.saturating_sub(capacity.saturating_sub(1));
        for (index, label) in visible.into_iter().skip(start).take(capacity) {
            if y + 1 >= area.bottom() {
                break;
            }
            frame.render_widget(
                Paragraph::new(*label).style(Style::default().fg(THEME.dialog_accent)),
                Rect::new(area.x, y, area.width, 1),
            );
            let field = Rect::new(area.x, y + 1, area.width, 1);
            frame.render_widget(
                Paragraph::new(form::input_line(
                    &self.fields[index],
                    area.width as usize,
                    false,
                    self.focus == index,
                    "—",
                )),
                field,
            );
            areas.push((field, index));
            if self.focus == index {
                frame.set_cursor_position((
                    area.x
                        + (form::cursor_column(&self.fields[index], area.width as usize, false)
                            as u16)
                            .min(area.width.saturating_sub(1)),
                    y + 1,
                ));
            }
            y += 3;
        }
        if self.focus == 0 {
            let count = area.bottom().saturating_sub(y) as usize;
            let start = self.suggestion.saturating_sub(count.saturating_sub(1));
            for (row, (index, name)) in self
                .choices()
                .iter()
                .enumerate()
                .skip(start)
                .take(count)
                .enumerate()
            {
                let rect = Rect::new(area.x, y + row as u16, area.width, 1);
                frame.render_widget(
                    Paragraph::new(format!(
                        "{} {name}",
                        if index == self.suggestion { "›" } else { " " }
                    )),
                    rect,
                );
                areas.push((rect, 100 + index));
            }
        }
    }
}
