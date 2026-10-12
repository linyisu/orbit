//! Application shortcuts, kept separate from Kit's editor and widget keymaps.
//! Overrides use stable command IDs; an absent entry follows the default and
//! `null` explicitly clears the shortcut.
use crate::{preferences::Preferences, workspace};
use gpui_kit::component::input::Enter;
use gpui_kit::{
    Action, App, DummyKeyboardMapper, Global, KeyBinding, KeyBindingContextPredicate, KeyContext,
    Keymap, Keystroke, NoAction, Unbind,
};
use std::{collections::BTreeMap, fmt};

pub(crate) type Overrides = BTreeMap<String, Option<String>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SaveStatus {
    Saving(u64),
    Saved(u64),
    Failed { revision: u64, message: String },
}

impl SaveStatus {
    pub(crate) fn revision(&self) -> u64 {
        match self {
            Self::Saving(revision) | Self::Saved(revision) | Self::Failed { revision, .. } => {
                *revision
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct LoadWarning {
    id: &'static str,
    configured: String,
    message: String,
}

impl LoadWarning {
    pub(crate) fn id(&self) -> &'static str {
        self.id
    }
    pub(crate) fn configured(&self) -> &str {
        &self.configured
    }
    pub(crate) fn message(&self) -> &str {
        &self.message
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ValidationError {
    Invalid(String),
    Conflict { commands: Vec<&'static str> },
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(message) => f.write_str(message),
            Self::Conflict { commands } => {
                let names = commands
                    .iter()
                    .filter_map(|id| command(id).ok().map(Command::label))
                    .collect::<Vec<_>>();
                write!(f, "与{}的快捷键冲突", names.join("、"))
            }
        }
    }
}
impl std::error::Error for ValidationError {}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Command {
    id: &'static str,
    label: &'static str,
    default: &'static str,
    group: &'static str,
    composer: bool,
    action: fn() -> Box<dyn Action>,
}

impl Command {
    pub(crate) fn id(&self) -> &'static str {
        self.id
    }
    pub(crate) fn label(&self) -> &'static str {
        self.label
    }
    pub(crate) fn default_binding(&self) -> &'static str {
        self.default
    }
    pub(crate) fn group(&self) -> &'static str {
        self.group
    }
    pub(crate) fn scope(&self) -> &'static str {
        if self.composer {
            "消息输入框"
        } else {
            "应用所有窗口"
        }
    }
    pub(crate) fn action_name(&self) -> &'static str {
        (self.action)().name()
    }

    fn contexts(&self) -> &'static [&'static str] {
        if matches!(self.id, "input_history_previous" | "input_history_next") {
            &["Orbit > OrbitComposer > Input"]
        } else if self.composer {
            &["Orbit > (OrbitComposer && !ComposerCompletions) > Input"]
        } else {
            &["Orbit", "Orbit > Input"]
        }
    }
}

macro_rules! command {
    ($id:literal, $label:literal, $key:literal, $group:literal, $action:expr) => {
        Command {
            id: $id,
            label: $label,
            default: $key,
            group: $group,
            composer: false,
            action: || Box::new($action),
        }
    };
}

static COMMANDS: &[Command] = &[
    command!(
        "send_message",
        "发送消息",
        "ctrl-enter",
        "会话",
        workspace::SendMessage
    ),
    command!(
        "new_conversation",
        "新建会话",
        "ctrl-n",
        "会话",
        workspace::NewConversation
    ),
    command!(
        "close_task",
        "关闭当前标签页",
        "ctrl-w",
        "会话",
        workspace::CloseTask
    ),
    command!(
        "stop_task",
        "停止当前响应",
        "ctrl-.",
        "会话",
        workspace::StopTask
    ),
    command!(
        "appearance_settings",
        "设置",
        "ctrl-,",
        "工作区",
        workspace::AppearanceSettings
    ),
    command!(
        "focus_composer",
        "聚焦消息输入框",
        "ctrl-l",
        "会话",
        workspace::FocusComposer
    ),
    command!(
        "toggle_sidebar",
        "显示或隐藏侧栏",
        "ctrl-b",
        "工作区",
        workspace::ToggleSidebar
    ),
    command!(
        "workspace_commands",
        "命令面板",
        "ctrl-shift-p",
        "工作区",
        workspace::WorkspaceCommands
    ),
    command!(
        "show_changes",
        "查看更改",
        "ctrl-shift-d",
        "工作区",
        workspace::ShowChanges
    ),
    command!(
        "search_sessions",
        "查找会话",
        "ctrl-p",
        "会话",
        workspace::SearchSessions
    ),
    command!(
        "open_project",
        "打开项目",
        "ctrl-o",
        "工作区",
        workspace::OpenProject
    ),
    command!(
        "next_task",
        "下一个任务",
        "ctrl-tab",
        "工作区",
        workspace::NextTask
    ),
    command!(
        "previous_task",
        "上一个任务",
        "ctrl-shift-tab",
        "工作区",
        workspace::PreviousTask
    ),
    command!(
        "toggle_inspector",
        "显示或隐藏检查器",
        "ctrl-shift-e",
        "工作区",
        workspace::ToggleInspector
    ),
    Command {
        id: "input_history_previous",
        label: "上一条已发送输入",
        default: "ctrl-up",
        group: "消息输入",
        composer: true,
        action: || Box::new(workspace::InputHistoryPrevious),
    },
    Command {
        id: "input_history_next",
        label: "下一条输入或返回草稿",
        default: "ctrl-down",
        group: "消息输入",
        composer: true,
        action: || Box::new(workspace::InputHistoryNext),
    },
    Command {
        id: "composer_send",
        label: "输入框发送消息",
        default: "enter",
        group: "消息输入",
        composer: true,
        action: || Box::new(workspace::SendMessage),
    },
    Command {
        id: "composer_newline",
        label: "输入框换行",
        default: "shift-enter",
        group: "消息输入",
        composer: true,
        action: || {
            Box::new(Enter {
                secondary: false,
                shift: true,
            })
        },
    },
];

pub(crate) fn catalog() -> &'static [Command] {
    COMMANDS
}

fn command(id: &str) -> Result<&'static Command, ValidationError> {
    COMMANDS
        .iter()
        .find(|command| command.id == id)
        .ok_or_else(|| ValidationError::Invalid(format!("未知快捷键命令：{id}")))
}

fn effective<'a>(command: &Command, overrides: &'a Overrides) -> Option<&'a str> {
    match overrides.get(command.id) {
        Some(value) => value.as_deref(),
        None => Some(command.default),
    }
}

pub(crate) fn current(id: &str, cx: &App) -> Option<String> {
    command(id).ok().and_then(|command| {
        effective(command, &cx.global::<KeymapState>().effective).map(str::to_owned)
    })
}

pub(crate) fn configured(id: &str, cx: &App) -> Option<String> {
    command(id)
        .ok()
        .and_then(|command| effective(command, Preferences::keybindings(cx)).map(str::to_owned))
}

pub(crate) fn load_warnings(cx: &App) -> Vec<LoadWarning> {
    cx.global::<KeymapState>().warnings.clone()
}

pub(crate) fn is_customized(id: &str, cx: &App) -> bool {
    command(id).is_ok_and(|command| {
        effective(command, Preferences::keybindings(cx)) != Some(command.default)
    })
}

pub(crate) fn save_status(cx: &App) -> SaveStatus {
    Preferences::save_status(cx)
}

pub(crate) fn label_for(id: &str, cx: &App) -> Option<String> {
    current(id, cx).map(|binding| display_binding(&binding))
}

pub(crate) fn tooltip(label: &str, id: &str, cx: &App) -> String {
    match label_for(id, cx) {
        Some(binding) => format!("{label} ({binding})"),
        None => label.to_owned(),
    }
}

fn display_binding(binding: &str) -> String {
    binding
        .split_whitespace()
        .map(|stroke| {
            stroke
                .split('-')
                .map(|part| match part {
                    "ctrl" => "Ctrl".to_owned(),
                    "alt" => "Alt".to_owned(),
                    "shift" => "Shift".to_owned(),
                    "cmd" => "Cmd".to_owned(),
                    "super" => "Super".to_owned(),
                    "fn" => "Fn".to_owned(),
                    "enter" => "Enter".to_owned(),
                    "tab" => "Tab".to_owned(),
                    "space" => "Space".to_owned(),
                    other => other.to_uppercase(),
                })
                .collect::<Vec<_>>()
                .join("+")
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Native parsing alone accepts arbitrary key names and even an empty sequence.
/// Reject those, plain typing, and editing/overlay keys before constructing a
/// binding. Prefixes count as conflicts because they delay the shorter action.
fn normalize(command: &Command, text: &str) -> Result<String, ValidationError> {
    let invalid = |message: &str| ValidationError::Invalid(message.to_owned());
    if text.len() > 256
        || text
            .chars()
            .any(|ch| ch.is_control() && !ch.is_whitespace())
        || text.contains("->")
    {
        return Err(invalid("快捷键格式无效"));
    }
    let parts = text.split_whitespace().collect::<Vec<_>>();
    if parts.is_empty() || parts.len() > 3 {
        return Err(invalid("请输入 1 至 3 击快捷键，用空格分隔"));
    }
    let strokes = parts
        .iter()
        .map(|part| {
            let stroke = Keystroke::parse(part)
                .map_err(|_| invalid("无法识别快捷键，请使用 ctrl-k 等键名"))?;
            if !valid_key(&stroke.key) {
                return Err(invalid("无法识别按键名称"));
            }
            Ok(stroke)
        })
        .collect::<Result<Vec<_>, ValidationError>>()?;
    let first = &strokes[0];
    let modifiers = &first.modifiers;
    let command_modifier =
        modifiers.control || modifiers.platform || modifiers.alt || modifiers.function;
    let composer_enter = command.composer && first.key == "enter";
    if !command_modifier && !function_key(&first.key) && !composer_enter {
        return Err(invalid(
            "首击需包含 Ctrl、Alt、Cmd/Super 或功能键，避免覆盖普通输入",
        ));
    }
    if matches!(first.key.as_str(), "escape" | "backspace" | "delete") && !command_modifier {
        return Err(invalid("此按键用于原生编辑和关闭界面"));
    }
    if (modifiers.control || modifiers.platform)
        && !modifiers.alt
        && matches!(first.key.as_str(), "c" | "x" | "v" | "a" | "z" | "y")
    {
        return Err(invalid("此快捷键保留给复制、剪切、粘贴、选择及撤销操作"));
    }
    // Files keeps its own native context. A global action would override Save
    // inside Input, while a composer-only binding cannot reach the file editor.
    if !command.composer
        && (first.key == "f5"
            || (first.key == "s"
                && !modifiers.alt
                && !modifiers.shift
                && if cfg!(target_os = "macos") {
                    modifiers.platform
                } else {
                    modifiers.control
                }))
    {
        return Err(invalid("此快捷键保留给文件保存或刷新"));
    }
    Ok(strokes
        .iter()
        .map(Keystroke::unparse)
        .collect::<Vec<_>>()
        .join(" "))
}

fn function_key(key: &str) -> bool {
    key.strip_prefix('f')
        .and_then(|key| key.parse::<u8>().ok())
        .is_some_and(|number| (1..=35).contains(&number))
}

fn valid_key(key: &str) -> bool {
    key.chars().count() == 1
        || function_key(key)
        || matches!(
            key,
            "enter"
                | "escape"
                | "tab"
                | "space"
                | "backspace"
                | "delete"
                | "up"
                | "down"
                | "left"
                | "right"
                | "home"
                | "end"
                | "pageup"
                | "pagedown"
                | "insert"
        )
}

fn overlapping(left: &str, right: &str) -> bool {
    let left = left.split_whitespace().collect::<Vec<_>>();
    let right = right.split_whitespace().collect::<Vec<_>>();
    left.iter().zip(&right).all(|(left, right)| left == right)
}

fn conflicting_commands(id: &str, binding: &str, overrides: &Overrides) -> Vec<&'static Command> {
    COMMANDS
        .iter()
        .filter(|command| command.id != id)
        .filter(|command| {
            effective(command, overrides).is_some_and(|other| overlapping(binding, other))
        })
        .collect()
}

pub(crate) fn conflicts(
    id: &str,
    text: &str,
    cx: &App,
) -> Result<Vec<&'static Command>, ValidationError> {
    let command = command(id)?;
    let binding = normalize(command, text)?;
    protect_native(command, &binding, &cx.global::<KeymapState>().protected)?;
    Ok(conflicting_commands(
        id,
        &binding,
        &cx.global::<KeymapState>().effective,
    ))
}

pub(crate) fn validate(id: &str, text: &str, cx: &App) -> Result<String, ValidationError> {
    let command = command(id)?;
    let binding = normalize(command, text)?;
    protect_native(command, &binding, &cx.global::<KeymapState>().protected)?;
    reject_conflicts(id, &binding, &cx.global::<KeymapState>().effective)?;
    Ok(binding)
}

fn reject_conflicts(id: &str, binding: &str, overrides: &Overrides) -> Result<(), ValidationError> {
    let commands = conflicting_commands(id, binding, overrides);
    if commands.is_empty() {
        Ok(())
    } else {
        Err(ValidationError::Conflict {
            commands: commands.iter().map(|command| command.id).collect(),
        })
    }
}

/// Validate known entries for import/tests. Startup uses `restore` instead so
/// an invalid hand-edited shortcut never prevents opening the application.
#[cfg(test)]
pub(crate) fn validate_overrides(overrides: &mut Overrides) -> Result<(), ValidationError> {
    for command in COMMANDS {
        if let Some(Some(binding)) = overrides.get_mut(command.id) {
            *binding = normalize(command, binding)?;
        }
    }
    for command in COMMANDS {
        if let Some(binding) = effective(command, overrides) {
            reject_conflicts(command.id, binding, overrides)?;
        }
    }
    Ok(())
}

fn edit(
    overrides: &Overrides,
    id: &str,
    text: &str,
    replace: bool,
) -> Result<Overrides, ValidationError> {
    let command = command(id)?;
    let binding = normalize(command, text)?;
    let conflicts = conflicting_commands(id, &binding, overrides);
    if !replace && !conflicts.is_empty() {
        return Err(ValidationError::Conflict {
            commands: conflicts.iter().map(|command| command.id).collect(),
        });
    }
    let mut next = overrides.clone();
    for conflict in conflicts {
        next.insert(conflict.id.to_owned(), None);
    }
    if binding == command.default {
        next.remove(id);
    } else {
        next.insert(id.to_owned(), Some(binding));
    }
    Ok(next)
}

pub(crate) fn set(id: &str, text: &str, cx: &mut App) -> Result<u64, ValidationError> {
    protect_native_text(id, text, cx)?;
    let next = edit_stored(id, text, false, cx)?;
    Ok(commit(next, cx))
}

pub(crate) fn set_replacing(id: &str, text: &str, cx: &mut App) -> Result<u64, ValidationError> {
    protect_native_text(id, text, cx)?;
    let next = edit_stored(id, text, true, cx)?;
    Ok(commit(next, cx))
}

pub(crate) fn clear(id: &str, cx: &mut App) -> Result<u64, ValidationError> {
    command(id)?;
    let mut next = Preferences::keybindings(cx).clone();
    next.insert(id.to_owned(), None);
    Ok(commit(next, cx))
}

pub(crate) fn reset(id: &str, cx: &mut App) -> Result<u64, ValidationError> {
    set(id, command(id)?.default, cx)
}

pub(crate) fn reset_all(cx: &mut App) -> Result<u64, ValidationError> {
    let mut next = Preferences::keybindings(cx).clone();
    for command in COMMANDS {
        next.remove(command.id);
    }
    Ok(commit(next, cx))
}

#[derive(Default)]
struct KeymapState {
    applied: Vec<KeyBinding>,
    effective: Overrides,
    protected: Vec<NativeBinding>,
    warnings: Vec<LoadWarning>,
}
impl Global for KeymapState {}

struct NativeBinding {
    text: String,
    enter: bool,
}

fn native_input_bindings(keymap: &Keymap) -> Vec<NativeBinding> {
    let input = [KeyContext::parse("Input mode=editor").expect("static input context")];
    keymap
        .bindings()
        .filter(|binding| {
            binding
                .predicate()
                .is_some_and(|predicate| predicate.depth_of(&input).is_some())
                && !binding.action().as_any().is::<Unbind>()
                && !binding.action().as_any().is::<NoAction>()
        })
        .filter_map(|binding| {
            let text = binding
                .keystrokes()
                .iter()
                .map(|stroke| stroke.inner().unparse())
                .collect::<Vec<_>>()
                .join(" ");
            // These keys were already intentionally overridden by Orbit. Keeping
            // them available preserves the existing defaults on every platform.
            if COMMANDS.iter().any(|command| command.default == text) {
                None
            } else {
                Some(NativeBinding {
                    text,
                    enter: binding.action().as_any().is::<Enter>(),
                })
            }
        })
        .collect()
}

fn protect_native(
    command: &Command,
    text: &str,
    protected: &[NativeBinding],
) -> Result<(), ValidationError> {
    if protected
        .iter()
        .any(|native| !(command.composer && native.enter) && overlapping(text, &native.text))
    {
        Err(ValidationError::Invalid(
            "此快捷键与原生文本编辑操作冲突，请选择其它按键".into(),
        ))
    } else {
        Ok(())
    }
}

fn protect_native_text(id: &str, text: &str, cx: &App) -> Result<(), ValidationError> {
    let command = command(id)?;
    let binding = normalize(command, text)?;
    protect_native(command, &binding, &cx.global::<KeymapState>().protected)
}

#[cfg(test)]
fn validate_native_overrides(
    overrides: &Overrides,
    protected: &[NativeBinding],
) -> Result<(), ValidationError> {
    for command in COMMANDS {
        if let Some(binding) = effective(command, overrides) {
            protect_native(command, binding, protected)?;
        }
    }
    Ok(())
}

fn restore(stored: &Overrides, protected: &[NativeBinding]) -> (Overrides, Vec<LoadWarning>) {
    let mut active = stored.clone();
    let mut warnings = Vec::new();
    for command in COMMANDS {
        let Some(Some(text)) = stored.get(command.id) else {
            continue;
        };
        match normalize(command, text).and_then(|binding| {
            protect_native(command, &binding, protected)?;
            Ok(binding)
        }) {
            Ok(binding) => {
                active.insert(command.id.to_owned(), Some(binding));
            }
            Err(error) => {
                active.remove(command.id);
                warnings.push(LoadWarning {
                    id: command.id,
                    configured: text.clone(),
                    message: format!(
                        "{}：{}；暂时使用默认快捷键，原设置仍保留。",
                        command.label, error
                    ),
                });
            }
        }
    }
    // Falling back one invalid entry can expose a default occupied by another
    // override. Resolve all such conflicts without modifying the stored text.
    loop {
        let invalid = COMMANDS
            .iter()
            .filter(|command| {
                active.get(command.id).is_some_and(|value| value.is_some())
                    && effective(command, &active).is_some_and(|binding| {
                        !conflicting_commands(command.id, binding, &active).is_empty()
                    })
            })
            .collect::<Vec<_>>();
        if invalid.is_empty() {
            break;
        }
        for command in invalid {
            let configured = stored
                .get(command.id)
                .and_then(|value| value.clone())
                .unwrap_or_default();
            active.remove(command.id);
            warnings.push(LoadWarning {
                id: command.id,
                configured,
                message: format!(
                    "{}：与其它命令的快捷键冲突；暂时使用默认快捷键，原设置仍保留。",
                    command.label
                ),
            });
        }
    }
    (active, warnings)
}

fn edit_stored(
    id: &str,
    text: &str,
    replace: bool,
    cx: &App,
) -> Result<Overrides, ValidationError> {
    let active = &cx.global::<KeymapState>().effective;
    let edited = edit(active, id, text, replace)?;
    let mut stored = Preferences::keybindings(cx).clone();
    for command in COMMANDS {
        // Include the edited command even if its effective value was already
        // the default: this explicitly repairs a preserved invalid override.
        if command.id == id || edited.get(command.id) != active.get(command.id) {
            match edited.get(command.id) {
                Some(value) => {
                    stored.insert(command.id.to_owned(), value.clone());
                }
                None => {
                    stored.remove(command.id);
                }
            }
        }
    }
    Ok(stored)
}

fn binding(text: &str, action: Box<dyn Action>, context: &str) -> KeyBinding {
    // All key sequences pass normalize first; predicates are static constants.
    KeyBinding::load(
        text,
        action,
        Some(
            KeyBindingContextPredicate::parse(context)
                .expect("static shortcut context")
                .into(),
        ),
        false,
        None,
        &DummyKeyboardMapper,
    )
    .expect("validated shortcut")
}

fn bindings(overrides: &Overrides) -> Vec<KeyBinding> {
    COMMANDS
        .iter()
        .flat_map(|command| {
            effective(command, overrides)
                .into_iter()
                .flat_map(move |text| {
                    command
                        .contexts()
                        .iter()
                        .map(move |context| binding(text, (command.action)(), context))
                })
        })
        .collect()
}

fn replacement(previous: &[KeyBinding], next: &[KeyBinding]) -> Vec<KeyBinding> {
    let mut result = previous
        .iter()
        .map(|old| {
            let text = old
                .keystrokes()
                .iter()
                .map(|stroke| stroke.inner().unparse())
                .collect::<Vec<_>>()
                .join(" ");
            KeyBinding::load(
                &text,
                Box::new(Unbind(old.action().name().into())),
                old.predicate(),
                false,
                None,
                &DummyKeyboardMapper,
            )
            .expect("existing shortcut")
        })
        .collect::<Vec<_>>();
    // Native Unbind only affects earlier bindings. Appending active bindings
    // after the tombstones also makes setting the same key again reliable.
    result.extend_from_slice(next);
    result
}

pub(crate) fn init(cx: &mut App) {
    Preferences::observe_saves(cx);
    if cx.has_global::<KeymapState>() {
        return;
    }
    let protected = native_input_bindings(&cx.key_bindings().borrow());
    let (effective, warnings) = restore(Preferences::keybindings(cx), &protected);
    let enter = Enter {
        secondary: false,
        shift: false,
    };
    cx.bind_keys(["enter", "shift-enter"].map(|text| {
        binding(
            text,
            Box::new(Unbind(enter.name().into())),
            "Orbit > OrbitComposer > Input",
        )
    }));
    let applied = bindings(&effective);
    cx.bind_keys(applied.clone());
    cx.set_global(KeymapState {
        applied,
        effective,
        protected,
        warnings,
    });
}

fn commit(next: Overrides, cx: &mut App) -> u64 {
    let (effective, warnings) = restore(&next, &cx.global::<KeymapState>().protected);
    let active = bindings(&effective);
    let updates = replacement(&cx.global::<KeymapState>().applied, &active);
    cx.bind_keys(updates);
    let state = cx.global_mut::<KeymapState>();
    state.applied = active;
    state.effective = effective;
    state.warnings = warnings;
    // Deliberately publish even a same-value edit: it retries a failed disk
    // write and gives the initiating editor its own persistence receipt.
    Preferences::set_keybindings(next, cx)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(keymap: &Keymap, text: &str, contexts: &[&str]) -> Vec<&'static str> {
        let strokes = text
            .split_whitespace()
            .map(|part| Keystroke::parse(part).unwrap())
            .collect::<Vec<_>>();
        let contexts = contexts
            .iter()
            .map(|context| KeyContext::parse(context).unwrap())
            .collect::<Vec<_>>();
        keymap
            .bindings_for_input(&strokes, &contexts)
            .0
            .iter()
            .map(|binding| binding.action().name())
            .collect()
    }

    #[test]
    fn validation_rejects_typing_unknown_keys_and_native_editing_prefixes() {
        let global = command("toggle_sidebar").unwrap();
        for text in [
            "",
            "a",
            "shift-a",
            "enter",
            "ctrl-notakey",
            "ctrl",
            "ctrl-c",
            "ctrl-z ctrl-b",
            "f5",
            "ctrl-a->x",
            "ctrl-k ctrl-b ctrl-c ctrl-d",
        ] {
            assert!(normalize(global, text).is_err(), "{text}");
        }
        assert!(
            normalize(
                global,
                if cfg!(target_os = "macos") {
                    "cmd-s"
                } else {
                    "ctrl-s"
                }
            )
            .is_err()
        );
        assert_eq!(
            normalize(global, "  ctrl-K   ctrl-b  ").unwrap(),
            "ctrl-shift-k ctrl-b"
        );
        assert_eq!(
            normalize(global, "secondary-k").unwrap(),
            if cfg!(target_os = "macos") {
                "cmd-k"
            } else {
                "ctrl-k"
            }
        );
        assert_eq!(
            normalize(command("composer_send").unwrap(), "enter").unwrap(),
            "enter"
        );
    }

    #[test]
    fn replacement_rechecks_prefix_conflicts_and_clears_them_atomically() {
        let initial = Overrides::new();
        assert!(
            matches!(edit(&initial, "toggle_sidebar", "ctrl-p ctrl-b", false), Err(ValidationError::Conflict { commands }) if commands == ["search_sessions"])
        );
        let next = edit(&initial, "toggle_sidebar", "ctrl-p ctrl-b", true).unwrap();
        assert_eq!(next.get("search_sessions"), Some(&None));
        assert_eq!(
            effective(command("toggle_sidebar").unwrap(), &next),
            Some("ctrl-p ctrl-b")
        );
        assert!(matches!(
            edit(&next, "search_sessions", "ctrl-p", false),
            Err(ValidationError::Conflict { .. })
        ));
    }

    #[test]
    fn native_rebinding_can_clear_reuse_and_restore_the_same_key() {
        let defaults = bindings(&Overrides::new());
        let mut keymap = Keymap::new(defaults.clone());
        let custom = edit(&Overrides::new(), "toggle_sidebar", "ctrl-k ctrl-b", false).unwrap();
        let custom_bindings = bindings(&custom);
        keymap.add_bindings(replacement(&defaults, &custom_bindings));
        assert!(
            !resolved(&keymap, "ctrl-b", &["Orbit", "Input"])
                .contains(&workspace::ToggleSidebar.name())
        );
        assert_eq!(
            resolved(&keymap, "ctrl-k ctrl-b", &["Orbit", "Input"])[0],
            workspace::ToggleSidebar.name()
        );
        keymap.add_bindings(replacement(&custom_bindings, &custom_bindings));
        assert_eq!(
            resolved(&keymap, "ctrl-k ctrl-b", &["Orbit", "Input"])[0],
            workspace::ToggleSidebar.name()
        );
        keymap.add_bindings(replacement(&custom_bindings, &defaults));
        assert_eq!(
            resolved(&keymap, "ctrl-b", &["Orbit", "Input"])[0],
            workspace::ToggleSidebar.name()
        );
        assert!(
            !resolved(&keymap, "ctrl-k ctrl-b", &["Orbit", "Input"])
                .contains(&workspace::ToggleSidebar.name())
        );
    }

    #[test]
    fn composer_newline_and_send_respect_completions_and_other_inputs() {
        let enter = Enter {
            secondary: false,
            shift: false,
        };
        let mut keymap = Keymap::new(vec![
            KeyBinding::new("enter", enter.clone(), Some("Input")),
            KeyBinding::new("ctrl-c", workspace::CompletionDismiss, Some("Input")),
            KeyBinding::new(
                "enter",
                workspace::CompletionSubmit,
                Some("Orbit > ComposerCompletions > Input"),
            ),
        ]);
        keymap.add_bindings(["enter", "shift-enter"].map(|text| {
            binding(
                text,
                Box::new(Unbind(enter.name().into())),
                "Orbit > OrbitComposer > Input",
            )
        }));
        let active = bindings(&Overrides::new());
        keymap.add_bindings(active.clone());
        assert_eq!(
            resolved(&keymap, "enter", &["Orbit", "OrbitComposer", "Input"])[0],
            workspace::SendMessage.name()
        );
        assert_eq!(
            resolved(&keymap, "shift-enter", &["Orbit", "OrbitComposer", "Input"])[0],
            enter.name()
        );
        assert_eq!(
            resolved(&keymap, "enter", &["Orbit", "Input"])[0],
            enter.name()
        );
        assert_eq!(
            resolved(
                &keymap,
                "enter",
                &["Orbit", "OrbitComposer ComposerCompletions", "Input"]
            )[0],
            workspace::CompletionSubmit.name()
        );
        let mut cleared = Overrides::new();
        cleared.insert("composer_send".into(), None);
        cleared.insert("composer_newline".into(), None);
        keymap.add_bindings(replacement(&active, &bindings(&cleared)));
        assert!(resolved(&keymap, "enter", &["Orbit", "OrbitComposer", "Input"]).is_empty());
        assert!(resolved(&keymap, "shift-enter", &["Orbit", "OrbitComposer", "Input"]).is_empty());
        assert_eq!(
            resolved(&keymap, "ctrl-c", &["Orbit", "OrbitComposer", "Input"])[0],
            workspace::CompletionDismiss.name()
        );
    }

    #[test]
    fn history_is_composer_scoped_and_window_commands_can_be_rebound_and_cleared() {
        let defaults = bindings(&Overrides::new());
        let mut keymap = Keymap::new(defaults.clone());
        for context in ["OrbitComposer", "OrbitComposer ComposerCompletions"] {
            assert_eq!(
                resolved(&keymap, "ctrl-up", &["Orbit", context, "Input"])[0],
                workspace::InputHistoryPrevious.name()
            );
            assert_eq!(
                resolved(&keymap, "ctrl-down", &["Orbit", context, "Input"])[0],
                workspace::InputHistoryNext.name()
            );
        }
        assert!(resolved(&keymap, "ctrl-up", &["Orbit", "Input"]).is_empty());
        assert_eq!(
            resolved(&keymap, "ctrl-w", &["Orbit", "Input"])[0],
            workspace::CloseTask.name()
        );
        assert_eq!(
            resolved(&keymap, "ctrl-.", &["Orbit", "Input"])[0],
            workspace::StopTask.name()
        );
        let mut custom = edit(&Overrides::new(), "close_task", "ctrl-alt-w", false).unwrap();
        custom.insert("stop_task".into(), None);
        keymap.add_bindings(replacement(&defaults, &bindings(&custom)));
        assert!(resolved(&keymap, "ctrl-w", &["Orbit", "Input"]).is_empty());
        assert!(resolved(&keymap, "ctrl-.", &["Orbit", "Input"]).is_empty());
        assert_eq!(
            resolved(&keymap, "ctrl-alt-w", &["Orbit", "Input"])[0],
            workspace::CloseTask.name()
        );
    }

    #[test]
    fn persistence_normalizes_known_commands_and_preserves_unknown_ids() {
        let mut overrides = Overrides::new();
        overrides.insert("toggle_sidebar".into(), Some(" ctrl-k   ctrl-b ".into()));
        overrides.insert("future_command".into(), Some("future-key".into()));
        validate_overrides(&mut overrides).unwrap();
        assert_eq!(
            overrides["toggle_sidebar"].as_deref(),
            Some("ctrl-k ctrl-b")
        );
        assert_eq!(overrides["future_command"].as_deref(), Some("future-key"));
        overrides.insert("search_sessions".into(), Some("ctrl-k".into()));
        assert!(matches!(
            validate_overrides(&mut overrides),
            Err(ValidationError::Conflict { .. })
        ));
    }

    #[test]
    fn native_input_protection_matches_startup_and_keeps_legacy_orbit_exceptions() {
        let enter = Enter {
            secondary: false,
            shift: false,
        };
        let native = Keymap::new(vec![
            KeyBinding::new("ctrl-f", workspace::CompletionDismiss, Some("Input")),
            KeyBinding::new(
                "ctrl-left",
                workspace::CompletionNext,
                Some("Input && mode == editor"),
            ),
            KeyBinding::new("ctrl-enter", enter.clone(), Some("Input")),
            KeyBinding::new("enter", enter, Some("Input")),
        ]);
        let protected = native_input_bindings(&native);
        let command = command("toggle_sidebar").unwrap();
        for text in ["ctrl-f", "ctrl-f ctrl-b", "ctrl-left"] {
            assert!(protect_native(command, text, &protected).is_err());
            let overrides = edit(&Overrides::new(), command.id, text, false).unwrap();
            assert!(validate_native_overrides(&overrides, &protected).is_err());
        }
        assert!(protect_native(command, "ctrl-enter", &protected).is_ok());
        assert!(
            protect_native(
                super::command("composer_send").unwrap(),
                "enter",
                &protected
            )
            .is_ok()
        );
        assert!(validate_native_overrides(&Overrides::new(), &protected).is_ok());
    }

    #[test]
    fn invalid_saved_keys_fall_back_without_rewriting_and_can_be_repaired() {
        let native = Keymap::new(vec![KeyBinding::new(
            "ctrl-f",
            workspace::CompletionDismiss,
            Some("Input"),
        )]);
        let protected = native_input_bindings(&native);
        let stored = Overrides::from([
            ("toggle_sidebar".into(), Some("ctrl-f".into())),
            ("search_sessions".into(), Some("ctrl-unknown".into())),
            ("open_project".into(), Some("ctrl-k ctrl-o".into())),
            (
                "future_command".into(),
                Some("unknown-future-syntax".into()),
            ),
        ]);
        let (active, warnings) = restore(&stored, &protected);
        assert_eq!(
            warnings.iter().map(LoadWarning::id).collect::<Vec<_>>(),
            ["toggle_sidebar", "search_sessions"]
        );
        assert_eq!(
            effective(command("toggle_sidebar").unwrap(), &active),
            Some("ctrl-b")
        );
        assert_eq!(
            effective(command("search_sessions").unwrap(), &active),
            Some("ctrl-p")
        );
        assert_eq!(
            effective(command("open_project").unwrap(), &active),
            Some("ctrl-k ctrl-o")
        );
        assert_eq!(stored["toggle_sidebar"].as_deref(), Some("ctrl-f"));
        assert_eq!(stored["future_command"], active["future_command"]);
        let mut repaired = stored.clone();
        repaired.remove("toggle_sidebar");
        let (active, warnings) = restore(&repaired, &protected);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].id(), "search_sessions");
        assert_eq!(active["open_project"].as_deref(), Some("ctrl-k ctrl-o"));
    }

    #[test]
    fn invalid_fallback_resolves_new_default_conflicts_until_stable() {
        let stored = Overrides::from([
            ("toggle_sidebar".into(), Some("invalid-key".into())),
            ("search_sessions".into(), Some("ctrl-b".into())),
            ("open_project".into(), Some("ctrl-p".into())),
        ]);
        let (active, warnings) = restore(&stored, &[]);
        assert_eq!(warnings.len(), 3);
        assert_eq!(
            effective(command("toggle_sidebar").unwrap(), &active),
            Some("ctrl-b")
        );
        assert_eq!(
            effective(command("search_sessions").unwrap(), &active),
            Some("ctrl-p")
        );
        assert_eq!(
            effective(command("open_project").unwrap(), &active),
            Some("ctrl-o")
        );
        assert_eq!(stored["search_sessions"].as_deref(), Some("ctrl-b"));
    }
}
