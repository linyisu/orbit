use anyhow::{Result, ensure};
use orbit_model::types::ChatMessage;
use orbit_store::types::{MemoryCategory, ProjectTask, SessionMemory, TaskStatus};

use crate::session::SessionState;

pub enum TaskAction {
    List { include_completed: bool },
    Add { title: String },
    SetStatus { id: i64, status: TaskStatus },
}

pub enum TaskResult {
    List(Vec<ProjectTask>),
    Added(ProjectTask),
    Updated(ProjectTask),
}

pub enum MemoryAction {
    List {
        include_inactive: bool,
    },
    Add {
        category: MemoryCategory,
        content: String,
    },
    Search {
        query: String,
    },
    Forget {
        id: i64,
    },
}

pub enum MemoryResult {
    List(Vec<SessionMemory>),
    Added(SessionMemory),
    Search {
        query: String,
        memories: Vec<SessionMemory>,
    },
    Forgotten(SessionMemory),
}

pub async fn task(
    session: &SessionState,
    history: &mut Vec<ChatMessage>,
    action: TaskAction,
) -> Result<TaskResult> {
    let project = &session.snapshot.session.project_id;
    let result = match action {
        TaskAction::List { include_completed } => {
            return Ok(TaskResult::List(
                session.store.tasks(project, include_completed).await?,
            ));
        }
        TaskAction::Add { title } => {
            ensure!(!title.trim().is_empty(), "任务标题不能为空。");
            TaskResult::Added(
                session
                    .store
                    .create_task(project, None, title.trim().into(), String::new(), 0)
                    .await?,
            )
        }
        TaskAction::SetStatus { id, status } => TaskResult::Updated(
            session
                .store
                .update_task(project, id, None, None, Some(status), None)
                .await?,
        ),
    };
    super::inject(history, &session.store, project, session.id()).await?;
    Ok(result)
}

pub async fn memory(
    session: &SessionState,
    history: &mut Vec<ChatMessage>,
    action: MemoryAction,
) -> Result<MemoryResult> {
    let id = session.id();
    let result = match action {
        MemoryAction::List { include_inactive } => {
            return Ok(MemoryResult::List(
                session.store.session_memories(id, include_inactive).await?,
            ));
        }
        MemoryAction::Search { query } => {
            ensure!(!query.trim().is_empty(), "搜索内容不能为空。");
            let query = query.trim().to_owned();
            let memories = session.store.search_session_memories(id, &query, 8).await?;
            return Ok(MemoryResult::Search { query, memories });
        }
        MemoryAction::Add { category, content } => {
            ensure!(!content.trim().is_empty(), "记忆内容不能为空。");
            let content = content.trim().to_owned();
            MemoryResult::Added(
                session
                    .store
                    .remember_in_session(id, category, concise_title(&content), content)
                    .await?,
            )
        }
        MemoryAction::Forget { id: memory } => MemoryResult::Forgotten(
            session
                .store
                .set_session_memory_active(id, memory, false)
                .await?,
        ),
    };
    super::inject(
        history,
        &session.store,
        &session.snapshot.session.project_id,
        id,
    )
    .await?;
    Ok(result)
}

fn concise_title(content: &str) -> String {
    const MAX_CHARS: usize = 72;
    let mut title: String = content.chars().take(MAX_CHARS).collect();
    if content.chars().nth(MAX_CHARS).is_some() {
        title.push('…');
    }
    title
}
