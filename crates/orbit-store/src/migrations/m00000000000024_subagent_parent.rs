use sea_orm_migration::prelude::*;

pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &str {
        "00000000000024_subagent_parent"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("agent_sessions_v24"))
                    .col(
                        ColumnDef::new(Alias::new("session_id"))
                            .text()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("parent_session_id")).text())
                    .col(ColumnDef::new(Alias::new("role")).text().not_null().check(
                        Expr::col(Alias::new("role")).is_in(["explore", "review", "develop"]),
                    ))
                    .col(ColumnDef::new(Alias::new("task")).text().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .from(Alias::new("agent_sessions_v24"), Alias::new("session_id"))
                            .to(Alias::new("sessions"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(
                                Alias::new("agent_sessions_v24"),
                                Alias::new("parent_session_id"),
                            )
                            .to(Alias::new("sessions"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;
        super::copy(
            manager,
            "agent_sessions",
            "agent_sessions_v24",
            &["session_id", "parent_session_id", "role", "task"],
        )
        .await?;
        manager
            .drop_table(Table::drop().table(Alias::new("agent_sessions")).to_owned())
            .await?;
        manager
            .rename_table(
                Table::rename()
                    .table(
                        Alias::new("agent_sessions_v24"),
                        Alias::new("agent_sessions"),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("agent_sessions_parent")
                    .table(Alias::new("agent_sessions"))
                    .col(Alias::new("parent_session_id"))
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        super::forward_only()
    }
}
