//! Persistence model; domain types remain independent of SeaORM.
use sea_orm::entity::prelude::*;
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "agent_invocations")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub parent_session_id: String,
    pub parent_run_id: i64,
    pub child_session_id: String,
    pub call: String,
    pub result: Option<String>,
    pub delivered: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::agent_sessions::Entity",
        from = "Column::ChildSessionId",
        to = "super::agent_sessions::Column::SessionId"
    )]
    ChildSession,
    #[sea_orm(
        belongs_to = "super::runs::Entity",
        from = "Column::ParentRunId",
        to = "super::runs::Column::Id"
    )]
    ParentRun,
    #[sea_orm(
        belongs_to = "super::sessions::Entity",
        from = "Column::ParentSessionId",
        to = "super::sessions::Column::Id"
    )]
    ParentSession,
}

impl ActiveModelBehavior for ActiveModel {}

impl Related<super::agent_sessions::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ChildSession.def()
    }
}

impl Related<super::runs::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ParentRun.def()
    }
}

impl Related<super::sessions::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::ParentSession.def()
    }
}
