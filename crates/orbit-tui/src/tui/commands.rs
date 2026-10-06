#[derive(Clone, Copy, Debug)]
pub struct Command {
    pub name: &'static str,
    pub description: &'static str,
}

pub const COMMAND_NAME_WIDTH: usize = 14;

pub const COMMANDS: &[Command] = &[
    Command {
        name: "/new",
        description: "在新标签开始会话",
    },
    Command {
        name: "/close",
        description: "关闭当前标签；会话仍可通过 /resume 恢复",
    },
    Command {
        name: "/rename",
        description: "修改当前会话的标题",
    },
    Command {
        name: "/status",
        description: "查看连接配置和会话状态",
    },
    Command {
        name: "/diagnostics",
        description: "查看本会话的模型、工具与失败诊断",
    },
    Command {
        name: "/model",
        description: "选择模型；/model refresh 刷新账号目录",
    },
    Command {
        name: "/reasoning",
        description: "选择当前模型的思考强度；用法 /reasoning high",
    },
    Command {
        name: "/connect",
        description: "连接账号或配置供应商 API Key",
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
        name: "/restore",
        description: "选择恢复上次未关闭的标签",
    },
    Command {
        name: "/context",
        description: "查看上下文估算和预算",
    },
    Command {
        name: "/btw",
        description: "独立询问当前上下文；不打断主任务，再次输入可查看记录",
    },
    Command {
        name: "/skills",
        description: "查看已发现的 Skills",
    },
    Command {
        name: "/skill",
        description: "载入 Skill；用法 /skill <名称>",
    },
    Command {
        name: "/mcp",
        description: "查看 MCP server 连接状态",
    },
    Command {
        name: "/plugins",
        description: "管理插件市场与项目启用状态",
    },
    Command {
        name: "/tasks",
        description: "查看或更新当前项目的任务计划",
    },
    Command {
        name: "/memory",
        description: "查看或保存当前会话的长期记忆",
    },
    Command {
        name: "/changes",
        description: "列出当前会话中 Orbit 成功写入的文件",
    },
    Command {
        name: "/diff",
        description: "查看 Orbit 最近一次写入的检查点差异",
    },
    Command {
        name: "/undo",
        description: "回退最近一次 Orbit 写入；可指定 /undo <路径>",
    },
    Command {
        name: "/compact",
        description: "整理旧对话与工具输出，保留近期工作",
    },
    Command {
        name: "/mode",
        description: "ask 逐次确认 · auto 工作区编辑与离线只读命令 · YOLO 自动确认",
    },
    Command {
        name: "/cd",
        description: "将当前会话移动到另一项目；用法 /cd <路径>",
    },
    Command {
        name: "/editor",
        description: "使用外部编辑器编写草稿；保存返回后按 Enter 发送",
    },
    Command {
        name: "/ssh",
        description: "新标签连接 SSH 工作区；/ssh HOST [PATH]",
    },
    Command {
        name: "/wsl",
        description: "新标签连接 WSL 工作区；/wsl DISTRO [PATH]",
    },
    Command {
        name: "/local",
        description: "新建本地会话标签",
    },
    Command {
        name: "/reconnect",
        description: "重新连接当前工作区服务",
    },
    Command {
        name: "/exit",
        description: "退出 Orbit",
    },
];

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
        "/ssh"
            | "/wsl"
            | "/local"
            | "/reconnect"
            | "/new"
            | "/close"
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
        "/ssh"
            | "/wsl"
            | "/local"
            | "/new"
            | "/close"
            | "/reconnect"
            | "/status"
            | "/model"
            | "/mode"
            | "/sessions"
            | "/resume"
            | "/exit"
            | "/help"
            | "/btw"
            | "/compact"
            | "/reasoning"
            | "/context"
            | "/skills"
            | "/skill"
            | "/mcp"
            | "/diagnostics"
            | "/changes"
            | "/diff"
            | "/rename"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_search_accepts_subsequences_and_prioritizes_prefixes() {
        assert_eq!(matching("/rsm")[0].name, "/resume");
        assert_eq!(matching("/MDL")[0].name, "/model");
        assert_eq!(matching("/mode")[0].name, "/model");
        let matches = matching("/s");
        let first_non_prefix = matches
            .iter()
            .position(|command| !command.name.starts_with("/s"))
            .unwrap();
        assert!(
            matches[..first_non_prefix]
                .iter()
                .all(|command| command.name.starts_with("/s"))
        );
        assert!(matches.iter().any(|command| command.name == "/resume"));
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
