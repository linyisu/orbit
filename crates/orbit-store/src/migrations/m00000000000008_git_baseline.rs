use sea_orm_migration::prelude::*;

pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &str {
        "00000000000008_git_baseline"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("session_git_baselines"))
                    .col(
                        ColumnDef::new(Alias::new("session_id"))
                            .text()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("payload")).text().not_null())
                    .col(
                        ColumnDef::new(Alias::new("captured_at"))
                            .integer()
                            .not_null()
                            .default(super::timestamp_default()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(
                                Alias::new("session_git_baselines"),
                                Alias::new("session_id"),
                            )
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
