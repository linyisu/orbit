use sea_orm_migration::prelude::*;

pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &str {
        "00000000000021_subagents"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("agent_sessions"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("session_id"))
                            .text()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("parent_session_id")).text())
                    .col(
                        ColumnDef::new(Alias::new("role"))
                            .text()
                            .not_null()
                            .check(Expr::col(Alias::new("role")).is_in(["explore", "review"])),
                    )
                    .col(
                        ColumnDef::new(Alias::new("task"))
                            .text()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Alias::new("agent_sessions"), Alias::new("session_id"))
                            .to(Alias::new("sessions"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(
                                Alias::new("agent_sessions"),
                                Alias::new("parent_session_id"),
                            )
                            .to(Alias::new("sessions"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("agent_sessions_parent")
                    .table(Alias::new("agent_sessions"))
                    .if_not_exists()
                    .col(Alias::new("parent_session_id"))
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("agent_invocations"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .primary_key()
                            .auto_increment(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("parent_session_id"))
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("parent_run_id"))
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("child_session_id"))
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("call"))
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("result")).text())
                    .col(
                        ColumnDef::new(Alias::new("delivered"))
                            .integer()
                            .not_null()
                            .default(0)
                            .check(Expr::col(Alias::new("delivered")).is_in([0, 1])),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(
                                Alias::new("agent_invocations"),
                                Alias::new("parent_session_id"),
                            )
                            .to(Alias::new("sessions"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Alias::new("agent_invocations"), Alias::new("parent_run_id"))
                            .to(Alias::new("runs"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(
                                Alias::new("agent_invocations"),
                                Alias::new("child_session_id"),
                            )
                            .to(Alias::new("agent_sessions"), Alias::new("session_id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("agent_invocations_delivery")
                    .table(Alias::new("agent_invocations"))
                    .if_not_exists()
                    .col(Alias::new("parent_session_id"))
                    .col(Alias::new("delivered"))
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("agent_messages"))
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .primary_key()
                            .auto_increment(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("session_id"))
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("content"))
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("received"))
                            .integer()
                            .not_null()
                            .default(0)
                            .check(Expr::col(Alias::new("received")).is_in([0, 1])),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Alias::new("agent_messages"), Alias::new("session_id"))
                            .to(Alias::new("agent_sessions"), Alias::new("session_id"))
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
