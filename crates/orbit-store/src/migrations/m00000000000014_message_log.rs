use sea_orm_migration::prelude::*;

pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &str {
        "00000000000014_message_log"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("session_messages"))
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("session_id"))
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("run_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("ordinal"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("payload"))
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("sha256"))
                            .text()
                            .not_null(),
                    )
                    .index(
                        Index::create()
                            .col(Alias::new("run_id"))
                            .col(Alias::new("ordinal"))
                            .unique(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Alias::new("session_messages"), Alias::new("session_id"))
                            .to(Alias::new("sessions"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Alias::new("session_messages"), Alias::new("run_id"))
                            .to(Alias::new("runs"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("session_messages_session")
                    .table(Alias::new("session_messages"))
                    .col(Alias::new("session_id"))
                    .col(Alias::new("id"))
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("session_log_gaps"))
                    .col(
                        ColumnDef::new(Alias::new("session_id"))
                            .text()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("reason"))
                            .text()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Alias::new("session_log_gaps"), Alias::new("session_id"))
                            .to(Alias::new("sessions"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        super::backfill::message_gaps(manager).await?;
        Ok(())
    }

    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        super::forward_only()
    }
}
