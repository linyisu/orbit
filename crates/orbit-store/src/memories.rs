//! Session-scoped memory repository and local lexical ranking.
use crate::{
    database::now,
    entities::{session_memories as memories, sessions},
    store::Store,
    types::{MemoryCategory, SessionMemory},
};
use anyhow::{Context, Result, ensure};
use sea_orm::sea_query::Expr;
use sea_orm::*;

const MAX_SEARCH_CANDIDATES: usize = 512;

pub struct MemorySearchPage {
    pub memories: Vec<SessionMemory>,
    pub search_truncated: bool,
}

impl Store {
    /// Read a live ID-ordered page confined to one conversation.
    pub async fn session_memory_page(
        &self,
        session_id: &str,
        include_inactive: bool,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<SessionMemory>> {
        ensure!(
            (1..=101).contains(&limit) && after_id >= 0,
            "invalid memory page"
        );
        self.read_access(|conn| async move {
            let mut query = memories::Entity::find()
                .filter(memories::Column::SessionId.eq(session_id))
                .filter(memories::Column::Id.gt(after_id));
            if !include_inactive {
                query = query.filter(memories::Column::Active.eq(true));
            }
            query
                .order_by_asc(memories::Column::Id)
                .limit(limit as u64)
                .all(&conn)
                .await?
                .into_iter()
                .map(memory)
                .collect()
        })
        .await
    }

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
        Ok(self
            .search_session_memories_filtered(session_id, query, limit, None)
            .await?
            .memories)
    }

    /// Rank bounded database candidates, optionally confined to one category.
    /// Chinese bigrams improve lexical recall; truncation means older matches were not ranked.
    pub async fn search_session_memories_filtered(
        &self,
        session_id: &str,
        query: &str,
        limit: usize,
        category: Option<MemoryCategory>,
    ) -> Result<MemorySearchPage> {
        let query = query.trim();
        ensure!(!query.is_empty(), "memory search query must not be empty");
        ensure!(query.len() <= 512, "memory search query exceeds 512 bytes");
        ensure!(
            (1..=20).contains(&limit),
            "memory search limit must be between 1 and 20"
        );

        let phrase = query.to_lowercase();
        let mut terms = phrase
            .split(|character: char| !character.is_alphanumeric())
            .filter(|term| term.chars().count() >= 2)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let characters = phrase.chars().collect::<Vec<_>>();
        terms.extend(
            characters
                .windows(2)
                .filter(|pair| {
                    pair.iter()
                        .all(|c| matches!(*c, '\u{3400}'..='\u{9fff}'))
                })
                .map(|pair| pair.iter().collect::<String>()),
        );
        terms.sort_unstable();
        terms.dedup();
        let mut condition = Condition::any();
        for term in std::iter::once(&phrase).chain(&terms) {
            let pattern = format!(
                "%{}%",
                term.replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            );
            for column in [memories::Column::Title, memories::Column::Content] {
                condition = condition.add(
                    Expr::expr(sea_orm::sea_query::Func::lower(Expr::col(column)))
                        .like(sea_orm::sea_query::LikeExpr::new(pattern.clone()).escape('\\')),
                );
            }
        }
        // SQLite's built-in case folding is ASCII-only. Rank other scripts in Rust
        // rather than excluding valid case-insensitive matches in the SQL prefilter.
        let prefilter = phrase
            .chars()
            .all(|c| c.is_ascii() || matches!(c, '\u{3400}'..='\u{9fff}'));
        let candidates = self
            .read_access(|conn| async move {
                let mut query = memories::Entity::find()
                    .filter(memories::Column::SessionId.eq(session_id))
                    .filter(memories::Column::Active.eq(true));
                if prefilter {
                    query = query.filter(condition);
                }
                if let Some(category) = category {
                    query = query.filter(memories::Column::Category.eq(category.as_str()));
                }
                Ok(query
                    .order_by_desc(memories::Column::UpdatedAt)
                    .order_by_desc(memories::Column::Id)
                    .limit((MAX_SEARCH_CANDIDATES + 1) as u64)
                    .all(&conn)
                    .await?)
            })
            .await?;
        let search_truncated = candidates.len() > MAX_SEARCH_CANDIDATES;
        let mut matches = candidates
            .into_iter()
            .take(MAX_SEARCH_CANDIDATES)
            .map(memory)
            .collect::<Result<Vec<_>>>()?
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
                    if title.contains(term.as_str()) {
                        score += 10;
                    }

                    if content.contains(term.as_str()) {
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

        Ok(MemorySearchPage {
            memories: matches
                .into_iter()
                .take(limit)
                .map(|(_, memory)| memory)
                .collect(),
            search_truncated,
        })
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
                sessions::Entity::find_by_id(session_id)
                    .count(&tx)
                    .await?
                    > 0,
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
    ensure!(content.len() <= 8_192, "memory content exceeds 8192 bytes");

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn filtered_search_handles_chinese_categories_literals_and_candidate_budgets() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("memory.db"))
            .await
            .unwrap();
        let writer = store
            .create(dir.path().display().to_string(), "fixture".into())
            .await
            .unwrap();
        let wanted = store
            .remember_in_session(
                writer.id(),
                MemoryCategory::Constraint,
                "缓存策略".into(),
                "策略必须可以配置。".into(),
            )
            .await
            .unwrap();
        store
            .remember_in_session(
                writer.id(),
                MemoryCategory::Fact,
                "策略约束".into(),
                "这是另一种分类。".into(),
            )
            .await
            .unwrap();
        let page = store
            .search_session_memories_filtered(
                writer.id(),
                "策略约束",
                8,
                Some(MemoryCategory::Constraint),
            )
            .await
            .unwrap();
        assert_eq!(page.memories.len(), 1);
        assert_eq!(page.memories[0].id, wanted.id);
        assert!(!page.search_truncated);
        store
            .remember_in_session(
                writer.id(),
                MemoryCategory::Fact,
                "Literal % character".into(),
                "Use a literal symbol.".into(),
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .search_session_memories(writer.id(), "%", 8)
                .await
                .unwrap()
                .len(),
            1
        );
        let unicode = store
            .remember_in_session(
                writer.id(),
                MemoryCategory::Preference,
                "ÉTUDES".into(),
                "Mixed-case lookup.".into(),
            )
            .await
            .unwrap();
        assert_eq!(
            store
                .search_session_memories(writer.id(), "études", 8)
                .await
                .unwrap()[0]
                .id,
            unicode.id
        );
        let session = writer.id().to_owned();
        store
            .write_access(|conn| async move {
                memories::Entity::insert_many((0..MAX_SEARCH_CANDIDATES + 1).map(|index| {
                    memories::ActiveModel {
                        session_id: Set(session.clone()),
                        category: Set("fact".into()),
                        title: Set(format!("candidate {index}")),
                        content: Set("ranked fixture".into()),
                        active: Set(true),
                        created_at: Set(now()),
                        updated_at: Set(now()),
                        ..Default::default()
                    }
                }))
                .exec(&conn)
                .await?;
                Ok(())
            })
            .await
            .unwrap();
        let page = store
            .search_session_memories_filtered(writer.id(), "candidate", 5, None)
            .await
            .unwrap();
        assert_eq!(page.memories.len(), 5);
        assert!(page.search_truncated);
    }
}
