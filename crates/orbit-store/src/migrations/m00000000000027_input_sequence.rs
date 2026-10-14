use sea_orm_migration::prelude::*;

pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &str {
        "00000000000027_input_sequence"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("session_inputs_v27"))
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
                            .from(Alias::new("session_inputs_v27"), Alias::new("session_id"))
                            .to(Alias::new("sessions"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        super::copy(
            manager,
            "agent_messages",
            "session_inputs_v27",
            &["id", "session_id", "content", "received"],
        )
        .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(Alias::new("agent_messages"))
                    .to_owned(),
            )
            .await?;
        manager
            .rename_table(
                Table::rename()
                    .table(
                        Alias::new("session_inputs_v27"),
                        Alias::new("agent_messages"),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("agent_messages_pending")
                    .table(Alias::new("agent_messages"))
                    .col(Alias::new("session_id"))
                    .col(Alias::new("received"))
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        super::forward_only()
    }
}
