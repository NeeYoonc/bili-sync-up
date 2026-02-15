use sea_orm::Statement;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if submission_has_dynamic_api_full_synced(manager).await? {
            return Ok(());
        }

        manager
            .alter_table(
                Table::alter()
                    .table(Submission::Table)
                    .add_column(
                        ColumnDef::new(Submission::DynamicApiFullSynced)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await?;

        let backend = manager.get_connection().get_database_backend();
        manager
            .get_connection()
            .execute(Statement::from_string(
                backend,
                "UPDATE submission SET dynamic_api_full_synced = 1 WHERE use_dynamic_api = 1".to_string(),
            ))
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !submission_has_dynamic_api_full_synced(manager).await? {
            return Ok(());
        }

        manager
            .alter_table(
                Table::alter()
                    .table(Submission::Table)
                    .drop_column(Submission::DynamicApiFullSynced)
                    .to_owned(),
            )
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Submission {
    Table,
    DynamicApiFullSynced,
}

async fn submission_has_dynamic_api_full_synced(manager: &SchemaManager<'_>) -> Result<bool, DbErr> {
    let backend = manager.get_connection().get_database_backend();
    let sql = "SELECT COUNT(*) FROM pragma_table_info('submission') WHERE name = 'dynamic_api_full_synced'";
    let result = manager
        .get_connection()
        .query_one(Statement::from_string(backend, sql.to_string()))
        .await?;
    Ok(result.and_then(|row| row.try_get_by_index(0).ok()).unwrap_or(0) >= 1)
}
