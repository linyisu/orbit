use anyhow::{Result, bail, ensure};
use orbit_protocol::{
    SessionAction,
    project::{MemoryAction, MemoryCategory, TaskAction, TaskStatus},
};

pub(super) fn parse(command: &str, argument: &str) -> Result<SessionAction> {
    let (action, rest) = argument
        .trim()
        .split_once(char::is_whitespace)
        .unwrap_or((argument.trim(), ""));
    let rest = rest.trim();
    Ok(match command {
        "/tasks" => SessionAction::Task {
            task: match action {
                "" | "list" | "all" => TaskAction::List {
                    include_completed: action == "all",
                },
                "add" => {
                    ensure!(!rest.is_empty(), "用法：/tasks add <标题>");
                    TaskAction::Add { title: rest.into() }
                }
                "start" | "done" | "block" | "cancel" => TaskAction::SetStatus {
                    id: rest
                        .parse()
                        .map_err(|_| anyhow::anyhow!("请输入任务编号"))?,
                    status: match action {
                        "start" => TaskStatus::InProgress,
                        "done" => TaskStatus::Completed,
                        "block" => TaskStatus::Blocked,
                        _ => TaskStatus::Cancelled,
                    },
                },
                _ => bail!("用法：/tasks [list|all|add|start|done|block|cancel]"),
            },
        },
        "/memory" => SessionAction::Memory {
            memory: match action {
                "" | "list" | "all" => MemoryAction::List {
                    include_inactive: action == "all",
                },
                "search" => MemoryAction::Search { query: rest.into() },
                "forget" => MemoryAction::Forget {
                    id: rest
                        .parse()
                        .map_err(|_| anyhow::anyhow!("请输入记忆编号"))?,
                },
                "add" => {
                    let (category, content) =
                        rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
                    let category = MemoryCategory::parse(category).ok_or_else(|| {
                        anyhow::anyhow!(
                            "用法：/memory add constraint|decision|fact|preference <内容>"
                        )
                    })?;
                    MemoryAction::Add {
                        category,
                        content: content.trim().into(),
                    }
                }
                _ => bail!("用法：/memory [list|all|search|add|forget]"),
            },
        },
        _ => bail!("未知项目命令"),
    })
}
