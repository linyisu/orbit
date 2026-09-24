//! User-visible project tasks and conversation-local memories.

use anyhow::{Result, ensure};
use rusqlite::{OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::store::Store;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Blocked,
    Completed,
    Cancelled,
}

impl TaskStatus {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(Self::Pending),
            "in_progress" => Some(Self::InProgress),
            "blocked" => Some(Self::Blocked),
            "completed" => Some(Self::Completed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in_progress",
            Self::Blocked => "blocked",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProjectTask {
    pub id: i64,
    pub workspace: String,
    pub parent_id: Option<i64>,
    pub title: String,
    pub details: String,
    pub status: TaskStatus,
    pub priority: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, Default)]
pub struct TaskChanges {
    pub title: Option<String>,
    pub details: Option<String>,
    pub status: Option<TaskStatus>,
    pub priority: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryCategory {
    Constraint,
    Decision,
    Fact,
    Preference,
}

impl MemoryCategory {
    fn as_str(self) -> &'static str {
        match self {
            Self::Constraint => "constraint",
            Self::Decision => "decision",
            Self::Fact => "fact",
            Self::Preference => "preference",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "constraint" => Some(Self::Constraint),
            "decision" => Some(Self::Decision),
            "fact" => Some(Self::Fact),
            "preference" => Some(Self::Preference),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionMemory {
    pub id: i64,
    pub session_id: String,
    pub category: MemoryCategory,
    pub title: String,
    pub content: String,
    pub active: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl Store {
    pub async fn list_tasks(
        &self,
        workspace: &str,
        include_closed: bool,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<ProjectTask>> {
        ensure!((1..=101).contains(&limit), "task limit must be 1–101");
        ensure!(offset <= 512, "task offset exceeds 512");
        let workspace = workspace.to_owned();
        self.access(move |connection| {
            let mut statement = connection.prepare(
                "SELECT id, workspace, parent_id, title, details, status, priority,
                        created_at_ms, updated_at_ms
                 FROM project_tasks
                 WHERE workspace=?1 AND (?2 OR status NOT IN ('completed','cancelled'))
                 ORDER BY priority DESC, updated_at_ms DESC, id DESC LIMIT ?3 OFFSET ?4",
            )?;
            Ok(statement
                .query_map(
                    params![workspace, include_closed, limit as i64, offset as i64],
                    task_row,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    pub async fn create_task(
        &self,
        workspace: &str,
        parent_id: Option<i64>,
        title: String,
        details: String,
        priority: i64,
    ) -> Result<ProjectTask> {
        validate_task(&title, &details, priority)?;
        let workspace = workspace.to_owned();
        self.access(move |connection| {
            let transaction = connection.transaction()?;
            if let Some(parent_id) = parent_id {
                let exists: bool = transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM project_tasks WHERE id=?1 AND workspace=?2)",
                    params![parent_id, workspace],
                    |row| row.get(0),
                )?;
                ensure!(exists, "parent task does not belong to this workspace");
            }
            let count: i64 = transaction.query_row(
                "SELECT count(*) FROM project_tasks WHERE workspace=?1",
                [&workspace],
                |row| row.get(0),
            )?;
            ensure!(count < 512, "workspace task limit reached");

            let now = super::store::now_millis();
            transaction.execute(
                "INSERT INTO project_tasks
                 (workspace, parent_id, title, details, priority, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                params![workspace, parent_id, title, details, priority, now],
            )?;
            let task = transaction.query_row(
                "SELECT id, workspace, parent_id, title, details, status, priority,
                        created_at_ms, updated_at_ms FROM project_tasks WHERE id=?1",
                [transaction.last_insert_rowid()],
                task_row,
            )?;
            transaction.commit()?;
            Ok(task)
        })
        .await
    }

    pub async fn update_task(
        &self,
        workspace: &str,
        id: i64,
        changes: TaskChanges,
    ) -> Result<ProjectTask> {
        ensure!(id > 0, "task id must be positive");
        ensure!(
            changes.title.is_some()
                || changes.details.is_some()
                || changes.status.is_some()
                || changes.priority.is_some(),
            "task update has no changes"
        );
        if let Some(title) = &changes.title {
            ensure!(
                !title.trim().is_empty() && title.len() <= 512,
                "invalid task title"
            );
        }
        if let Some(details) = &changes.details {
            ensure!(details.len() <= 8192, "task details exceed 8192 bytes");
        }
        if let Some(priority) = changes.priority {
            ensure!(
                (-100..=100).contains(&priority),
                "task priority must be -100–100"
            );
        }

        let workspace = workspace.to_owned();
        self.access(move |connection| {
            let changed = connection.execute(
                "UPDATE project_tasks SET title=COALESCE(?3,title), details=COALESCE(?4,details),
                        status=COALESCE(?5,status), priority=COALESCE(?6,priority), updated_at_ms=?7
                 WHERE id=?1 AND workspace=?2",
                params![
                    id,
                    workspace,
                    changes.title,
                    changes.details,
                    changes.status.map(TaskStatus::as_str),
                    changes.priority,
                    super::store::now_millis()
                ],
            )?;
            ensure!(changed == 1, "task not found in this workspace");
            Ok(connection.query_row(
                "SELECT id, workspace, parent_id, title, details, status, priority,
                        created_at_ms, updated_at_ms FROM project_tasks WHERE id=?1",
                [id],
                task_row,
            )?)
        })
        .await
    }

    pub async fn list_memories(
        &self,
        session_id: &str,
        include_inactive: bool,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<SessionMemory>> {
        ensure!((1..=256).contains(&limit), "memory limit must be 1–256");
        ensure!(offset <= 256, "memory offset exceeds 256");
        let session_id = session_id.to_owned();
        self.access(move |connection| {
            let mut statement = connection.prepare(
                "SELECT id, session_id, category, title, content, active, created_at_ms, updated_at_ms
                 FROM session_memories WHERE session_id=?1 AND (?2 OR active=1)
                 ORDER BY active DESC, updated_at_ms DESC, id DESC LIMIT ?3 OFFSET ?4",
            )?;
            Ok(statement
                .query_map(params![session_id, include_inactive, limit as i64, offset as i64], memory_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?)
        })
        .await
    }

    pub async fn search_memories(
        &self,
        session_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SessionMemory>> {
        let query = query.trim();
        ensure!(
            !query.is_empty() && query.len() <= 512,
            "memory query must be 1–512 bytes"
        );
        ensure!(
            (1..=20).contains(&limit),
            "memory search limit must be 1–20"
        );
        let phrase = query.to_lowercase();
        let terms = phrase
            .split(|character: char| !character.is_alphanumeric())
            .filter(|term| term.chars().count() >= 2)
            .collect::<Vec<_>>();
        let mut matches = self
            .list_memories(session_id, false, 0, 256)
            .await?
            .into_iter()
            .filter_map(|memory| {
                let title = memory.title.to_lowercase();
                let content = memory.content.to_lowercase();
                let mut score = u32::from(title.contains(&phrase)) * 100
                    + u32::from(content.contains(&phrase)) * 60;
                for term in &terms {
                    score += u32::from(title.contains(term)) * 10;
                    score += u32::from(content.contains(term)) * 5;
                }
                (score > 0).then_some((score, memory))
            })
            .collect::<Vec<_>>();
        matches.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| right.updated_at_ms.cmp(&left.updated_at_ms))
                .then_with(|| right.id.cmp(&left.id))
        });
        Ok(matches
            .into_iter()
            .take(limit)
            .map(|(_, memory)| memory)
            .collect())
    }

    pub async fn save_memory(
        &self,
        session_id: &str,
        category: MemoryCategory,
        title: String,
        content: String,
    ) -> Result<SessionMemory> {
        ensure!(
            !title.trim().is_empty() && title.len() <= 512,
            "invalid memory title"
        );
        ensure!(
            !content.trim().is_empty() && content.len() <= 8192,
            "invalid memory content"
        );
        let session_id = session_id.to_owned();
        self.access(move |connection| {
            let transaction = connection.transaction()?;
            let count: Option<i64> = transaction.query_row(
                "SELECT (SELECT count(*) FROM session_memories WHERE session_id=?1)
                 FROM sessions WHERE id=?1",
                [&session_id],
                |row| row.get(0),
            ).optional()?;
            let count = count.ok_or_else(|| anyhow::anyhow!("session not found"))?;
            ensure!(count < 256, "session memory limit reached");

            let now = super::store::now_millis();
            transaction.execute(
                "INSERT INTO session_memories
                 (session_id, category, title, content, created_at_ms, updated_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                params![session_id, category.as_str(), title, content, now],
            )?;
            let memory = transaction.query_row(
                "SELECT id, session_id, category, title, content, active, created_at_ms, updated_at_ms
                 FROM session_memories WHERE id=?1",
                [transaction.last_insert_rowid()],
                memory_row,
            )?;
            transaction.commit()?;
            Ok(memory)
        })
        .await
    }

    pub async fn forget_memory(&self, session_id: &str, id: i64) -> Result<SessionMemory> {
        ensure!(id > 0, "memory id must be positive");
        let session_id = session_id.to_owned();
        self.access(move |connection| {
            let changed = connection.execute(
                "UPDATE session_memories SET active=0, updated_at_ms=?3 WHERE id=?1 AND session_id=?2",
                params![id, session_id, super::store::now_millis()],
            )?;
            ensure!(changed == 1, "memory not found in this session");
            Ok(connection.query_row(
                "SELECT id, session_id, category, title, content, active, created_at_ms, updated_at_ms
                 FROM session_memories WHERE id=?1",
                [id],
                memory_row,
            )?)
        })
        .await
    }
}

fn validate_task(title: &str, details: &str, priority: i64) -> Result<()> {
    ensure!(
        !title.trim().is_empty() && title.len() <= 512,
        "invalid task title"
    );
    ensure!(details.len() <= 8192, "task details exceed 8192 bytes");
    ensure!(
        (-100..=100).contains(&priority),
        "task priority must be -100–100"
    );
    Ok(())
}

fn task_row(row: &Row<'_>) -> rusqlite::Result<ProjectTask> {
    let status: String = row.get(5)?;
    Ok(ProjectTask {
        id: row.get(0)?,
        workspace: row.get(1)?,
        parent_id: row.get(2)?,
        title: row.get(3)?,
        details: row.get(4)?,
        status: TaskStatus::parse(&status).ok_or(rusqlite::Error::InvalidQuery)?,
        priority: row.get(6)?,
        created_at_ms: row.get(7)?,
        updated_at_ms: row.get(8)?,
    })
}

fn memory_row(row: &Row<'_>) -> rusqlite::Result<SessionMemory> {
    let category: String = row.get(2)?;
    Ok(SessionMemory {
        id: row.get(0)?,
        session_id: row.get(1)?,
        category: MemoryCategory::parse(&category).ok_or(rusqlite::Error::InvalidQuery)?,
        title: row.get(3)?,
        content: row.get(4)?,
        active: row.get(5)?,
        created_at_ms: row.get(6)?,
        updated_at_ms: row.get(7)?,
    })
}
