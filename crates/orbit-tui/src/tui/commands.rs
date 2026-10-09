#[derive(Clone, Copy, Debug)]
pub struct Command {
    pub name: &'static str,
    pub description: &'static str,
}

pub const COMMAND_NAME_WIDTH: usize = 14;

pub const COMMANDS: &[Command] = &[
    Command {
        name: "/answers",
        description: "查看 Agent 问答记录并回答待处理的问题",
    },
    Command {
        name: "/btw",
        description: "独立询问当前上下文；不打断主任务，关闭后丢弃",
    },
    Command {
        name: "/build",
        description: "返回执行模式，不会自动批准或执行计划",
    },
    Command {
        name: "/cd",
        description: "将当前会话移动到另一项目；用法 /cd <路径>",
    },
    Command {
        name: "/changes",
        description: "列出当前会话中 Orbit 成功写入的文件",
    },
    Command {
        name: "/close",
        description: "关闭当前标签；会话仍可通过 /resume 恢复",
    },
    Command {
        name: "/compact",
        description: "整理旧对话与工具输出，保留近期工作",
    },
    Command {
        name: "/connect",
        description: "连接账号或配置供应商 API Key",
    },
    Command {
        name: "/context",
        description: "查看上下文估算和预算",
    },
    Command {
        name: "/diagnostics",
        description: "查看本会话的模型、工具与失败诊断",
    },
    Command {
        name: "/diff",
        description: "查看 Orbit 最近一次写入的检查点差异",
    },
    Command {
        name: "/editor",
        description: "使用外部编辑器编写草稿；保存返回后按 Enter 发送",
    },
    Command {
        name: "/exit",
        description: "退出 Orbit",
    },
    Command {
        name: "/help",
        description: "查看命令和快捷键帮助",
    },
    Command {
        name: "/local",
        description: "新建本地会话标签",
    },
    Command {
        name: "/mcp",
        description: "查看 MCP server 连接状态",
    },
    Command {
        name: "/memory",
        description: "查看或保存当前会话的长期记忆",
    },
    Command {
        name: "/mode",
        description: "plan 只读规划 · build 执行；审批设置使用 /permissions",
    },
    Command {
        name: "/model",
        description: "选择模型；/model refresh 刷新账号目录",
    },
    Command {
        name: "/new",
        description: "在新标签开始会话",
    },
    Command {
        name: "/plan",
        description: "进入只读规划；/plan view 查看方案并反馈",
    },
    Command {
        name: "/plugins",
        description: "管理插件市场与项目启用状态",
    },
    Command {
        name: "/reasoning",
        description: "选择当前模型的思考强度；用法 /reasoning high",
    },
    Command {
        name: "/reconnect",
        description: "重新连接当前工作区服务",
    },
    Command {
        name: "/rename",
        description: "修改当前会话的标题",
    },
    Command {
        name: "/restore",
        description: "选择恢复上次未关闭的标签",
    },
    Command {
        name: "/resume",
        description: "搜索并恢复当前项目的历史会话",
    },
    Command {
        name: "/sessions",
        description: "打开会话选择器（同 /resume）",
    },
    Command {
        name: "/skill",
        description: "载入 Skill；用法 /skill <名称>",
    },
    Command {
        name: "/skills",
        description: "查看已发现的 Skills",
    },
    Command {
        name: "/ssh",
        description: "新标签连接 SSH 工作区；/ssh HOST [PATH]",
    },
    Command {
        name: "/status",
        description: "查看连接配置和会话状态",
    },
    Command {
        name: "/tasks",
        description: "查看或更新当前项目的任务计划",
    },
    Command {
        name: "/undo",
        description: "回退最近一次 Orbit 写入；可指定 /undo <路径>",
    },
    Command {
        name: "/wsl",
        description: "新标签连接 WSL 工作区；/wsl DISTRO [PATH]",
    },
];

/// Slash commands use a single name; absolute paths remain ordinary messages.
pub fn is_command_input(input: &str) -> bool {
    let name = input.split_whitespace().next().unwrap_or("");
    name.strip_prefix('/').is_some_and(|name| {
        !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphabetic() || matches!(c, '-' | '_'))
    })
}

pub fn matching(input: &str) -> Vec<&'static Command> {
    if !input.starts_with('/') || input.chars().any(char::is_whitespace) {
        return Vec::new();
    }

    super::search::ranked(COMMANDS.iter(), |command| {
        super::search::rank(
            command.name.trim_start_matches('/'),
            input.trim_start_matches('/'),
        )
    })
}

/// Available during a main run. Profile changes affect the next request;
/// `/btw` makes an isolated request and never enters the main history.
pub fn available_while_running(name: &str) -> bool {
    matches!(
        name,
        "/connect"
            | "/help"
            | "/restore"
            | "/editor"
            | "/tasks"
            | "/memory"
            | "/ssh"
            | "/wsl"
            | "/local"
            | "/reconnect"
            | "/new"
            | "/close"
            | "/answers"
            | "/btw"
            | "/status"
            | "/diagnostics"
            | "/model"
            | "/reasoning"
            | "/context"
            | "/skills"
            | "/mcp"
            | "/plugins"
            | "/mode"
            | "/plan"
            | "/build"
            | "/permissions"
            | "/resume"
            | "/sessions"
    )
}

pub fn suggestion(input: &str) -> Option<&'static Command> {
    let typed = input
        .split_once(char::is_whitespace)
        .map_or(input, |(name, _)| name);
    let typed_len = typed.chars().count();

    COMMANDS
        .iter()
        .map(|command| (command, edit_distance(typed, command.name)))
        .min_by_key(|(command, distance)| (*distance, std::cmp::Reverse(command.name.len())))
        .and_then(|(command, distance)| {
            (distance <= 2 || distance.saturating_mul(3) <= typed_len).then_some(command)
        })
}

fn edit_distance(left: &str, right: &str) -> usize {
    let left: Vec<_> = left.chars().collect();
    let right: Vec<_> = right.chars().collect();
    let mut before_previous: Vec<usize> = (0..=right.len()).collect();
    let mut previous = before_previous.clone();

    for (left_index, left_char) in left.iter().enumerate() {
        let mut current = Vec::with_capacity(right.len() + 1);
        current.push(left_index + 1);

        for (right_index, right_char) in right.iter().enumerate() {
            let mut distance = (previous[right_index + 1] + 1)
                .min(current[right_index] + 1)
                .min(previous[right_index] + usize::from(left_char != right_char));

            if left_index > 0
                && right_index > 0
                && left_char == &right[right_index - 1]
                && left[left_index - 1] == *right_char
            {
                distance = distance.min(before_previous[right_index] + 1);
            }

            current.push(distance);
        }

        before_previous = previous;
        previous = current;
    }

    previous[right.len()]
}

pub fn available_in_workspace(name: &str) -> bool {
    matches!(
        name,
        "/cd"
            | "/connect"
            | "/restore"
            | "/editor"
            | "/tasks"
            | "/memory"
            | "/plugins"
            | "/ssh"
            | "/wsl"
            | "/local"
            | "/new"
            | "/close"
            | "/reconnect"
            | "/status"
            | "/model"
            | "/mode"
            | "/plan"
            | "/build"
            | "/sessions"
            | "/resume"
            | "/exit"
            | "/help"
            | "/answers"
            | "/btw"
            | "/compact"
            | "/reasoning"
            | "/context"
            | "/skills"
            | "/skill"
            | "/mcp"
            | "/diagnostics"
            | "/undo"
            | "/changes"
            | "/diff"
            | "/rename"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_search_ranks_matches_and_sorts_ties_alphabetically() {
        assert_eq!(matching("/rsm")[0].name, "/resume");
        assert_eq!(matching("/MDL")[0].name, "/model");
        assert_eq!(matching("/mode")[0].name, "/mode");
        assert_eq!(
            matching("/")
                .iter()
                .map(|command| command.name)
                .collect::<Vec<_>>(),
            COMMANDS
                .iter()
                .map(|command| command.name)
                .collect::<Vec<_>>()
        );
        assert_eq!(
            matching("/se")
                .iter()
                .map(|command| command.name)
                .collect::<Vec<_>>(),
            ["/sessions", "/close", "/answers", "/restore", "/resume"]
        );
        assert!(
            matching("/s")
                .iter()
                .any(|command| command.name == "/resume")
        );
        assert_eq!(matching("/help")[0].name, "/help");
        assert!(available_while_running("/help"));
        assert!(COMMANDS.windows(2).all(|pair| pair[0].name < pair[1].name));
    }

    #[test]
    fn suggests_nearby_command_names() {
        assert_eq!(
            suggestion("/stauts").map(|command| command.name),
            Some("/status")
        );
        assert_eq!(
            suggestion("/modeel").map(|command| command.name),
            Some("/model")
        );
        assert!(suggestion("/unrelated").is_none());
        assert!(
            matching("/r")
                .iter()
                .any(|command| command.name == "/rename")
        );
    }
}
