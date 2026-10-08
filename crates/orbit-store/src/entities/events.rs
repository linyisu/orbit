//! Persistence model; domain types remain independent of SeaORM.
use sea_orm::entity::prelude::*;
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "events")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub run_id: i64,
    pub payload: String,
    pub created_at_ms: Option<i64>,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::runs::Entity",
        from = "Column::RunId",
        to = "super::runs::Column::Id"
    )]
    Run,
}
impl ActiveModelBehavior for ActiveModel {}

impl Related<super::runs::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Run.def()
    }
}
