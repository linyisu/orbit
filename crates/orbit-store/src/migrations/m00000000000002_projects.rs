use sea_orm_migration::prelude::*;

pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &str {
        "00000000000002_projects"
    }
}
#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("projects"))
                    .col(ColumnDef::new(Alias::new("id")).text().primary_key())
                    .col(ColumnDef::new(Alias::new("path")).text())
                    .col(ColumnDef::new(Alias::new("identity")).text())
                    .col(
                        ColumnDef::new(Alias::new("strong"))
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(Alias::new("legacy"))
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .name("projects_identity")
                    .table(Alias::new("projects"))
                    .col(Alias::new("identity"))
                    .to_owned(),
            )
            .await?;
        manager
            .create_table(
                Table::create()
                    .table(Alias::new("project_paths"))
                    .col(ColumnDef::new(Alias::new("project_id")).text().not_null())
                    .col(ColumnDef::new(Alias::new("path")).text().not_null())
                    .primary_key(
                        Index::create()
                            .col(Alias::new("project_id"))
                            .col(Alias::new("path")),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(Alias::new("project_paths"), Alias::new("project_id"))
                            .to(Alias::new("projects"), Alias::new("id")),
                    )
                    .to_owned(),
            )
            .await?;
        super::backfill::projects(manager).await?;
        manager
            .create_index(
                Index::create()
                    .name("sessions_project")
                    .table(Alias::new("sessions"))
                    .col(Alias::new("project_id"))
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        super::forward_only()
    }
}
