use sea_orm_migration::prelude::*;

pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &str {
        "00000000000029_task_collaboration"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("task_claims"))
                    .col(
                        ColumnDef::new(Alias::new("task_id"))
                            .integer()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Alias::new("session_id")).text().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .from(Alias::new("task_claims"), Alias::new("task_id"))
                            .to(Alias::new("project_tasks"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Alias::new("task_claims"), Alias::new("session_id"))
                            .to(Alias::new("sessions"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("task_claims_session")
                    .table(Alias::new("task_claims"))
                    .col(Alias::new("session_id"))
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("task_dependencies"))
                    .col(ColumnDef::new(Alias::new("task_id")).integer().not_null())
                    .col(
                        ColumnDef::new(Alias::new("depends_on_id"))
                            .integer()
                            .not_null(),
                    )
                    .primary_key(
                        Index::create()
                            .col(Alias::new("task_id"))
                            .col(Alias::new("depends_on_id")),
                    )
                    .check(
                        Expr::col(Alias::new("task_id")).ne(Expr::col(Alias::new("depends_on_id"))),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Alias::new("task_dependencies"), Alias::new("task_id"))
                            .to(Alias::new("project_tasks"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Alias::new("task_dependencies"), Alias::new("depends_on_id"))
                            .to(Alias::new("project_tasks"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("task_dependencies_target")
                    .table(Alias::new("task_dependencies"))
                    .col(Alias::new("depends_on_id"))
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        super::forward_only()
    }
}
