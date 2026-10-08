use sea_orm_migration::prelude::*;

pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &str {
        "00000000000012_checkpoints"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("execution_checkpoints"))
                    .col(ColumnDef::new(Alias::new("run_id")).integer().primary_key())
                    .col(ColumnDef::new(Alias::new("payload")).text().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .from(Alias::new("execution_checkpoints"), Alias::new("run_id"))
                            .to(Alias::new("runs"), Alias::new("id"))
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
