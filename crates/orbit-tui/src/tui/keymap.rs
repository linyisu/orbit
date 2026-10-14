//! Terminal application shortcuts. Widget editing and Ctrl+C keep their own routes.
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use serde::{Deserialize, Serialize};

pub(super) type Shared = Arc<Mutex<State>>;

/// All live tabs using one client configuration observe edits immediately.
pub(super) fn shared(config: &Path) -> Shared {
    type Registry = Mutex<BTreeMap<PathBuf, std::sync::Weak<Mutex<State>>>>;
    static REGISTRY: std::sync::OnceLock<Registry> = std::sync::OnceLock::new();
    let registry = REGISTRY.get_or_init(Default::default);
    let mut registry = registry
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let path = config
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("tui-keymap.json");
    if let Some(state) = registry
        .get(&path)
        .and_then(std::sync::Weak::upgrade)
    {
        return state;
    }
    let state = Arc::new(Mutex::new(State::load(config)));
    registry.retain(|_, state| state.strong_count() > 0);
    registry.insert(path, Arc::downgrade(&state));
    state
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Command {
    NewTab,
    CloseTab,
    Help,
    Search,
    Plugins,
    Files,
    Review,
    Queue,
    Settings,
    Stop,
    HistoryPrevious,
    HistoryNext,
}

pub(super) const COMMANDS: &[Command] = &[
    Command::NewTab,
    Command::CloseTab,
    Command::Help,
    Command::Search,
    Command::Plugins,
    Command::Files,
    Command::Review,
    Command::Queue,
    Command::Settings,
    Command::Stop,
    Command::HistoryPrevious,
    Command::HistoryNext,
];

impl Command {
    pub(super) fn id(self) -> &'static str {
        match self {
            Self::NewTab => "new_tab",
            Self::CloseTab => "close_tab",
            Self::Help => "help",
            Self::Search => "search",
            Self::Plugins => "plugins",
            Self::Files => "files",
            Self::Review => "review",
            Self::Queue => "queue",
            Self::Settings => "settings",
            Self::Stop => "stop",
            Self::HistoryPrevious => "input_history_previous",
            Self::HistoryNext => "input_history_next",
        }
    }

    pub(super) fn label(self) -> &'static str {
        match self {
            Self::NewTab => "新建会话标签",
            Self::CloseTab => "关闭当前标签",
            Self::Help => "命令与快捷键帮助",
            Self::Search => "搜索历史会话",
            Self::Plugins => "插件市场",
            Self::Files => "工作区文件",
            Self::Review => "Git 变更",
            Self::Queue => "待发送输入队列",
            Self::Settings => "设置",
            Self::Stop => "停止当前响应",
            Self::HistoryPrevious => "上一条输入历史",
            Self::HistoryNext => "下一条输入历史",
        }
    }

    pub(super) fn default(self) -> Option<&'static str> {
        match self {
            Self::Help => Some("?"),
            Self::HistoryPrevious => Some("Ctrl+Up"),
            Self::HistoryNext => Some("Ctrl+Down"),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Chord {
    code: KeyCode,
    modifiers: KeyModifiers,
}

impl Chord {
    fn event(event: &KeyEvent) -> Result<Self, String> {
        if event
            .modifiers
            .intersects(KeyModifiers::SUPER | KeyModifiers::HYPER | KeyModifiers::META)
        {
            return Err("终端无法可靠区分 Super / Meta / Hyper，请使用 Ctrl、Alt 或功能键".into());
        }
        let mut modifiers = event.modifiers;
        let code = match event.code {
            KeyCode::Char(ch) if ch.is_ascii_uppercase() => {
                modifiers.insert(KeyModifiers::SHIFT);
                KeyCode::Char(ch.to_ascii_lowercase())
            }
            KeyCode::Char(ch) if ch.is_ascii_graphic() || ch == ' ' => {
                if !ch.is_ascii_alphabetic() {
                    modifiers.remove(KeyModifiers::SHIFT);
                }
                KeyCode::Char(ch)
            }
            KeyCode::F(number) if (1..=24).contains(&number) => KeyCode::F(number),
            KeyCode::Up
            | KeyCode::Down
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Insert
            | KeyCode::Delete
            | KeyCode::Backspace => event.code,
            _ => return Err("不能绑定此按键；使用 Ctrl、Alt 或 F1–F24 的单个组合键".into()),
        };
        Ok(Self { code, modifiers })
    }

    fn parse(value: &str) -> Result<Self, String> {
        if value == "?" {
            return Ok(Self {
                code: KeyCode::Char('?'),
                modifiers: KeyModifiers::NONE,
            });
        }
        let mut parts = value.split('+').collect::<Vec<_>>();
        let key = parts
            .pop()
            .filter(|key| !key.is_empty())
            .ok_or("快捷键不能为空")?;
        let mut modifiers = KeyModifiers::NONE;
        for modifier in parts {
            let flag = match modifier.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => KeyModifiers::CONTROL,
                "alt" => KeyModifiers::ALT,
                "shift" => KeyModifiers::SHIFT,
                _ => return Err("仅支持 Ctrl、Alt、Shift 的单个组合键".into()),
            };
            if modifiers.contains(flag) {
                return Err("组合键包含重复修饰键".into());
            }
            modifiers.insert(flag);
        }
        let lower = key.to_ascii_lowercase();
        let code = match lower.as_str() {
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" => KeyCode::PageUp,
            "pagedown" => KeyCode::PageDown,
            "insert" => KeyCode::Insert,
            "delete" => KeyCode::Delete,
            "backspace" => KeyCode::Backspace,
            "space" => KeyCode::Char(' '),
            _ if lower.starts_with('f') && lower.len() > 1 => KeyCode::F(
                lower[1..]
                    .parse::<u8>()
                    .ok()
                    .filter(|number| (1..=24).contains(number))
                    .ok_or("功能键须为 F1–F24")?,
            ),
            _ if key.chars().count() == 1 => KeyCode::Char(
                key.chars()
                    .next()
                    .unwrap()
                    .to_ascii_lowercase(),
            ),
            _ => return Err("无法识别快捷键；例如 Ctrl+N、Alt+P 或 F6".into()),
        };
        Self::event(&KeyEvent::new(code, modifiers))
    }

    fn label(&self) -> String {
        let mut parts = Vec::new();
        if self
            .modifiers
            .contains(KeyModifiers::CONTROL)
        {
            parts.push("Ctrl".to_owned());
        }
        if self
            .modifiers
            .contains(KeyModifiers::ALT)
        {
            parts.push("Alt".to_owned());
        }
        if self
            .modifiers
            .contains(KeyModifiers::SHIFT)
        {
            parts.push("Shift".to_owned());
        }
        parts.push(match self.code {
            KeyCode::Char(' ') => "Space".into(),
            KeyCode::Char(ch) => ch.to_ascii_uppercase().to_string(),
            KeyCode::F(number) => format!("F{number}"),
            KeyCode::Up => "Up".into(),
            KeyCode::Down => "Down".into(),
            KeyCode::Left => "Left".into(),
            KeyCode::Right => "Right".into(),
            KeyCode::Home => "Home".into(),
            KeyCode::End => "End".into(),
            KeyCode::PageUp => "PageUp".into(),
            KeyCode::PageDown => "PageDown".into(),
            KeyCode::Insert => "Insert".into(),
            KeyCode::Delete => "Delete".into(),
            KeyCode::Backspace => "Backspace".into(),
            _ => unreachable!("validated terminal chord"),
        });
        parts.join("+")
    }

    fn validate(&self, command: Command) -> Result<(), String> {
        let control = self
            .modifiers
            .contains(KeyModifiers::CONTROL);
        let alt = self
            .modifiers
            .contains(KeyModifiers::ALT);
        if self.code == KeyCode::Char('?') && self.modifiers.is_empty() && command == Command::Help
        {
            return Ok(());
        }
        if !control && !alt && !matches!(self.code, KeyCode::F(_)) {
            return Err("普通文字、方向键和编辑键保留给输入框；请选择 Ctrl、Alt 或功能键".into());
        }
        if (control
            && matches!(
                self.code,
                KeyCode::Char(
                    'a' | 'c' | 'd' | 'e' | 'h' | 'i' | 'j' | 'k' | 'l' | 'm' | 'o' | 'u' | 'v',
                ) | KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Home
                    | KeyCode::End
            ))
            || (alt && matches!(self.code, KeyCode::Char('v' | '1'..='9')))
        {
            return Err("此组合键保留给文本编辑、粘贴或 Ctrl+C 的取消 / 清空 / 退出行为".into());
        }
        Ok(())
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    #[serde(default = "version")]
    version: u8,
    #[serde(default)]
    bindings: BTreeMap<String, Option<String>>,
}

fn version() -> u8 {
    1
}

#[derive(Debug)]
pub(super) struct State {
    path: PathBuf,
    baseline: Option<Vec<u8>>,
    overrides: BTreeMap<String, Option<String>>,
    warning: Option<String>,
}

impl State {
    pub(super) fn load(config: &Path) -> Self {
        let path = config
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("tui-keymap.json");
        let mut state = Self {
            path,
            baseline: None,
            overrides: BTreeMap::new(),
            warning: None,
        };
        state.reload();
        state
    }

    pub(super) fn reload(&mut self) {
        match read(&self.path) {
            Ok(bytes) => {
                self.baseline = bytes.clone();
                match bytes
                    .map(|bytes| {
                        serde_json::from_slice::<Document>(&bytes)
                            .map_err(|error| error.to_string())
                    })
                    .transpose()
                {
                    Ok(document) => {
                        let document = document.unwrap_or_else(|| Document {
                            version: 1,
                            bindings: BTreeMap::new(),
                        });
                        if document.version != 1 {
                            self.warning = Some("不支持的快捷键文件版本；原文件已保留".into());
                            return;
                        }
                        match validate(&document.bindings) {
                            Ok(()) => {
                                self.overrides = document.bindings;
                                self.warning = None;
                            }
                            Err(error) => {
                                self.warning =
                                    Some(format!("快捷键文件无效，原文件已保留：{error}"))
                            }
                        }
                    }
                    Err(error) => {
                        self.warning = Some(format!("无法读取快捷键 JSON，原文件已保留：{error}"))
                    }
                }
            }
            Err(error) => self.warning = Some(error),
        }
    }

    pub(super) fn warning(&self) -> Option<&str> {
        self.warning.as_deref()
    }

    pub(super) fn path(&self) -> &Path {
        &self.path
    }

    pub(super) fn export(&self) -> String {
        serde_json::to_string_pretty(&Document {
            version: 1,
            bindings: self.overrides.clone(),
        })
        .unwrap_or_default()
    }

    pub(super) fn binding(&self, command: Command) -> Option<String> {
        effective(&self.overrides, command).map(str::to_owned)
    }

    pub(super) fn overridden(&self, command: Command) -> bool {
        self.overrides
            .contains_key(command.id())
    }

    pub(super) fn resolve(&self, event: &KeyEvent) -> Option<Command> {
        if event.kind == KeyEventKind::Release {
            return None;
        }
        let chord = Chord::event(event).ok()?;
        COMMANDS
            .iter()
            .copied()
            .find(|command| {
                (event.kind != KeyEventKind::Repeat
                    || matches!(command, Command::HistoryPrevious | Command::HistoryNext))
                    && effective(&self.overrides, *command)
                        .and_then(|value| Chord::parse(value).ok())
                        .is_some_and(|binding| binding == chord)
            })
    }

    pub(super) fn capture(&mut self, command: Command, event: &KeyEvent) -> Result<(), String> {
        let chord = Chord::event(event)?;
        chord.validate(command)?;
        self.set(command, Some(chord.label()))
    }

    pub(super) fn remove(&mut self, command: Command) -> Result<(), String> {
        self.set(command, None)
    }

    fn set(&mut self, command: Command, chord: Option<String>) -> Result<(), String> {
        let mut next = self.overrides.clone();
        next.insert(command.id().into(), chord);
        self.save(next)
    }

    pub(super) fn restore(&mut self, command: Command) -> Result<(), String> {
        let mut next = self.overrides.clone();
        next.remove(command.id());
        self.save(next)
    }

    pub(super) fn reset(&mut self) -> Result<(), String> {
        self.save(BTreeMap::new())
    }

    fn save(&mut self, next: BTreeMap<String, Option<String>>) -> Result<(), String> {
        if self.warning.is_some() {
            return Err(
                "当前快捷键文件无效或已变更；先修复文件并按 F5 重新读取，避免覆盖原内容".into(),
            );
        }
        validate(&next)?;
        let bytes = serde_json::to_vec_pretty(&Document {
            version: 1,
            bindings: next.clone(),
        })
        .map_err(|error| error.to_string())?;
        let parent = self
            .path
            .parent()
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|error| format!("无法创建快捷键目录：{error}"))?;
        let lock_path = self.path.with_extension("json.lock");
        let lock = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&lock_path)
            .map_err(|error| format!("快捷键文件正在被其他客户端写入，或目录不可写：{error}"))?;
        let _guard = Lock {
            path: lock_path,
            _file: lock,
        };
        if read(&self.path)? != self.baseline {
            self.warning = Some("快捷键文件已被其他程序修改；按 F5 重新读取".into());
            return Err(self.warning.clone().unwrap());
        }
        let mut temporary = tempfile::NamedTempFile::new_in(parent)
            .map_err(|error| format!("无法创建快捷键临时文件：{error}"))?;
        temporary
            .write_all(&bytes)
            .and_then(|_| temporary.flush())
            .and_then(|_| temporary.as_file().sync_all())
            .map_err(|error| format!("无法写入快捷键：{error}"))?;
        temporary
            .persist(&self.path)
            .map_err(|error| format!("无法保存快捷键：{}", error.error))?;
        self.baseline = Some(bytes);
        self.overrides = next;
        Ok(())
    }
}

fn effective(overrides: &BTreeMap<String, Option<String>>, command: Command) -> Option<&str> {
    overrides
        .get(command.id())
        .map_or_else(|| command.default(), |value| value.as_deref())
}

fn validate(overrides: &BTreeMap<String, Option<String>>) -> Result<(), String> {
    for id in overrides.keys() {
        if !COMMANDS
            .iter()
            .any(|command| command.id() == id)
        {
            return Err(format!("未知命令：{id}"));
        }
    }
    let mut chords = Vec::<(Command, Chord)>::new();
    for command in COMMANDS {
        if let Some(binding) = effective(overrides, *command) {
            let chord = Chord::parse(binding)?;
            chord.validate(*command)?;
            if let Some((other, _)) = chords
                .iter()
                .find(|(_, other)| *other == chord)
            {
                return Err(format!(
                    "{} 与 {} 的快捷键冲突",
                    command.label(),
                    other.label()
                ));
            }
            chords.push((*command, chord));
        }
    }
    Ok(())
}

fn read(path: &Path) -> Result<Option<Vec<u8>>, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err("快捷键路径不是普通文件；原路径已保留".into());
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("无法读取快捷键文件：{error}")),
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .and_then(|file| {
            file.take(64 * 1_024 + 1)
                .read_to_end(&mut bytes)
        })
        .map_err(|error| format!("无法读取快捷键文件：{error}"))?;
    if bytes.len() > 64 * 1_024 {
        return Err("快捷键文件超过 64 KiB；原文件已保留".into());
    }
    Ok(Some(bytes))
}
struct Lock {
    path: PathBuf,
    _file: fs::File,
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    #[test]
    fn live_tabs_sharing_a_client_keymap_observe_changes_without_reconnect() {
        let directory = tempfile::tempdir().unwrap();
        let config = directory.path().join("config.toml");
        let first = shared(&config);
        let second = shared(&config);
        assert!(Arc::ptr_eq(&first, &second));
        first
            .lock()
            .unwrap()
            .capture(Command::Files, &event(KeyCode::F(6), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(
            second
                .lock()
                .unwrap()
                .resolve(&event(KeyCode::F(6), KeyModifiers::NONE)),
            Some(Command::Files)
        );
        second
            .lock()
            .unwrap()
            .remove(Command::HistoryPrevious)
            .unwrap();
        assert_eq!(
            first
                .lock()
                .unwrap()
                .resolve(&event(KeyCode::Up, KeyModifiers::CONTROL)),
            None
        );
        drop(first);
        drop(second);
        let reopened = shared(&config);
        assert_eq!(
            reopened
                .lock()
                .unwrap()
                .resolve(&event(KeyCode::F(6), KeyModifiers::NONE)),
            Some(Command::Files)
        );
        assert_eq!(
            reopened
                .lock()
                .unwrap()
                .resolve(&event(KeyCode::Up, KeyModifiers::CONTROL)),
            None
        );
    }

    #[test]
    fn editing_keys_ctrl_c_and_ambiguous_sequences_are_reserved() {
        for binding in [
            "Ctrl+C",
            "Ctrl+J",
            "Ctrl+V",
            "Ctrl+Left",
            "Alt+V",
            "N",
            "Left",
            "Ctrl+N Ctrl+P",
            "Ctrl+Ctrl+N",
            "F25",
        ] {
            let parsed = Chord::parse(binding).and_then(|chord| chord.validate(Command::NewTab));
            assert!(parsed.is_err(), "{binding}");
        }
        for binding in ["Ctrl+N", "Alt+P", "F6", "Ctrl+Shift+N"] {
            assert!(
                Chord::parse(binding)
                    .unwrap()
                    .validate(Command::NewTab)
                    .is_ok()
            );
        }
    }

    #[test]
    fn rebinding_conflicts_persistence_disable_and_restore_are_consistent() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let mut state = State::load(&config);
        state
            .capture(
                Command::NewTab,
                &event(KeyCode::Char('n'), KeyModifiers::CONTROL),
            )
            .unwrap();
        assert_eq!(
            state.resolve(&event(KeyCode::Char('n'), KeyModifiers::CONTROL)),
            Some(Command::NewTab)
        );
        assert!(
            state
                .capture(
                    Command::CloseTab,
                    &event(KeyCode::Char('n'), KeyModifiers::CONTROL)
                )
                .is_err()
        );
        let mut loaded = State::load(&config);
        assert_eq!(
            loaded
                .binding(Command::NewTab)
                .as_deref(),
            Some("Ctrl+N")
        );
        loaded
            .remove(Command::HistoryPrevious)
            .unwrap();
        assert_eq!(
            loaded.resolve(&event(KeyCode::Up, KeyModifiers::CONTROL)),
            None
        );
        loaded
            .restore(Command::HistoryPrevious)
            .unwrap();
        assert_eq!(
            loaded.resolve(&event(KeyCode::Up, KeyModifiers::CONTROL)),
            Some(Command::HistoryPrevious)
        );
    }

    #[test]
    fn invalid_and_concurrently_modified_files_are_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let path = dir.path().join("tui-keymap.json");
        fs::write(&path, "invalid user JSON").unwrap();
        let mut state = State::load(&config);
        assert!(state.warning().is_some());
        assert!(state.reset().is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "invalid user JSON");
        fs::remove_file(&path).unwrap();
        state.reload();
        state
            .capture(Command::Files, &event(KeyCode::F(6), KeyModifiers::NONE))
            .unwrap();
        let external = "{\"version\":1,\"bindings\":{\"review\":\"F7\"}}";
        fs::write(&path, external).unwrap();
        assert!(state.remove(Command::Files).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), external);
        assert_eq!(
            state.resolve(&event(KeyCode::F(6), KeyModifiers::NONE)),
            Some(Command::Files)
        );
        state.reload();
        assert_eq!(
            state.resolve(&event(KeyCode::F(7), KeyModifiers::NONE)),
            Some(Command::Review)
        );
    }

    #[test]
    fn uppercase_capture_has_one_canonical_representation_and_releases_do_not_dispatch() {
        let chord = Chord::event(&event(KeyCode::Char('N'), KeyModifiers::CONTROL)).unwrap();
        assert_eq!(chord.label(), "Ctrl+Shift+N");
        assert_eq!(Chord::parse(&chord.label()).unwrap(), chord);
        let dir = tempfile::tempdir().unwrap();
        let mut state = State::load(&dir.path().join("config.toml"));
        state
            .capture(Command::NewTab, &event(KeyCode::F(6), KeyModifiers::NONE))
            .unwrap();
        let mut release = event(KeyCode::F(6), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(state.resolve(&release), None);
    }
}
