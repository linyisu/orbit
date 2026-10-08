use sea_orm_migration::prelude::*;

pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &str {
        "00000000000010_session_memory"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("session_memories"))
                    .col(ColumnDef::new(Alias::new("id")).integer().primary_key())
                    .col(ColumnDef::new(Alias::new("session_id")).text().not_null())
                    .col(
                        ColumnDef::new(Alias::new("category"))
                            .text()
                            .not_null()
                            .check(Expr::col(Alias::new("category")).is_in([
                                "constraint",
                                "decision",
                                "fact",
                                "preference",
                            ])),
                    )
                    .col(
                        ColumnDef::new(Alias::new("title")).text().not_null().check(
                            Expr::expr(Func::char_length(Expr::col(Alias::new("title"))))
                                .between(1, 512),
                        ),
                    )
                    .col(
                        ColumnDef::new(Alias::new("content"))
                            .text()
                            .not_null()
                            .check(
                                Expr::expr(Func::char_length(Expr::col(Alias::new("content"))))
                                    .between(1, 8192),
                            ),
                    )
                    .col(
                        ColumnDef::new(Alias::new("active"))
                            .integer()
                            .not_null()
                            .default(1)
                            .check(Expr::col(Alias::new("active")).is_in([0, 1])),
                    )
                    .col(
                        ColumnDef::new(Alias::new("created_at"))
                            .integer()
                            .not_null()
                            .default(super::timestamp_default()),
                    )
                    .col(
                        ColumnDef::new(Alias::new("updated_at"))
                            .integer()
                            .not_null()
                            .default(super::timestamp_default()),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Alias::new("session_memories"), Alias::new("session_id"))
                            .to(Alias::new("sessions"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("session_memories_session_active")
                    .table(Alias::new("session_memories"))
                    .col(Alias::new("session_id"))
                    .col(Alias::new("active"))
                    .col((Alias::new("updated_at"), IndexOrder::Desc))
                    .col((Alias::new("id"), IndexOrder::Desc))
                    .to_owned(),
            )
            .await?;
        super::backfill::session_memories(manager).await?;
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        super::forward_only()
    }
}
