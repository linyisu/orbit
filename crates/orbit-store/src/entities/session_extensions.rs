//! Immutable extension packages owned by a persisted session.
use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "session_extensions")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub session_id: String,
    #[sea_orm(primary_key, auto_increment = false)]
    pub key: String,
    pub bundle: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
