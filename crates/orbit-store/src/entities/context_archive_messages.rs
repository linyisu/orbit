//! Persistence model; domain types remain independent of SeaORM.
use sea_orm::entity::prelude::*;
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "context_archive_messages")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub archive_id: i64,
    #[sea_orm(primary_key, auto_increment = false)]
    pub ordinal: i64,
    pub payload: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::context_archives::Entity",
        from = "Column::ArchiveId",
        to = "super::context_archives::Column::Id"
    )]
    Archive,
}

impl ActiveModelBehavior for ActiveModel {}

impl Related<super::context_archives::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Archive.def()
    }
}
