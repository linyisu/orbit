use sea_orm_migration::prelude::*;

pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &str {
        "00000000000016_plugin_revision"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager
            .has_column("project_plugins", "marketplace_revision")
            .await?
        {
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("project_plugins"))
                        .add_column(ColumnDef::new(Alias::new("marketplace_revision")).text())
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        super::forward_only()
    }
}
