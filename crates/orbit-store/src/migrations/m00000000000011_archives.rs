use sea_orm_migration::prelude::*;

pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &str {
        "00000000000011_archives"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("context_archives"))
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
                        ColumnDef::new(Alias::new("created_at"))
                            .integer()
                            .not_null()
                            .default(super::timestamp_default()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Alias::new("context_archives"), Alias::new("session_id"))
                            .to(Alias::new("sessions"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("context_archives_session")
                    .table(Alias::new("context_archives"))
                    .col(Alias::new("session_id"))
                    .col(Alias::new("id"))
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("context_archive_messages"))
                    .col(
                        ColumnDef::new(Alias::new("archive_id"))
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
                    .primary_key(
                        Index::create()
                            .col(Alias::new("archive_id"))
                            .col(Alias::new("ordinal")),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(
                                Alias::new("context_archive_messages"),
                                Alias::new("archive_id"),
                            )
                            .to(Alias::new("context_archives"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        super::forward_only()
    }
}
