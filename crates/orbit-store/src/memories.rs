//! Session-scoped memory repository and local lexical ranking.
use crate::{
    database::now,
    entities::{session_memories as memories, sessions},
    store::Store,
    types::{MemoryCategory, SessionMemory},
};
use anyhow::{Context, Result, ensure};
use sea_orm::*;
impl Store {
    pub async fn session_memories(
        &self,
        session_id: &str,
        include_inactive: bool,
    ) -> Result<Vec<SessionMemory>> {
        self.read_access(|conn| async move {
            let mut query =
                memories::Entity::find().filter(memories::Column::SessionId.eq(session_id));
            if !include_inactive {
                query = query.filter(memories::Column::Active.eq(true));
            }
            query
                .order_by_desc(memories::Column::Active)
                .order_by_desc(memories::Column::UpdatedAt)
                .order_by_desc(memories::Column::Id)
                .all(&conn)
                .await?
                .into_iter()
                .map(memory)
                .collect()
        })
        .await
    }
    /// Local lexical search: exact phrases, then terms, with newest records breaking ties.
    pub async fn search_session_memories(
        &self,
        session_id: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<SessionMemory>> {
        let query = query.trim();
        ensure!(!query.is_empty(), "memory search query must not be empty");
        ensure!(query.len() <= 512, "memory search query exceeds 512 bytes");
        ensure!(
            (1..=20).contains(&limit),
            "memory search limit must be between 1 and 20"
        );

        let phrase = query.to_lowercase();
        let terms = phrase
            .split(|character: char| !character.is_alphanumeric())
            .filter(|term| term.chars().count() >= 2)
            .collect::<Vec<_>>();
        let mut matches = self
            .session_memories(session_id, false)
            .await?
            .into_iter()
            .filter_map(|memory| {
                let title = memory.title.to_lowercase();
                let content = memory.content.to_lowercase();
                let mut score = 0_u32;

                if title.contains(&phrase) {
                    score += 100;
                }

                if content.contains(&phrase) {
                    score += 60;
                }

                for term in &terms {
                    if title.contains(term) {
                        score += 10;
                    }

                    if content.contains(term) {
                        score += 5;
                    }
                }

                (score > 0).then_some((score, memory))
            })
            .collect::<Vec<_>>();

        matches.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| right.updated_at.cmp(&left.updated_at))
                .then_with(|| right.id.cmp(&left.id))
        });

        Ok(matches
            .into_iter()
            .take(limit)
            .map(|(_, memory)| memory)
            .collect())
    }

    pub async fn remember_in_session(
        &self,
        session_id: &str,
        category: MemoryCategory,
        title: String,
        content: String,
    ) -> Result<SessionMemory> {
        validate_memory_fields(&title, &content)?;
        self.write_access(|conn| async move {
            let tx = conn.begin().await?;
            ensure!(
                sessions::Entity::find_by_id(session_id).count(&tx).await? > 0,
                "session not found"
            );
            let row = memories::ActiveModel {
                session_id: Set(session_id.into()),
                category: Set(category.as_str().into()),
                title: Set(title),
                content: Set(content),
                ..Default::default()
            }
            .insert(&tx)
            .await?;
            let result = memory(row)?;
            tx.commit().await?;
            Ok(result)
        })
        .await
    }
    pub async fn set_session_memory_active(
        &self,
        session_id: &str,
        id: i64,
        active: bool,
    ) -> Result<SessionMemory> {
        self.write_access(|conn| async move {
            let tx = conn.begin().await?;
            let mut row = memories::Entity::find_by_id(id)
                .filter(memories::Column::SessionId.eq(session_id))
                .one(&tx)
                .await?
                .context("memory not found in this session")?
                .into_active_model();
            row.active = Set(active);
            row.updated_at = Set(now());
            let result = memory(row.update(&tx).await?)?;
            tx.commit().await?;
            Ok(result)
        })
        .await
    }
}
fn memory(row: memories::Model) -> Result<SessionMemory> {
    Ok(SessionMemory {
        id: row.id,
        session_id: row.session_id,
        category: MemoryCategory::parse(&row.category).context("invalid stored memory category")?,
        title: row.title,
        content: row.content,
        active: row.active,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}
fn validate_memory_fields(title: &str, content: &str) -> Result<()> {
    ensure!(!title.trim().is_empty(), "memory title must not be empty");
    ensure!(title.len() <= 512, "memory title exceeds 512 bytes");
    ensure!(
        !content.trim().is_empty(),
        "memory content must not be empty"
    );
    ensure!(content.len() <= 8192, "memory content exceeds 8192 bytes");

    Ok(())
}
