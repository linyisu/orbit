#[derive(Clone, Copy, Debug)]
pub struct Command {
    pub name: &'static str,
    pub description: &'static str,
}

pub const COMMANDS: &[Command] = &[
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
        name: "/compact",
        description: "总结旧对话，保留最近两轮；真实模式调用模型",
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
