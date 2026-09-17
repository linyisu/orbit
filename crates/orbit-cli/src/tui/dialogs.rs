use super::{app::UiState, commands::COMMANDS};
use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseEventKind};
use orbit_store::types::Session;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
};

#[derive(Debug)]
pub struct SessionPicker {
    sessions: Vec<Session>,
    query: String,
    selected: usize,
    pub error: Option<String>,
}
impl SessionPicker {
    pub fn new(sessions: Vec<Session>) -> Self {
        Self {
            sessions,
            query: String::new(),
            selected: 0,
            error: None,
        }
    }
    fn matches(&self) -> Vec<&Session> {
        let query = self.query.to_lowercase();
        self.sessions
            .iter()
            .filter(|s| s.title.to_lowercase().contains(&query) || s.id.contains(&query))
            .collect()
    }
    fn move_by(&mut self, down: bool) {
        let count = self.matches().len();
        self.selected = if down {
            self.selected.saturating_add(1).min(count.saturating_sub(1))
        } else {
            self.selected.saturating_sub(1)
        };
    }
}

pub fn handle(state: &mut UiState, event: Event) -> Option<String> {
    if let Event::Key(key) = &event {
        if key.kind == KeyEventKind::Release {
            return None;
        }
        if key.code == KeyCode::Esc
            || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            state.show_help = false;
            state.picker = None;
            return None;
        }
    }
    if let Some(picker) = &mut state.picker {
        match event {
            Event::Key(key) => match key.code {
                KeyCode::Up => picker.move_by(false),
                KeyCode::Down => picker.move_by(true),
                KeyCode::Enter => {
                    return picker.matches().get(picker.selected).map(|s| s.id.clone());
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    picker.query.push(c);
                    picker.selected = 0;
                    picker.error = None;
                }
                KeyCode::Backspace => {
                    picker.query.pop();
                    picker.selected = 0;
                    picker.error = None;
                }
                _ => {}
            },
            Event::Paste(text) => {
                picker
                    .query
                    .extend(text.chars().filter(|c| !c.is_control()));
                picker.selected = 0;
            }
            Event::Mouse(mouse) if mouse.kind == MouseEventKind::ScrollUp => picker.move_by(false),
            Event::Mouse(mouse) if mouse.kind == MouseEventKind::ScrollDown => picker.move_by(true),
            _ => {}
        }
    } else if state.show_help {
        match event {
            Event::Key(key) => match key.code {
                KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                    state.help_keys = !state.help_keys;
                    state.help_scroll = 0;
                }
                KeyCode::Up => state.help_scroll = state.help_scroll.saturating_sub(1),
                KeyCode::Down => state.help_scroll += 1,
                _ => {}
            },
            Event::Mouse(mouse) if mouse.kind == MouseEventKind::ScrollUp => {
                state.help_scroll = state.help_scroll.saturating_sub(super::wheel::LINES)
            }
            Event::Mouse(mouse) if mouse.kind == MouseEventKind::ScrollDown => {
                state.help_scroll += super::wheel::LINES
            }
            _ => {}
        }
    }
    None
}

fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

pub fn draw(frame: &mut Frame<'_>, state: &mut UiState) {
    if !state.show_help && state.picker.is_none() {
        return;
    }
    state.selection.clear();
    let full = frame.area();
    let width = full.width.saturating_sub(4).min(96);
    let height = full.height.saturating_sub(2).min(24);
    let area = Rect::new(
        (full.width - width) / 2,
        (full.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, area);
    let title = if state.picker.is_some() {
        " 恢复会话 · 当前项目 "
    } else {
        " Orbit 帮助 "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Rgb(218, 139, 109)));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let header = Rect::new(
        inner.x + 1,
        inner.y,
        inner.width.saturating_sub(2),
        2.min(inner.height),
    );
    let body = Rect::new(
        header.x,
        inner.y + 2,
        header.width,
        inner.height.saturating_sub(4),
    );
    let footer = Rect::new(header.x, inner.bottom().saturating_sub(2), header.width, 2);
    if let Some(picker) = &mut state.picker {
        let search = super::render::fit_label(
            &format!("搜索标题或 ID：{}", clean(&picker.query)),
            header.width.saturating_sub(1) as usize,
            true,
        );
        state.ime_cursor = Some(
            (
                header.x + unicode_width::UnicodeWidthStr::width(search.as_str()) as u16,
                header.y,
            )
                .into(),
        );
        frame.render_widget(Paragraph::new(format!("{search}▏")), header);
        let rows = picker.matches();
        let visible = (body.height as usize / 2).max(1);
        let first = picker.selected.saturating_sub(visible - 1);
        let mut lines = Vec::new();
        for (index, session) in rows.iter().enumerate().skip(first).take(visible) {
            let selected = index == picker.selected;
            let style = if selected {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Rgb(218, 139, 109))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            lines.push(Line::styled(
                format!(
                    "{} {}",
                    if selected { "›" } else { " " },
                    clean(&session.title)
                ),
                style,
            ));
            lines.push(Line::styled(
                format!("  {} · {}", session.id, clean(&session.model)),
                Style::default().fg(Color::Rgb(170, 170, 170)),
            ));
        }
        if lines.is_empty() {
            lines.push(Line::from(if picker.sessions.is_empty() {
                "暂无其他可恢复会话。/new 后可在这里找回之前的对话。"
            } else {
                "没有匹配的会话。"
            }));
        }
        frame.render_widget(Paragraph::new(lines), body);
        let detail = picker
            .error
            .as_deref()
            .map(clean)
            .unwrap_or_else(|| format!("{} 个会话 · 按最近更新排序", rows.len()));
        frame.render_widget(
            Paragraph::new(format!("{detail}\n↑↓ 选择 · Enter 恢复 · Esc 返回")),
            footer,
        );
    } else {
        state.ime_cursor = None;
        frame.render_widget(
            Paragraph::new(if state.help_keys {
                "命令     [快捷键]"
            } else {
                "[命令]     快捷键"
            })
            .style(
                Style::default()
                    .fg(Color::Rgb(218, 139, 109))
                    .add_modifier(Modifier::BOLD),
            ),
            header,
        );
        let lines: Vec<Line> = if state.help_keys {
            [
                "Enter          发送消息",
                "Ctrl+J / Shift+Enter  换行",
                "Ctrl+V / Alt+V  粘贴剪贴板图片或文字",
                "↑ / ↓          移动输入光标，首尾切换历史",
                "滚轮           滚动对话，不移动输入光标",
                "拖选 + Ctrl+C  复制选区",
                "Ctrl+O         展开/收起视口底部的工具结果",
                "Shift+Tab      切换 ask / auto / yolo",
                "Esc            取消响应 / 关闭面板",
                "Ctrl+A / E     移到行首 / 行尾",
                "Ctrl+U / K     删除到行首 / 行尾",
                "Ctrl+C         取消响应 / 清空输入 / 空输入时退出",
                "Ctrl+D         空输入时退出",
                "?              打开快捷键帮助",
            ]
            .into_iter()
            .map(Line::from)
            .collect()
        } else {
            COMMANDS
                .iter()
                .map(|c| Line::from(format!("{:<12} {}", c.name, c.description)))
                .collect()
        };
        let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
        state.help_scroll = state.help_scroll.min(
            paragraph
                .line_count(body.width)
                .saturating_sub(body.height as usize),
        );
        frame.render_widget(paragraph.scroll((state.help_scroll as u16, 0)), body);
        frame.render_widget(
            Paragraph::new("←→ / Tab 切换 · ↑↓ / 滚轮浏览\nEsc 返回对话"),
            footer,
        );
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
    fn session(id: &str, title: &str) -> Session {
        Session {
            id: id.into(),
            title: title.into(),
            workspace: "/project".into(),
            project_id: "project-id".into(),
            model: "model".into(),
            demo: true,
            updated_at: 0,
        }
    }

    #[test]
    fn picker_filters_selects_and_cancels_without_touching_draft() {
        let mut state = UiState::new("model");
        state.input.insert("draft");
        state.picker = Some(SessionPicker::new(vec![
            session("abc", "older task"),
            session("def", "目标任务"),
        ]));
        handle(&mut state, Event::Paste("目标".into()));
        assert_eq!(handle(&mut state, key(KeyCode::Enter)), Some("def".into()));
        handle(&mut state, key(KeyCode::Esc));
        assert!(state.picker.is_none());
        assert_eq!(state.input.text, "draft");
    }

    #[test]
    fn help_has_its_own_panel_and_all_shortcuts_are_reachable() {
        let mut state = UiState::new("model");
        state.show_help = true;
        for (width, height) in [(80, 24), (40, 14), (24, 10)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal.draw(|frame| draw(frame, &mut state)).unwrap();
            let text: String = terminal
                .backend()
                .buffer()
                .content()
                .iter()
                .map(|c| c.symbol())
                .collect();
            assert!(text.contains("Orbit"));
        }
        handle(&mut state, key(KeyCode::Right));
        assert!(state.help_keys);
        for _ in 0..100 {
            handle(&mut state, key(KeyCode::Down));
        }
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal.draw(|frame| draw(frame, &mut state)).unwrap();
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(text.contains("?"));
        assert!(!text.contains("/help"));
        handle(&mut state, key(KeyCode::Esc));
        assert!(!state.show_help);
    }
}
