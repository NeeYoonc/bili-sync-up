use sea_orm::Statement;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !submission_has_column(manager, "last_scan_at").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Submission::Table)
                        .add_column(ColumnDef::new(Submission::LastScanAt).string().null())
                        .to_owned(),
                )
                .await?;
        }

        if !submission_has_column(manager, "next_scan_at").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Submission::Table)
                        .add_column(ColumnDef::new(Submission::NextScanAt).string().null())
                        .to_owned(),
                )
                .await?;
        }

        if !submission_has_column(manager, "no_update_streak").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Submission::Table)
                        .add_column(
                            ColumnDef::new(Submission::NoUpdateStreak)
                                .integer()
                                .not_null()
                                .default(0),
                        )
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if submission_has_column(manager, "last_scan_at").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Submission::Table)
                        .drop_column(Submission::LastScanAt)
                        .to_owned(),
                )
                .await?;
        }

        if submission_has_column(manager, "next_scan_at").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Submission::Table)
                        .drop_column(Submission::NextScanAt)
                        .to_owned(),
                )
                .await?;
        }

        if submission_has_column(manager, "no_update_streak").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Submission::Table)
                        .drop_column(Submission::NoUpdateStreak)
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Submission {
    Table,
    LastScanAt,
    NextScanAt,
    NoUpdateStreak,
}

async fn submission_has_column(manager: &SchemaManager<'_>, column: &str) -> Result<bool, DbErr> {
    let backend = manager.get_connection().get_database_backend();
    let sql = format!(
        "SELECT COUNT(*) FROM pragma_table_info('submission') WHERE name = '{}'",
        column.replace('\'', "''")
    );
    let result = manager
        .get_connection()
        .query_one(Statement::from_string(backend, sql))
        .await?;
    Ok(result.and_then(|row| row.try_get_by_index(0).ok()).unwrap_or(0) >= 1)
}
