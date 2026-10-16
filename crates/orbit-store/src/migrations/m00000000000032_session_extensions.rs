use sea_orm_migration::prelude::*;

pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &str {
        "00000000000032_session_extensions"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("session_extensions"))
                    .col(
                        ColumnDef::new(Alias::new("session_id"))
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("key"))
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("bundle"))
                            .text()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(Alias::new("session_id"))
                            .col(Alias::new("key")),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Alias::new("session_extensions"), Alias::new("session_id"))
                            .to(Alias::new("sessions"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        super::forward_only()
    }
}
