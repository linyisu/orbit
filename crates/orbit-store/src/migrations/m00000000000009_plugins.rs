use sea_orm_migration::prelude::*;

pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &str {
        "00000000000009_plugins"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("project_plugins"))
                    .col(
                        ColumnDef::new(Alias::new("project_id"))
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Alias::new("marketplace_source"))
                            .text()
                            .not_null()
                            .check(
                                Expr::expr(Func::char_length(Expr::col(Alias::new(
                                    "marketplace_source",
                                ))))
                                .between(1, 4_096),
                            ),
                    )
                    .col(
                        ColumnDef::new(Alias::new("plugin_name"))
                            .text()
                            .not_null()
                            .check(
                                Expr::expr(Func::char_length(Expr::col(Alias::new("plugin_name"))))
                                    .between(1, 256),
                            ),
                    )
                    .col(
                        ColumnDef::new(Alias::new("package_source"))
                            .text()
                            .not_null()
                            .check(
                                Expr::expr(Func::char_length(Expr::col(Alias::new(
                                    "package_source",
                                ))))
                                .between(1, 4_096),
                            ),
                    )
                    .col(
                        ColumnDef::new(Alias::new("version"))
                            .text()
                            .check(
                                Expr::col(Alias::new("version"))
                                    .is_null()
                                    .or(Expr::expr(Func::char_length(Expr::col(Alias::new(
                                        "version",
                                    ))))
                                    .between(1, 256)),
                            ),
                    )
                    .col(
                        ColumnDef::new(Alias::new("enabled_at"))
                            .integer()
                            .not_null()
                            .default(super::timestamp_default()),
                    )
                    .primary_key(
                        Index::create()
                            .col(Alias::new("project_id"))
                            .col(Alias::new("marketplace_source"))
                            .col(Alias::new("plugin_name")),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Alias::new("project_plugins"), Alias::new("project_id"))
                            .to(Alias::new("projects"), Alias::new("id"))
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("project_plugins_project_enabled")
                    .table(Alias::new("project_plugins"))
                    .col(Alias::new("project_id"))
                    .col((Alias::new("enabled_at"), IndexOrder::Desc))
                    .col(Alias::new("plugin_name"))
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        super::forward_only()
    }
}
