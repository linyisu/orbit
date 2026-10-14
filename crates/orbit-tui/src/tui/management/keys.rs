//! Searchable local terminal keymap editor. Captured keys never reach the chat.
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
use ratatui::{Frame, layout::Rect, widgets::Paragraph};

use super::{Action, Panel};
use crate::{
    tui::{
        components::{self, Control},
        input::InputState,
        keymap::{self, Command, Shared},
        render::safe_output,
    },
    workspace::protocol::Response,
};

#[derive(Debug)]
pub(super) struct State {
    keymap: Shared,
    query: InputState,
    search: bool,
    selected: usize,
    offset: usize,
    capturing: Option<Command>,
    reset_confirmation: bool,
    message: Option<String>,
    body: Rect,
    search_area: Rect,
    rows: Vec<(Rect, usize)>,
    buttons: Vec<(Rect, char)>,
}

impl State {
    pub(super) fn new(keymap: Shared) -> Self {
        Self {
            keymap,
            query: InputState::default(),
            search: false,
            selected: 0,
            offset: 0,
            capturing: None,
            reset_confirmation: false,
            message: None,
            body: Rect::default(),
            search_area: Rect::default(),
            rows: Vec::new(),
            buttons: Vec::new(),
        }
    }

    fn commands(&self) -> Vec<Command> {
        let query = self.query.text.to_lowercase();
        keymap::COMMANDS
            .iter()
            .copied()
            .filter(|command| {
                query.is_empty()
                    || command
                        .label()
                        .to_lowercase()
                        .contains(&query)
                    || command.id().contains(&query)
            })
            .collect()
    }

    fn selected(&self) -> Option<Command> {
        self.commands()
            .get(self.selected)
            .copied()
    }

    fn result(&mut self, result: Result<(), String>) {
        self.message = Some(match result {
            Ok(()) => "快捷键已保存，立即生效".into(),
            Err(error) => error,
        });
    }

    fn activate(&mut self, action: char) -> Action {
        match action {
            'c' => return Action::Close,
            'e' => {
                self.capturing = self.selected();
                self.search = false;
                self.message = None;
            }
            'd' => {
                if let Some(command) = self.selected() {
                    let result = self
                        .keymap
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(command);
                    self.result(result);
                }
            }
            'r' => {
                if let Some(command) = self.selected() {
                    let result = self
                        .keymap
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .restore(command);
                    self.result(result);
                }
            }
            'a' => self.reset_confirmation = true,
            'f' => {
                let warning = {
                    let mut keymap = self
                        .keymap
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    keymap.reload();
                    keymap.warning().map(str::to_owned)
                };
                self.message = warning.or_else(|| Some("已重新读取本地快捷键".into()));
            }
            'y' => {
                let text = self
                    .keymap
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .export();
                return Action::Copy(text);
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
            if let Some(command) = self.capturing {
                if key.code == KeyCode::Esc {
                    self.capturing = None;
                    return Action::None;
                }
                let result = self
                    .keymap
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .capture(command, key);
                if result.is_ok() {
                    self.capturing = None;
                }
                self.result(result);
                return Action::None;
            }
            if self.reset_confirmation {
                match key.code {
                    KeyCode::Char('y' | 'Y') => {
                        self.reset_confirmation = false;
                        let result = self
                            .keymap
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .reset();
                        self.result(result);
                    }
                    KeyCode::Esc | KeyCode::Char('n' | 'N') => self.reset_confirmation = false,
                    _ => {}
                }
                return Action::None;
            }
            if key.code == KeyCode::Esc {
                if self.search {
                    self.search = false;
                    return Action::None;
                }
                return Action::Close;
            }
            if key.code == KeyCode::F(5) {
                return self.activate('f');
            }
            if key.code == KeyCode::Char('r')
                && key
                    .modifiers
                    .contains(KeyModifiers::CONTROL)
            {
                return self.activate('a');
            }
            if key.code == KeyCode::Char('e')
                && key
                    .modifiers
                    .contains(KeyModifiers::CONTROL)
            {
                return self.activate('y');
            }
            if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
                self.search = !self.search;
                return Action::None;
            }
            if self.search {
                if key.code == KeyCode::Enter || key.code == KeyCode::Down {
                    self.search = false;
                    return Action::None;
                }
                self.query
                    .edit(event, self.search_area.width.max(2) as usize);
                self.selected = 0;
                self.offset = 0;
            } else {
                match key.code {
                    KeyCode::Enter => return self.activate('e'),
                    KeyCode::Delete => return self.activate('d'),
                    KeyCode::Char('r' | 'R') => return self.activate('r'),
                    KeyCode::Char('/') => {
                        self.search = true;
                        self.query.select_all();
                    }
                    KeyCode::Up => self.selected = self.selected.saturating_sub(1),
                    KeyCode::Down => {
                        self.selected = self
                            .selected
                            .saturating_add(1)
                            .min(self.commands().len().saturating_sub(1))
                    }
                    KeyCode::PageUp => {
                        self.selected = self
                            .selected
                            .saturating_sub(self.body.height as usize)
                    }
                    KeyCode::PageDown => {
                        self.selected = self
                            .selected
                            .saturating_add(self.body.height as usize)
                            .min(self.commands().len().saturating_sub(1))
                    }
                    KeyCode::Home => self.selected = 0,
                    KeyCode::End => self.selected = self.commands().len().saturating_sub(1),
                    _ => {}
                }
            }
        } else if self.capturing.is_some() || self.reset_confirmation {
            return Action::None;
        } else if let Event::Paste(_) = event {
            self.search = true;
            self.query
                .edit(event, self.search_area.width.max(2) as usize);
            self.selected = 0;
            self.offset = 0;
        } else if let Event::Mouse(mouse) = event {
            let point = (mouse.column, mouse.row).into();
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some((_, action)) = self
                        .buttons
                        .iter()
                        .find(|(area, _)| area.contains(point))
                    {
                        return self.activate(*action);
                    }
                    if self.search_area.contains(point) {
                        self.search = true;
                    }
                    if let Some((_, index)) = self
                        .rows
                        .iter()
                        .find(|(area, _)| area.contains(point))
                    {
                        self.selected = *index;
                        self.search = false;
                        return self.activate('e');
                    }
                }
                MouseEventKind::ScrollUp if self.body.contains(point) => {
                    self.selected = self.selected.saturating_sub(3)
                }
                MouseEventKind::ScrollDown if self.body.contains(point) => {
                    self.selected = self
                        .selected
                        .saturating_add(3)
                        .min(self.commands().len().saturating_sub(1))
                }
                _ => {}
            }
        }
        Action::None
    }

    fn receive(&mut self, _: Response) -> Action {
        Action::None
    }

    fn error(&mut self, error: String) {
        self.message = Some(error);
    }

    fn invalidate_geometry(&mut self) {
        self.body = Rect::default();
        self.search_area = Rect::default();
        self.rows.clear();
        self.buttons.clear();
    }

    fn draw(&mut self, frame: &mut Frame<'_>, area: Rect) {
        self.invalidate_geometry();
        if area.is_empty() {
            return;
        }
        let path = self
            .keymap
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .path()
            .display()
            .to_string();
        components::title(frame, area, format!("终端快捷键 · {path}"));
        if area.height < 4 {
            return;
        }
        frame.render_widget(
            Paragraph::new(
                "应用命令使用单个组合键；终端无法可靠区分 Ctrl+I / Tab、Ctrl+M / Enter。",
            )
            .style(components::muted()),
            Rect::new(area.x, area.y + 1, area.width, 1),
        );
        self.search_area = Rect::new(area.x, area.y + 2, area.width, 1);
        frame.render_widget(
            Paragraph::new(format!(
                "{} {}",
                if self.search { "搜索 >" } else { "搜索 /" },
                safe_output(&self.query.text)
            ))
            .style(if self.search {
                components::accent()
            } else {
                components::muted()
            }),
            self.search_area,
        );
        let footer_height = 3.min(area.height.saturating_sub(3));
        let footer = Rect::new(
            area.x,
            area.bottom() - footer_height,
            area.width,
            footer_height,
        );
        self.body = Rect::new(
            area.x,
            area.y + 3,
            area.width,
            area.height
                .saturating_sub(3 + footer_height),
        );
        let commands = self.commands();
        self.selected = self
            .selected
            .min(commands.len().saturating_sub(1));
        if self.selected < self.offset {
            self.offset = self.selected;
        }
        if self.selected >= self.offset + self.body.height as usize {
            self.offset = self
                .selected
                .saturating_sub(self.body.height.saturating_sub(1) as usize);
        }
        let (bindings, warning) = {
            let keymap = self
                .keymap
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            (
                commands
                    .iter()
                    .map(|command| {
                        (
                            keymap
                                .binding(*command)
                                .unwrap_or_else(|| "未绑定".into()),
                            keymap.overridden(*command),
                        )
                    })
                    .collect::<Vec<_>>(),
                keymap.warning().map(str::to_owned),
            )
        };
        for (row, (index, command)) in commands
            .iter()
            .enumerate()
            .skip(self.offset)
            .take(self.body.height as usize)
            .enumerate()
        {
            let target = Rect::new(self.body.x, self.body.y + row as u16, self.body.width, 1);
            let (binding, overridden) = &bindings[index];
            let text = format!(
                "{}  ·  {}{}",
                command.label(),
                binding,
                if *overridden { "  自定义" } else { "" }
            );
            frame.render_widget(
                Paragraph::new(text).style(if index == self.selected && !self.search {
                    components::highlighted()
                } else {
                    components::normal()
                }),
                target,
            );
            self.rows.push((target, index));
        }
        if commands.is_empty() {
            frame.render_widget(
                Paragraph::new("没有匹配的应用命令").style(components::muted()),
                self.body,
            );
        }
        let status = if let Some(command) = self.capturing {
            format!(
                "按下 {} 的新组合键；Esc 取消。Ctrl+C 保留原有行为。",
                command.label()
            )
        } else if self.reset_confirmation {
            "将全部应用快捷键恢复默认？Y 恢复 · N / Esc 取消".into()
        } else {
            self.message
                .clone()
                .or(warning)
                .unwrap_or_else(|| {
                    "↑↓ 选择 · Enter 修改 · Delete 禁用 · R \
                恢复单项 · Ctrl+R 恢复全部 · Ctrl+E 复制配置"
                        .into()
                })
        };
        frame.render_widget(
            Paragraph::new(safe_output(&status)).style(components::accent()),
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
        for (label, action) in [
            ("修改 Enter", 'e'),
            ("禁用 Del", 'd'),
            ("恢复 R", 'r'),
            ("重置全部", 'a'),
            ("重读 F5", 'f'),
            ("复制配置", 'y'),
            ("关闭 Esc", 'c'),
        ] {
            if let Some(target) = bar.button(
                frame,
                label,
                Control {
                    disabled: self.capturing.is_some()
                        || self.reset_confirmation
                        || (matches!(action, 'e' | 'd' | 'r') && commands.is_empty()),
                    ..Default::default()
                },
            ) {
                self.buttons.push((target, action));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;
    use ratatui::{Terminal, backend::TestBackend};
    use std::sync::{Arc, Mutex};
    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    #[test]
    fn capture_errors_keep_editing_and_a_valid_key_is_persisted_immediately() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(Mutex::new(keymap::State::load(
            &directory.path().join("config.toml"),
        )));
        let mut state = State::new(shared.clone());
        state.handle(&key(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(state.capturing, Some(Command::NewTab));
        state.handle(&key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(state.capturing.is_some());
        assert!(state.message.is_some());
        state.handle(&key(KeyCode::F(6), KeyModifiers::NONE));
        assert!(state.capturing.is_none());
        assert_eq!(
            shared
                .lock()
                .unwrap()
                .resolve(&KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE)),
            Some(Command::NewTab)
        );
        assert!(
            directory
                .path()
                .join("tui-keymap.json")
                .exists()
        );
    }

    #[test]
    fn search_reset_confirmation_and_narrow_render_keep_binding_state() {
        let directory = tempfile::tempdir().unwrap();
        let shared = Arc::new(Mutex::new(keymap::State::load(
            &directory.path().join("config.toml"),
        )));
        let mut state = State::new(shared.clone());
        state.handle(&key(KeyCode::Char('/'), KeyModifiers::NONE));
        state.handle(&Event::Paste("input_history".into()));
        assert_eq!(state.commands().len(), 2);
        state.handle(&key(KeyCode::Enter, KeyModifiers::NONE));
        state.handle(&key(KeyCode::Delete, KeyModifiers::NONE));
        assert_eq!(
            shared
                .lock()
                .unwrap()
                .binding(Command::HistoryPrevious),
            None
        );
        state.handle(&key(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert!(state.reset_confirmation);
        state.handle(&key(KeyCode::Char('n'), KeyModifiers::NONE));
        assert_eq!(
            shared
                .lock()
                .unwrap()
                .binding(Command::HistoryPrevious),
            None
        );
        for (width, height) in [(100, 30), (28, 9), (1, 1)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| state.draw(frame, frame.area()))
                .unwrap();
        }
        assert_eq!(
            shared
                .lock()
                .unwrap()
                .binding(Command::HistoryPrevious),
            None
        );
    }
}
