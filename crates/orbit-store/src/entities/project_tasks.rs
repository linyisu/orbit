//! Persistence model; domain types remain independent of SeaORM.
use sea_orm::entity::prelude::*;
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "project_tasks")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub project_id: String,
    pub parent_id: Option<i64>,
    pub title: String,
    pub details: String,
    pub status: String,
    pub priority: i64,
    pub created_at: i64,
    pub updated_at: i64,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::project_tasks::Entity",
        from = "Column::ParentId",
        to = "super::project_tasks::Column::Id"
    )]
    Parent,
    #[sea_orm(
        belongs_to = "super::projects::Entity",
        from = "Column::ProjectId",
        to = "super::projects::Column::Id"
    )]
    Project,
}
impl ActiveModelBehavior for ActiveModel {}

impl Related<super::projects::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}
