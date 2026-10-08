//! Persistence model; domain types remain independent of SeaORM.
use sea_orm::entity::prelude::*;
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "tui_session_state")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub session_id: String,
    pub permission_mode: String,
    pub run_id: Option<i64>,
    pub runtime: Option<String>,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::runs::Entity",
        from = "Column::RunId",
        to = "super::runs::Column::Id"
    )]
    Run,
    #[sea_orm(
        belongs_to = "super::sessions::Entity",
        from = "Column::SessionId",
        to = "super::sessions::Column::Id"
    )]
    Session,
}
impl ActiveModelBehavior for ActiveModel {}

impl Related<super::runs::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Run.def()
    }
}
impl Related<super::sessions::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Session.def()
    }
}
