use sea_orm_migration::prelude::*;

pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &str {
        "00000000000019_runtime"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("tui_session_state"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("session_id"))
                            .text()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("permission_mode"))
                            .text()
                            .not_null()
                            .default("auto"),
                    )
                    .col(ColumnDef::new(Alias::new("run_id")).integer())
                    .col(ColumnDef::new(Alias::new("runtime")).text())
                    .foreign_key(
                        ForeignKey::create()
                            .from(Alias::new("tui_session_state"), Alias::new("session_id"))
                            .to(Alias::new("sessions"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Alias::new("tui_session_state"), Alias::new("run_id"))
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
