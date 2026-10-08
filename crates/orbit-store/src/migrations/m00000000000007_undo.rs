use sea_orm_migration::prelude::*;

pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &str {
        "00000000000007_undo"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("workspace_undos"))
                    .col(ColumnDef::new(Alias::new("session_id")).text().not_null())
                    .col(ColumnDef::new(Alias::new("run_id")).integer().not_null())
                    .col(ColumnDef::new(Alias::new("call_id")).text().not_null())
                    .col(ColumnDef::new(Alias::new("path")).text().not_null())
                    .col(
                        ColumnDef::new(Alias::new("undone_at"))
                            .integer()
                            .not_null()
                            .default(super::timestamp_default()),
                    )
                    .primary_key(
                        Index::create()
                            .col(Alias::new("session_id"))
                            .col(Alias::new("run_id"))
                            .col(Alias::new("call_id"))
                            .col(Alias::new("path")),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Alias::new("workspace_undos"), Alias::new("session_id"))
                            .to(Alias::new("sessions"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Alias::new("workspace_undos"), Alias::new("run_id"))
                            .to(Alias::new("runs"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("workspace_undos_session")
                    .table(Alias::new("workspace_undos"))
                    .col(Alias::new("session_id"))
                    .col(Alias::new("run_id"))
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        super::forward_only()
    }
}
