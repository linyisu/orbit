#[derive(Clone, Copy, Debug)]
pub struct Command {
    pub name: &'static str,
    pub description: &'static str,
}

pub const COMMANDS: &[Command] = &[
    Command {
        name: "/help",
        description: "查看命令和快捷键",
    },
    Command {
        name: "/clear",
        description: "清空当前对话，开始新会话",
    },
    Command {
        name: "/status",
        description: "查看连接配置和会话状态",
    },
    Command {
        name: "/model",
        description: "查看模型；/model <名称> 切换模型",
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
