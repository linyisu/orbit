use sea_orm_migration::prelude::*;

pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &str {
        "00000000000015_compaction"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager
            .has_column("context_archives", "content_sha256")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("context_archives"))
                        .add_column(ColumnDef::new(Alias::new("content_sha256")).text())
                        .to_owned(),
                )
                .await?;
        }
        manager
            .create_index(
                Index::create()
                    .name("context_archives_content")
                    .table(Alias::new("context_archives"))
                    .unique()
                    .if_not_exists()
                    .col(Alias::new("session_id"))
                    .col(Alias::new("content_sha256"))
                    .and_where(Expr::col(Alias::new("content_sha256")).is_not_null())
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("compaction_progress"))
                    .if_not_exists()
                    .col(ColumnDef::new(Alias::new("session_id")).text().not_null())
                    .col(
                        ColumnDef::new(Alias::new("source_fingerprint"))
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("payload")).text().not_null())
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .integer()
                            .not_null()
                            .default(super::timestamp_default()),
                    )
                    .primary_key(
                        Index::create()
                            .col(Alias::new("session_id"))
                            .col(Alias::new("source_fingerprint")),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Alias::new("compaction_progress"), Alias::new("session_id"))
                            .to(Alias::new("sessions"), Alias::new("id"))
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
