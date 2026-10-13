use sea_orm_migration::prelude::*;
pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &str {
        "00000000000031_queued_images"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("agent_messages"))
                    .add_column(
                        ColumnDef::new(Alias::new("attachments"))
                            .text()
                            .not_null()
                            .default("[]"),
                    )
                    .to_owned(),
            )
            .await
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        super::forward_only()
    }
}
