#[derive(Clone, Copy, Debug)]
pub struct Command {
    pub name: &'static str,
    pub description: &'static str,
}

/// Keep command descriptions aligned even for the longest built-in name.
pub const COMMAND_NAME_WIDTH: usize = 14;

pub const COMMANDS: &[Command] = &[
    Command {
        name: "/new",
        description: "开始新会话；旧对话保留，可用 /resume 恢复",
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
        description: "选择模型；支持 /model 供应商::模型",
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
        name: "/context",
        description: "查看上下文估算和预算",
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
        description: "ask 逐次确认 · auto 自动命令 · YOLO 自动修改和命令",
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

    COMMANDS
        .iter()
        .filter(|command| command.name.starts_with(input))
        .collect()
}

/// These commands only inspect or adjust local UI state. They may run while an
/// agent request is streaming and never alter that in-flight request, its
/// history, or tool execution. `/model` updates the selected model profile for
/// the next request and persists that selection in the current session. All
/// other commands require the current run to finish or be cancelled first.
pub fn available_while_running(name: &str) -> bool {
    matches!(
        name,
        "/status"
            | "/diagnostics"
            | "/model"
            | "/context"
            | "/skills"
            | "/mcp"
            | "/plugins"
            | "/mode"
            | "/permissions"
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

#[cfg(test)]
mod tests {
    use super::*;

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
