use sea_orm::Statement;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if submission_has_use_dynamic_api(manager).await? {
            return Ok(());
        }

        manager
            .alter_table(
                Table::alter()
                    .table(Submission::Table)
                    .add_column(
                        ColumnDef::new(Submission::UseDynamicApi)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !submission_has_use_dynamic_api(manager).await? {
            return Ok(());
        }

        manager
            .alter_table(
                Table::alter()
                    .table(Submission::Table)
                    .drop_column(Submission::UseDynamicApi)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Submission {
    Table,
    UseDynamicApi,
}

async fn submission_has_use_dynamic_api(manager: &SchemaManager<'_>) -> Result<bool, DbErr> {
    let backend = manager.get_connection().get_database_backend();
    let sql = "SELECT COUNT(*) FROM pragma_table_info('submission') WHERE name = 'use_dynamic_api'";
    let result = manager
        .get_connection()
        .query_one(Statement::from_string(backend, sql.to_string()))
        .await?;
    Ok(result.and_then(|row| row.try_get_by_index(0).ok()).unwrap_or(0) >= 1)
}
