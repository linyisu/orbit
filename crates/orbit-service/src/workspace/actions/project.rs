use crate::project_state::actions::{MemoryResult, TaskResult};
use orbit_protocol::Report;

pub(super) fn task_report(result: TaskResult) -> Report {
    let text = match result {
        TaskResult::List(tasks) if tasks.is_empty() => "当前项目没有待跟踪的任务。".into(),
        TaskResult::List(tasks) => tasks
            .iter()
            .map(|task| {
                format!(
                    "#{} [{}] {}{}",
                    task.id,
                    task.status.as_str(),
                    task.title,
                    if task.details.is_empty() {
                        String::new()
                    } else {
                        format!("\n  {}", task.details)
                    }
                )
            })
            .collect::<Vec<_>>()
            .join("\n"),
        TaskResult::Added(task) => format!("已添加任务 #{}：{}", task.id, task.title),
        TaskResult::Updated(task) => format!("任务 #{}：{}", task.id, task.status.as_str()),
    };
    super::report("项目任务", text)
}

pub(super) fn memory_report(result: MemoryResult) -> Report {
    use orbit_protocol::project::SessionMemory;
    fn listing(memories: &[SessionMemory]) -> String {
        if memories.is_empty() {
            return "当前会话没有已保存的记忆。".into();
        }
        memories
            .iter()
            .map(|memory| {
                format!(
                    "#{} [{}{}] {}\n  {}",
                    memory.id,
                    memory.category.as_str(),
                    if memory.active { "" } else { " · 已停用" },
                    memory.title,
                    memory.content
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    let text = match result {
        MemoryResult::List(memories) => listing(&memories),
        MemoryResult::Search { query, memories } => {
            format!("搜索：{query}\n{}", listing(&memories))
        }
        MemoryResult::Added(memory) => format!("已保存会话记忆 #{}。", memory.id),
        MemoryResult::Forgotten(memory) => format!("会话记忆 #{} 已停用。", memory.id),
    };
    super::report("会话记忆", text)
}
