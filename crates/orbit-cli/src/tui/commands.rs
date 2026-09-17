#[derive(Clone, Copy, Debug)]
pub struct Command {
    pub name: &'static str,
    pub description: &'static str,
}

pub const COMMANDS: &[Command] = &[
    Command {
        name: "/new",
        description: "开始新会话；旧对话保留，可用 /resume 恢复",
    },
    Command {
        name: "/status",
        description: "查看连接配置和会话状态",
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
        name: "/tasks",
        description: "查看或更新当前项目的任务计划",
    },
    Command {
        name: "/memory",
        description: "查看或保存当前项目的长期记忆",
    },
    Command {
        name: "/compact",
        description: "总结旧对话，保留最近两轮；模型不可用时本地回退",
    },
    Command {
        name: "/mode",
        description: "ask 逐次确认 · auto 自动命令 · yolo 自动修改和命令",
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
        "/status" | "/model" | "/context" | "/mode" | "/permissions"
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
    }
}
