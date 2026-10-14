use sea_orm_migration::prelude::*;

pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &str {
        "00000000000005_tasks_memories"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("project_tasks"))
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("project_id"))
                            .text()
                            .not_null(),
                    )
                    .col(ColumnDef::new(Alias::new("parent_id")).integer())
                    .col(
                        ColumnDef::new(Alias::new("title"))
                            .text()
                            .not_null()
                            .check(
                                Expr::expr(Func::char_length(Expr::col(Alias::new("title"))))
                                    .between(1, 512),
                            ),
                    )
                    .col(
                        ColumnDef::new(Alias::new("details"))
                            .text()
                            .not_null()
                            .default("")
                            .check(
                                Expr::expr(Func::char_length(Expr::col(Alias::new("details"))))
                                    .lte(8_192),
                            ),
                    )
                    .col(
                        ColumnDef::new(Alias::new("status"))
                            .text()
                            .not_null()
                            .default("pending")
                            .check(Expr::col(Alias::new("status")).is_in([
                                "pending",
                                "in_progress",
                                "blocked",
                                "completed",
                                "cancelled",
                            ])),
                    )
                    .col(
                        ColumnDef::new(Alias::new("priority"))
                            .integer()
                            .not_null()
                            .default(0)
                            .check(Expr::col(Alias::new("priority")).between(-100, 100)),
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
                            .from(Alias::new("project_tasks"), Alias::new("project_id"))
                            .to(Alias::new("projects"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Alias::new("project_tasks"), Alias::new("parent_id"))
                            .to(Alias::new("project_tasks"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("project_tasks_project_status")
                    .table(Alias::new("project_tasks"))
                    .col(Alias::new("project_id"))
                    .col(Alias::new("status"))
                    .col((Alias::new("priority"), IndexOrder::Desc))
                    .col(Alias::new("id"))
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("project_memories"))
                    .col(
                        ColumnDef::new(Alias::new("id"))
                            .integer()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("project_id"))
                            .text()
                            .not_null(),
                    )
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
                        ColumnDef::new(Alias::new("title"))
                            .text()
                            .not_null()
                            .check(
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
                                    .between(1, 8_192),
                            ),
                    )
                    .col(ColumnDef::new(Alias::new("source_session_id")).text())
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
                            .from(Alias::new("project_memories"), Alias::new("project_id"))
                            .to(Alias::new("projects"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(
                                Alias::new("project_memories"),
                                Alias::new("source_session_id"),
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
                    .name("project_memories_project_active")
                    .table(Alias::new("project_memories"))
                    .col(Alias::new("project_id"))
                    .col(Alias::new("active"))
                    .col((Alias::new("updated_at"), IndexOrder::Desc))
                    .col((Alias::new("id"), IndexOrder::Desc))
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        super::forward_only()
    }
}
