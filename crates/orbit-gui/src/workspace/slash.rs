//! Composer completions share the draft's focus and the workspace dispatcher.
use super::{actions::WorkspaceCommand as Command, *};
use gpui_kit::component::{
    IndexPath,
    command::{Command as CommandList, CommandItem},
};

#[derive(Clone, Copy)]
enum Action {
    Workspace(Command),
    Usage,
    Argument,
}
struct Entry {
    name: &'static str,
    label: &'static str,
    description: &'static str,
    icon: IconName,
    action: Action,
}
macro_rules! entry {
    ($name:literal, $label:literal, $description:literal, $icon:ident, $action:expr) => {
        Entry {
            name: $name,
            label: $label,
            description: $description,
            icon: IconName::$icon,
            action: $action,
        }
    };
}
const ENTRIES: &[Entry] = &[
    entry!(
        "mcp",
        "MCP",
        "查看工具服务器与连接状态",
        Server,
        Action::Workspace(Command::Mcp)
    ),
    entry!(
        "skills",
        "技能",
        "查看当前工作区可用技能",
        Sparkles,
        Action::Workspace(Command::Skills)
    ),
    entry!(
        "usage",
        "使用情况",
        "查看账号额度与会话用量",
        ChartBar,
        Action::Usage
    ),
    entry!(
        "btw",
        "侧边提问…",
        "发起不打断当前任务的提问",
        MessageCircle,
        Action::Workspace(Command::Ask)
    ),
    entry!(
        "compact",
        "压缩上下文",
        "整理当前会话，释放上下文空间",
        Minimize,
        Action::Workspace(Command::Compact)
    ),
    entry!(
        "context",
        "上下文",
        "查看当前会话的上下文使用情况",
        Info,
        Action::Workspace(Command::Context)
    ),
    entry!(
        "plan",
        "进入计划模式",
        "先只读分析并制定计划；/plan view 查看方案",
        List,
        Action::Workspace(Command::EnterPlan)
    ),
    entry!(
        "build",
        "返回执行模式",
        "返回执行模式，不会自动批准计划",
        SquareTerminal,
        Action::Workspace(Command::LeavePlan)
    ),
    entry!(
        "close",
        "关闭标签页",
        "保留会话记录，运行中的任务继续执行",
        Close,
        Action::Workspace(Command::CloseTask)
    ),
    entry!(
        "stop",
        "停止响应",
        "停止当前会话正在执行的任务",
        Square,
        Action::Workspace(Command::StopTask)
    ),
    entry!(
        "changes",
        "文件变更",
        "查看本轮修改的文件",
        File,
        Action::Workspace(Command::Changes)
    ),
    entry!(
        "file",
        "文件…",
        "浏览、读取和编辑工作区文件",
        File,
        Action::Workspace(Command::File)
    ),
    entry!(
        "diff",
        "查看差异",
        "检查工作区的代码变更",
        GitBranch,
        Action::Workspace(Command::Diff)
    ),
    entry!("memory", "记忆", "查看项目记忆", BookOpen, Action::Argument),
    entry!(
        "plugins",
        "插件…",
        "查看与管理工作区插件",
        Puzzle,
        Action::Workspace(Command::Plugins)
    ),
    entry!(
        "tasks",
        "任务",
        "查看项目任务与执行状态",
        List,
        Action::Argument
    ),
    entry!(
        "queue",
        "待处理输入",
        "查看、编辑或移除尚未处理的消息",
        List,
        Action::Workspace(Command::InputQueue)
    ),
    entry!(
        "status",
        "会话状态",
        "查看会话和工作区状态",
        Info,
        Action::Workspace(Command::Status)
    ),
    entry!(
        "diagnostics",
        "诊断",
        "检查当前会话的运行环境",
        Settings,
        Action::Workspace(Command::Diagnostics)
    ),
    entry!(
        "undo",
        "撤销文件修改",
        "选择要撤销修改的文件",
        File,
        Action::Workspace(Command::Undo)
    ),
    entry!(
        "mode",
        "工作模式",
        "选择规划或执行模式",
        Bot,
        Action::Argument
    ),
    entry!(
        "permissions",
        "审批模式",
        "选择执行审批方式",
        Settings,
        Action::Argument
    ),
    entry!("model", "切换模型", "输入模型 ID", Bot, Action::Argument),
    entry!(
        "reasoning",
        "推理强度",
        "输入推理强度",
        Brain,
        Action::Argument
    ),
    entry!(
        "skill",
        "加载技能",
        "输入要加载的技能名称",
        Sparkles,
        Action::Argument
    ),
    entry!(
        "resume",
        "查找会话",
        "搜索并恢复已有会话，也可输入会话 ID",
        Folder,
        Action::Workspace(Command::Sessions)
    ),
    entry!(
        "cd",
        "切换目录",
        "输入工作目录路径",
        Folder,
        Action::Argument
    ),
    entry!("task", "添加任务", "输入任务标题", Plus, Action::Argument),
];

fn query(draft: &str) -> Option<&str> {
    let query = draft.strip_prefix('/')?;
    query
        .chars()
        .all(|ch| ch.is_alphabetic() || matches!(ch, '-' | '_'))
        .then_some(query)
}
fn command_character(ch: char) -> bool {
    ch.is_ascii_alphabetic() || matches!(ch, '-' | '_')
}

pub(super) fn is_command_input(input: &str) -> bool {
    input
        .split_whitespace()
        .next()
        .and_then(|name| name.strip_prefix('/'))
        .is_some_and(|name| !name.is_empty() && name.chars().all(command_character))
}

pub(super) fn workspace_action(name: &str, argument: &str) -> Option<Command> {
    Some(match (name, argument) {
        ("/status", "") => Command::Status,
        ("/context", "") => Command::Context,
        ("/compact", "") => Command::Compact,
        ("/skills", "") => Command::Skills,
        ("/mcp", "") => Command::Mcp,
        ("/diagnostics", "") => Command::Diagnostics,
        ("/plan", "") | ("/mode", "plan") => Command::EnterPlan,
        ("/plan", "view") => Command::Plan,
        ("/build", "") | ("/mode", "build") => Command::LeavePlan,
        ("/plugins", "") => Command::Plugins,
        ("/memory", "") => Command::Memory,
        ("/btw", "") => Command::Ask,
        ("/queue", "") => Command::InputQueue,
        ("/close", "") => Command::CloseTask,
        ("/stop", "") => Command::StopTask,
        ("/resume" | "/sessions", "") => Command::Sessions,
        _ => return None,
    })
}
fn matches(draft: &str) -> Vec<&'static Entry> {
    let Some(query) = query(draft) else {
        return Vec::new();
    };
    let query = query.to_lowercase();
    ENTRIES
        .iter()
        .filter(|entry| {
            entry.name.contains(&query)
                || entry.label.to_lowercase().contains(&query)
                || entry.description.to_lowercase().contains(&query)
        })
        .collect()
}
#[derive(Default)]
pub(super) struct Arguments {
    input: String,
    entries: Vec<orbit_protocol::Completion>,
    pending: bool,
    error: Option<String>,
    dismissed: Option<String>,
}
impl Arguments {
    fn accept(
        &mut self,
        input: &str,
        result: Result<Vec<orbit_protocol::Completion>, String>,
    ) -> bool {
        if self.input != input || self.dismissed.as_deref() == Some(input) {
            return false;
        }
        self.pending = false;
        match result {
            Ok(entries) => {
                self.entries = entries;
                self.error = None;
            }
            Err(error) => {
                self.entries.clear();
                self.error = Some(error);
            }
        }
        true
    }
}
fn remote_arguments(input: &str) -> bool {
    if input.len() > 4096 || input.contains(['\n', '\r']) {
        return false;
    }
    [
        "/cd ",
        "/diff ",
        "/undo ",
        "/resume ",
        "/tasks start ",
        "/tasks done ",
        "/tasks block ",
        "/tasks cancel ",
        "/memory forget ",
    ]
    .iter()
    .any(|prefix| input.starts_with(prefix))
}

impl Workspace {
    pub(super) fn update_completions(
        &mut self,
        draft: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.completion_task = Task::ready(());
        if self.arguments.dismissed.as_deref() == Some(draft) {
            self.command_open = false;
            return;
        }
        self.arguments = Arguments {
            input: draft.into(),
            ..Default::default()
        };
        if query(draft).is_some() {
            self.command_open = true;
        } else if !draft.contains(['\n', '\r']) {
            if let Some(info) = &self.info {
                self.arguments.entries =
                    orbit_client::completion::arguments(draft, &info.models, &info.skills);
                if let Some(argument) = draft.strip_prefix("/reasoning ") {
                    self.arguments.entries =
                        orbit_protocol::search::ranked(&info.reasoning_efforts, |effort| {
                            orbit_protocol::search::rank(effort, argument)
                        })
                        .into_iter()
                        .map(|effort| orbit_protocol::Completion {
                            name: format!("/reasoning {effort}"),
                            display: Some(effort.clone()),
                            description: "推理强度".into(),
                            continue_input: false,
                        })
                        .collect();
                }
                if remote_arguments(draft) && self.connected {
                    self.arguments.entries.clear();
                    self.arguments.pending = true;
                    let session = info.id.clone();
                    let input = draft.to_owned();
                    self.completion_task = cx.spawn(async move |owner, cx| {
                        cx.background_executor()
                            .timer(Duration::from_millis(100))
                            .await;
                        let _ = owner.update(cx, |view, cx| {
                            if view.arguments.input != input || !view.command_open {
                                return;
                            }
                            let result = view
                                .backend
                                .as_ref()
                                .ok_or_else(|| anyhow::anyhow!("工作区已断开"))
                                .and_then(|backend| {
                                    backend.send(crate::backend::Command::Complete {
                                        session,
                                        input: input.clone(),
                                    })
                                });
                            if let Err(error) = result {
                                view.arguments.accept(&input, Err(error.to_string()));
                                cx.notify();
                            }
                        });
                    });
                }
            }
            self.command_open = self.arguments.pending || !self.arguments.entries.is_empty();
        } else {
            self.command_open = false;
        }
        self.command_search.update(cx, |state, cx| {
            state.set_query(draft.to_owned(), window, cx);
            if state.matched_count() > 0 {
                state.set_selected_index(Some(IndexPath::default().row(0)), window, cx);
            }
        });
    }
    pub(super) fn dismiss_completions(&mut self, cx: &mut Context<Self>) {
        self.arguments.dismissed = Some(self.input.read(cx).value().to_string());
        self.completion_task = Task::ready(());
        self.command_open = false;
        cx.notify();
    }
    pub(super) fn argument_completions(
        &mut self,
        session: &str,
        input: &str,
        result: Result<Vec<orbit_protocol::Completion>, String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.info.as_ref().is_some_and(|info| info.id == session)
            || self.input.read(cx).value().as_str() != input
        {
            return;
        }
        if self.arguments.accept(input, result) {
            self.command_open =
                !self.arguments.entries.is_empty() || self.arguments.error.is_some();
            self.command_search.update(cx, |state, cx| {
                if state.matched_count() > 0 {
                    state.set_selected_index(Some(IndexPath::default().row(0)), window, cx);
                }
            });
            cx.notify();
        }
    }
    pub(super) fn move_completion(
        &mut self,
        step: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let draft = self.input.read(cx).value();
        let count = if query(&draft).is_some() {
            matches(&draft).len()
        } else {
            self.arguments.entries.len()
        };
        if count == 0 {
            return;
        }
        self.command_search.update(cx, |state, cx| {
            let selected = state.selected_index().map(|index| index.row).unwrap_or(0);
            let next = (selected as isize + step).rem_euclid(count as isize) as usize;
            state.set_selected_index(Some(IndexPath::default().row(next)), window, cx);
        });
    }
    pub(super) fn submit_completion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let draft = self.input.read(cx).value();
        if draft
            .strip_prefix("/cd ")
            .is_some_and(|path| path.trim_end_matches(['\'', '"']).ends_with('/'))
        {
            self.dismiss_completions(cx);
            self.dispatch_slash(window, cx);
        } else {
            self.confirm_completion(window, cx);
        }
    }
    pub(super) fn confirm_completion(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let index = self
            .command_search
            .read(cx)
            .selected_index()
            .map(|index| index.row)
            .unwrap_or(0);
        self.choose_completion(index, window, cx);
    }
    fn choose_completion(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if query(&self.input.read(cx).value()).is_none() {
            let Some(entry) = self.arguments.entries.get(index).cloned() else {
                return;
            };
            let value = if entry.continue_input
                && !entry.name.starts_with("/cd ")
                && !entry.name.ends_with("::")
            {
                format!("{} ", entry.name.trim_end())
            } else {
                entry.name
            };
            self.command_open = false;
            self.arguments.dismissed = (!entry.continue_input).then(|| value.clone());
            self.input.update(cx, |input, cx| {
                input.set_value(value.clone(), window, cx);
                input.focus(window, cx);
            });
            self.update_completions(&value, window, cx);
            cx.notify();
            return;
        }
        let Some(entry) = matches(self.input.read(cx).value().as_str())
            .get(index)
            .copied()
        else {
            return;
        };
        self.command_open = false;
        self.input.update(cx, |input, cx| {
            input.set_value(
                if matches!(entry.action, Action::Argument) {
                    format!("/{} ", entry.name)
                } else {
                    String::new()
                },
                window,
                cx,
            );
            input.focus(window, cx);
        });
        match entry.action {
            Action::Workspace(action) => self.execute(action, window, cx),
            Action::Usage => self.show_usage(window, cx),
            Action::Argument => self.update_completions(&format!("/{} ", entry.name), window, cx),
        }
        cx.notify();
    }
    pub(super) fn completion_menu(
        &self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let owner = cx.weak_entity();
        let draft = self.input.read(cx).value();
        let entries: Vec<(String, String, String, IconName)> = if query(&draft).is_some() {
            matches(&draft)
                .into_iter()
                .map(|entry| {
                    (
                        entry.label.into(),
                        entry.description.into(),
                        format!("/{}", entry.name),
                        entry.icon,
                    )
                })
                .collect()
        } else {
            self.arguments
                .entries
                .iter()
                .map(|entry| {
                    (
                        entry.display.clone().unwrap_or_else(|| {
                            entry
                                .name
                                .split_once(' ')
                                .map_or(entry.name.as_str(), |(_, argument)| argument)
                                .to_owned()
                        }),
                        entry.description.clone(),
                        String::new(),
                        IconName::ChevronRight,
                    )
                })
                .collect()
        };
        let empty = if self.arguments.pending {
            "正在查找参数…".to_owned()
        } else {
            self.arguments
                .error
                .clone()
                .unwrap_or_else(|| "没有匹配的候选".into())
        };
        div()
            .id("composer-completions")
            // This deferred surface overlaps the transcript. Occlude its
            // hitbox so wheel events stay in CommandList, including at the
            // scroll edges; a bubble listener runs after gpui::list's wheel
            // handler and cannot reliably protect the transcript behind it.
            .occlude()
            .on_mouse_down_out(cx.listener(|view, _, _, cx| {
                view.dismiss_completions(cx);
            }))
            .absolute()
            .bottom_full()
            .left_0()
            .right_0()
            .mb_2()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .bg(cx.theme().popover)
            .shadow_sm()
            .overflow_hidden()
            .child(
                CommandList::new(&self.command_search)
                    .searchable(false)
                    .filterable(false)
                    .bordered(false)
                    .max_h((window.viewport_size().height * 0.3).min(window.rem_size() * 18.))
                    .items(entries.into_iter().map(|(label, description, name, icon)| {
                        CommandItem::new().label(label.clone()).child(move |_, cx| {
                            h_flex()
                                .w_full()
                                .min_w_0()
                                .gap_3()
                                .child(
                                    Icon::new(icon)
                                        .size_4()
                                        .text_color(cx.theme().muted_foreground),
                                )
                                .child(div().flex_shrink_0().child(label.clone()))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .truncate()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(description.clone()),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(name.clone()),
                                )
                        })
                    }))
                    .empty(move |_, _, cx| {
                        div()
                            .p_3()
                            .text_color(cx.theme().muted_foreground)
                            .child(empty.clone())
                    })
                    .on_confirm(move |index, window, cx| {
                        let _ = owner
                            .update(cx, |view, cx| view.choose_completion(index.row, window, cx));
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{Command, is_command_input, matches, query, workspace_action};
    #[test]
    fn completion_only_claims_a_slash_token_and_preserves_argument_drafts() {
        assert_eq!(query("/"), Some(""));
        assert_eq!(query("/mc"), Some("mc"));
        for draft in [
            "normal text",
            "/btw my question",
            "/model ",
            "/tmp/project\nnotes",
            "/home/user/file.rs",
            "/tmp/design notes.md",
        ] {
            assert_eq!(query(draft), None);
        }
    }
    #[test]
    fn absolute_paths_and_multiline_path_notes_are_chat_messages() {
        for draft in [
            "/home/user/project/main.rs",
            "/tmp/design notes.md",
            "/tmp/project\ninspect this directory",
            "/etc/config.toml",
            "/",
            "ordinary text",
        ] {
            assert!(!is_command_input(draft), "{draft}");
        }
        for draft in ["/plan", "/plan view", "/model provider::model", " /help "] {
            assert!(is_command_input(draft), "{draft}");
        }
    }
    #[test]
    fn planning_aliases_and_bare_resume_route_to_the_same_visible_commands() {
        assert_eq!(workspace_action("/plan", ""), Some(Command::EnterPlan));
        assert_eq!(workspace_action("/mode", "plan"), Some(Command::EnterPlan));
        assert_eq!(workspace_action("/plan", "view"), Some(Command::Plan));
        assert_eq!(workspace_action("/build", ""), Some(Command::LeavePlan));
        assert_eq!(workspace_action("/mode", "build"), Some(Command::LeavePlan));
        for name in ["/resume", "/sessions"] {
            assert_eq!(workspace_action(name, ""), Some(Command::Sessions));
            assert_eq!(workspace_action(name, "session-id"), None);
        }
        assert_eq!(workspace_action("/plan", "unknown"), None);
        assert_eq!(workspace_action("/build", "unexpected"), None);
    }
    #[test]
    fn filters_by_command_name_and_localized_purpose() {
        assert_eq!(matches("/MCP")[0].name, "mcp");
        assert_eq!(matches("/额度")[0].name, "usage");
        assert_eq!(matches("/编辑")[0].name, "file");
        assert!(matches("/not-a-command").is_empty());
    }
}

#[cfg(test)]
mod argument_tests {
    use super::{Arguments, remote_arguments};

    #[test]
    fn only_dynamic_argument_slots_request_workspace_candidates() {
        for input in [
            "/cd ",
            "/diff src/",
            "/resume ",
            "/tasks done ",
            "/memory forget 2",
        ] {
            assert!(remote_arguments(input), "{input}");
        }
        for input in [
            "ordinary text",
            "/tasks add a task",
            "/memory add fact hello",
            "/btw question",
            "/cd src\ntext",
        ] {
            assert!(!remote_arguments(input), "{input}");
        }
    }
    #[test]
    fn delayed_results_never_replace_a_new_query_or_reopen_a_dismissed_menu() {
        let mut arguments = Arguments {
            input: "/cd src/".into(),
            pending: true,
            ..Default::default()
        };
        assert!(!arguments.accept("/cd ", Err("stale error".into())));
        assert!(arguments.pending);
        assert!(arguments.error.is_none());
        assert!(arguments.accept("/cd src/", Ok(vec![])));
        assert!(!arguments.pending);
        arguments.dismissed = Some("/cd src/".into());
        assert!(!arguments.accept("/cd src/", Err("late error".into())));
        assert!(arguments.error.is_none());
    }
}
