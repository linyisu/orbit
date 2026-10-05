use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{Frame, layout::Rect, style::Style, widgets::Paragraph};

use super::{controls, wizard::Action};
use crate::{
    tui::{connect_custom::form, input::InputState, overlay, render, theme::THEME},
    workspace::{connect::Spec, discovery},
};

#[derive(Debug)]
pub(crate) struct Config {
    spec: Spec,
    selected_host: Option<discovery::Host>,
    pending_alias: Option<String>,
    pub(super) picker: bool,
    pub(super) alias: InputState,
    pub(super) fields: [InputState; 5],
    pub(super) focus: usize,
    pub(super) suggestion: usize,
    offset: usize,
    rendered_candidates: Vec<Candidate>,
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
            pending_alias: (!spec.is_wsl()).then(|| spec.target()),
            focus: if spec.is_wsl() { 0 } else { 6 },
            selected_host: None,
            picker: !spec.is_wsl(),
            alias: InputState::default(),
            spec,
            fields,
            suggestion: 0,
            offset: 0,
            rendered_candidates: Vec::new(),
            error: None,
        };
        config.refresh_alias();
        config
    }

    fn refresh_alias(&mut self) {
        let Some(alias) = &self.pending_alias else {
            return;
        };
        let (hosts, _) = discovery::snapshot();
        if let Some(host) = hosts.iter().find(|host| &host.name == alias) {
            let explicit = self.fields.clone();
            self.fill(host);
            for (index, input) in explicit.into_iter().enumerate().skip(1) {
                if !input.text.is_empty() {
                    self.fields[index] = input;
                }
            }
        }
    }

    fn fill(&mut self, host: &discovery::Host) {
        self.fields[0].set(if host.hostname.is_empty() {
            host.name.clone()
        } else {
            host.hostname.clone()
        });
        self.fields[1].set(host.user.clone());
        self.fields[2].set(host.port.clone());
        self.fields[3].set(host.identity.clone());
        self.alias.set(host.name.clone());
        self.selected_host = Some(host.clone());
        self.pending_alias = None;
        self.error = None;
    }

    pub(super) fn edit_field(&mut self, index: usize) {
        self.rendered_candidates.clear();
        self.pending_alias = None;
        if index == 0 {
            self.selected_host = None;
        }
        self.error = None;
    }

    pub(super) fn open_picker(&mut self) {
        self.rendered_candidates.clear();
        self.focus = 6;
        self.picker = true;
        self.suggestion = 0;
    }

    fn candidates(&self) -> Vec<Candidate> {
        let (hosts, wsl) = discovery::snapshot();
        if self.spec.is_wsl() {
            wsl.into_iter()
                .filter(|name| {
                    name.to_lowercase()
                        .contains(&self.fields[0].text.trim().to_lowercase())
                })
                .map(Candidate::Distribution)
                .collect()
        } else {
            host_candidates(hosts, &self.alias.text)
        }
    }

    pub(super) fn has_candidates(&self) -> bool {
        self.picker || (self.spec.is_wsl() && self.focus == 0)
    }

    pub(super) fn choose(&mut self) {
        let candidates = if self.rendered_candidates.is_empty() {
            self.candidates()
        } else {
            self.rendered_candidates.clone()
        };
        let Some(candidate) = candidates.into_iter().nth(self.suggestion) else {
            return;
        };
        match candidate {
            Candidate::Distribution(name) => self.fields[0].set(name),
            Candidate::Host(host) => self.fill(&host),
            Candidate::Manual => {
                for field in &mut self.fields[..4] {
                    field.set(String::new());
                }
                self.alias.set(String::new());
                self.selected_host = None;
                self.pending_alias = None;
                self.error = None;
            }
        }
        self.picker = false;
        self.focus = if self.spec.is_wsl() { 1 } else { 0 };
        self.suggestion = 0;
    }

    pub(super) fn paste(&mut self, text: &str) {
        if self.focus == 6 {
            form::paste(&mut self.alias, text);
            self.pending_alias = None;
            self.open_picker();
        } else if self.focus < 5 {
            self.edit_field(self.focus);
            form::paste(&mut self.fields[self.focus], text);
        }
    }

    pub(super) fn start(&mut self) -> Action {
        self.picker = false;
        let mut values = self.fields.clone().map(|input| input.text);
        if let Some(host) = &self.selected_host {
            // Keep the alias so OpenSSH still applies ProxyJump and other host options.
            values[0] = host.name.clone();
        }
        match self.spec.update(&values) {
            Ok(()) => Action::Start(Box::new(self.spec.clone())),
            Err(error) => {
                self.error = Some(error.to_string());
                Action::None
            }
        }
    }

    pub(super) fn handle(&mut self, key: &crossterm::event::KeyEvent) -> Action {
        if key.code == KeyCode::Enter && !key.modifiers.is_empty() {
            return Action::None;
        }
        if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            self.picker = false;
        }
        if self.picker {
            match key.code {
                KeyCode::Esc => self.picker = false,
                KeyCode::Enter => self.choose(),
                KeyCode::Down => {
                    self.suggestion =
                        (self.suggestion + 1).min(self.candidates().len().saturating_sub(1))
                }
                KeyCode::Up => self.suggestion = self.suggestion.saturating_sub(1),
                _ => {
                    form::edit(&mut self.alias, key.code, key.modifiers);
                    self.rendered_candidates.clear();
                    self.pending_alias = None;
                    self.suggestion = 0;
                }
            }
            return Action::None;
        }
        let indices: &[usize] = if self.spec.is_wsl() {
            &[0, 1, 4, 5, 99]
        } else {
            &[6, 0, 2, 1, 3, 4, 5, 99]
        };
        match key.code {
            KeyCode::Enter if self.focus == 99 => return Action::Cancel,
            KeyCode::Enter if self.focus == 5 => {
                return self.start();
            }
            KeyCode::Enter | KeyCode::Down if self.focus == 6 => self.open_picker(),
            KeyCode::Enter
                if self.spec.is_wsl() && self.focus == 0 && !self.candidates().is_empty() =>
            {
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
            KeyCode::Down if self.spec.is_wsl() && self.focus == 0 => {
                self.suggestion =
                    (self.suggestion + 1).min(self.candidates().len().saturating_sub(1))
            }
            KeyCode::Up if self.spec.is_wsl() && self.focus == 0 => {
                self.suggestion = self.suggestion.saturating_sub(1)
            }
            _ if self.focus == 6 => {
                form::edit(&mut self.alias, key.code, key.modifiers);
                self.pending_alias = None;
                self.open_picker();
            }
            _ if self.focus < 5 => {
                let before = self.fields[self.focus].text.clone();
                form::edit(&mut self.fields[self.focus], key.code, key.modifiers);
                if before != self.fields[self.focus].text {
                    self.edit_field(self.focus);
                }
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
        hover: Option<usize>,
    ) {
        self.refresh_alias();
        let mut y = area.y;
        let selector = if !self.spec.is_wsl() && y + 3 <= area.bottom() {
            let rect = Rect::new(area.x, y, area.width, 3);
            controls::field(
                frame,
                rect,
                "SSH 配置 · ~/.ssh/config",
                &self.alias,
                self.focus == 6,
                hover == Some(6),
                "输入别名筛选，或选择手动填写",
            );
            areas.push((rect, 6));
            y += 3;
            Some(rect)
        } else {
            None
        };
        let labels = if self.spec.is_wsl() {
            ["发行版", "用户名", "端口", "私钥", "工作目录"]
        } else {
            ["主机", "用户名", "端口", "私钥", "工作目录"]
        };
        let placeholders = [
            "地址或配置别名",
            "使用默认用户",
            "默认端口",
            "私钥路径（可选）",
            "连接后选择目录",
        ];
        let rows: Vec<Vec<usize>> = if self.spec.is_wsl() {
            vec![vec![0], vec![1], vec![4]]
        } else if area.width >= 64 {
            vec![vec![0, 2], vec![1, 3], vec![4]]
        } else {
            vec![vec![0], vec![2], vec![1], vec![3], vec![4]]
        };
        let capacity = (area.bottom().saturating_sub(y + 1) / 3) as usize;
        let focused_row = rows
            .iter()
            .position(|row| row.contains(&self.focus))
            .unwrap_or(0);
        let start = focused_row.saturating_sub(capacity.saturating_sub(1));
        for row in rows.into_iter().skip(start).take(capacity) {
            let left = if row.first() == Some(&0) {
                area.width * 2 / 3
            } else {
                area.width / 3
            };
            for (column, index) in row.iter().copied().enumerate() {
                let (x, width) = if row.len() == 1 {
                    (area.x, area.width)
                } else if column == 0 {
                    (area.x, left - 1)
                } else {
                    (area.x + left + 1, area.width - left - 1)
                };
                let rect = Rect::new(x, y, width, 3);
                controls::field(
                    frame,
                    rect,
                    labels[index],
                    &self.fields[index],
                    self.focus == index && !self.picker,
                    hover == Some(index),
                    placeholders[index],
                );
                areas.push((rect, index));
            }
            y += 3;
        }
        if y < area.bottom() {
            let text = self
                .error
                .as_deref()
                .unwrap_or("密码和私钥口令将在连接时询问");
            frame.render_widget(
                Paragraph::new(text).style(Style::default().fg(if self.error.is_some() {
                    THEME.error
                } else {
                    THEME.muted
                })),
                Rect::new(area.x, y, area.width, 1),
            );
        }
        let suggestions = if self.picker {
            selector.map(|rect| rect.y + 2)
        } else if self.spec.is_wsl() && self.focus == 0 {
            areas
                .iter()
                .find(|(_, id)| *id == 0)
                .map(|(rect, _)| rect.y + 2)
        } else {
            None
        };
        if let Some(y) = suggestions {
            self.rendered_candidates = self.candidates();
            let candidates = &self.rendered_candidates;
            let count = (area.bottom().saturating_sub(y) as usize)
                .min(render::MAX_COMPLETION_ITEMS)
                .min(candidates.len());
            self.suggestion = self.suggestion.min(candidates.len().saturating_sub(1));
            self.offset = self
                .offset
                .min(self.suggestion)
                .max(self.suggestion.saturating_sub(count.saturating_sub(1)))
                .min(candidates.len().saturating_sub(count));
            let start = self.offset;
            let panel = Rect::new(area.x, y, area.width, count as u16);
            overlay::clear(frame, panel);
            areas.insert(0, (panel, super::wizard::CANDIDATE_PANEL));
            for (row, (index, candidate)) in candidates
                .iter()
                .enumerate()
                .skip(start)
                .take(count)
                .enumerate()
            {
                let rect = render::completion_row(
                    frame,
                    Rect::new(area.x, y + row as u16, area.width, 1),
                    candidate.label(),
                    &candidate.description(),
                    index == self.suggestion,
                );
                areas.insert(0, (rect, 100 + index));
            }
        }
    }
}

#[derive(Clone, Debug)]
enum Candidate {
    Host(discovery::Host),
    Distribution(String),
    Manual,
}

impl Candidate {
    fn label(&self) -> &str {
        match self {
            Self::Host(host) => &host.name,
            Self::Distribution(name) => name,
            Self::Manual => "手动填写新服务器",
        }
    }

    fn description(&self) -> String {
        match self {
            Self::Host(host) => {
                let address = if host.hostname.is_empty() {
                    &host.name
                } else {
                    &host.hostname
                };
                let user = if host.user.is_empty() {
                    String::new()
                } else {
                    format!("{}@", host.user)
                };
                let port = if host.port.is_empty() {
                    String::new()
                } else {
                    format!(":{}", host.port)
                };
                format!("{user}{address}{port}")
            }
            Self::Distribution(_) => "WSL".into(),
            Self::Manual => "填写主机、用户名和端口".into(),
        }
    }
}

fn host_candidates(hosts: Vec<discovery::Host>, query: &str) -> Vec<Candidate> {
    let query = query.trim().to_lowercase();
    hosts
        .into_iter()
        .filter(|host| host.name.to_lowercase().contains(&query))
        .map(Candidate::Host)
        .chain(std::iter::once(Candidate::Manual))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    #[test]
    fn alias_search_filters_saved_hosts_and_keeps_manual_entry_available() {
        let hosts = vec![
            discovery::Host {
                name: "DevBox".into(),
                hostname: "dev.example".into(),
                user: "alice".into(),
                port: "2222".into(),
                ..Default::default()
            },
            discovery::Host {
                name: "production".into(),
                ..Default::default()
            },
        ];
        let candidates = host_candidates(hosts.clone(), " dev ");
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].label(), "DevBox");
        assert_eq!(candidates[0].description(), "alice@dev.example:2222");
        assert!(matches!(candidates[1], Candidate::Manual));
        assert!(matches!(
            host_candidates(hosts, "missing").as_slice(),
            [Candidate::Manual]
        ));
        let mut config = Config::new(Spec::parse("/ssh").unwrap());
        config.handle(&KeyEvent::new(KeyCode::Char('d'), KeyModifiers::NONE));
        config.paste("ev");
        assert_eq!(config.alias.text, "dev");
        assert!(config.picker);
        config.handle(&KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert_eq!(config.focus, 0);
        assert!(!config.picker);
    }

    #[test]
    fn selecting_a_config_host_fills_fields_and_retains_its_ssh_alias() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config");
        std::fs::write(&path, "Host dev\n HostName server.example\n User alice\n Port 2222\n IdentityFile ~/.ssh/dev\n ProxyJump bastion\n").unwrap();
        let hosts = discovery::hosts(&path, directory.path());
        let mut config = Config::new(Spec::parse("/ssh").unwrap());
        config.fill(&hosts[0]);
        assert_eq!(
            config.fields.clone().map(|field| field.text),
            ["server.example", "alice", "2222", "~/.ssh/dev", ""]
        );
        let Action::Start(spec) = config.start() else {
            panic!("valid connection")
        };
        assert_eq!(spec.target(), "dev");
        config.focus = 0;
        config.handle(&KeyEvent::new(KeyCode::Home, KeyModifiers::NONE));
        assert!(config.selected_host.is_some());
        config.handle(&KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE));
        let Action::Start(spec) = config.start() else {
            panic!("custom connection")
        };
        assert_eq!(spec.target(), "xserver.example");
    }

    #[test]
    fn plain_tab_and_enter_reach_connect_without_terminal_shortcuts() {
        let mut config = Config::new(Spec::parse("/ssh server.example").unwrap());
        assert!(matches!(
            config.handle(&KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL)),
            Action::None
        ));
        for _ in 0..6 {
            config.handle(&KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        }
        assert_eq!(config.focus, 5);
        assert!(matches!(
            config.handle(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Action::Start(_)
        ));
        config.handle(&KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
        assert!(matches!(
            config.handle(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            Action::Cancel
        ));
    }

    #[test]
    fn manual_server_clears_inherited_credentials_and_does_not_replace_entered_host() {
        let mut config = Config::new(Spec::parse("/ssh dev /project").unwrap());
        config.fill(&discovery::Host {
            name: "dev".into(),
            hostname: "old.example".into(),
            user: "old-user".into(),
            port: "2222".into(),
            identity: "old-key".into(),
        });
        config.suggestion = config.candidates().len() - 1;
        config.choose();
        assert!(config.fields[..4].iter().all(|field| field.text.is_empty()));
        assert_eq!(config.fields[4].text, "/project");
        config.fields[0].set("new.example".into());
        config.handle(&KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(config.fields[0].text, "new.example");
        assert_eq!(config.focus, 2);
        let Action::Start(spec) = config.start() else {
            panic!("manual connection")
        };
        assert_eq!(spec.target(), "new.example");
        assert_eq!(spec.fields()[1], "");
    }
}
