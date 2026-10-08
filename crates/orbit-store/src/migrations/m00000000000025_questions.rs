use sea_orm_migration::prelude::*;

pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &str {
        "00000000000025_questions"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("user_inputs"))
                    .if_not_exists()
                    .col(ColumnDef::new(Alias::new("id")).integer().primary_key())
                    .col(ColumnDef::new(Alias::new("session_id")).text().not_null())
                    .col(ColumnDef::new(Alias::new("request_id")).text().not_null())
                    .col(ColumnDef::new(Alias::new("payload")).text().not_null())
                    .index(
                        Index::create()
                            .col(Alias::new("session_id"))
                            .col(Alias::new("request_id"))
                            .unique(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Alias::new("user_inputs"), Alias::new("session_id"))
                            .to(Alias::new("sessions"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("user_inputs_session")
                    .table(Alias::new("user_inputs"))
                    .if_not_exists()
                    .col(Alias::new("session_id"))
                    .col(Alias::new("id"))
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        super::forward_only()
    }
}
