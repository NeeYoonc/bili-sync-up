use sea_orm::Statement;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !video_has_column(manager, "submission_membership_state").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Video::Table)
                        .add_column(
                            ColumnDef::new(Video::SubmissionMembershipState)
                                .integer()
                                .not_null()
                                .default(0),
                        )
                        .to_owned(),
                )
                .await?;
        }

        if !video_has_column(manager, "submission_membership_checked_at").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Video::Table)
                        .add_column(
                            ColumnDef::new(Video::SubmissionMembershipCheckedAt)
                                .big_integer()
                                .null(),
                        )
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if video_has_column(manager, "submission_membership_checked_at").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Video::Table)
                        .drop_column(Video::SubmissionMembershipCheckedAt)
                        .to_owned(),
                )
                .await?;
        }

        if video_has_column(manager, "submission_membership_state").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Video::Table)
                        .drop_column(Video::SubmissionMembershipState)
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }
}

#[derive(DeriveIden)]
enum Video {
    Table,
    SubmissionMembershipState,
    SubmissionMembershipCheckedAt,
}

async fn video_has_column(manager: &SchemaManager<'_>, column: &str) -> Result<bool, DbErr> {
    let backend = manager.get_connection().get_database_backend();
    let sql = format!(
        "SELECT COUNT(*) FROM pragma_table_info('video') WHERE name = '{}'",
        column.replace('\'', "''")
    );
    let result = manager
        .get_connection()
        .query_one(Statement::from_string(backend, sql))
        .await?;
    Ok(result.and_then(|row| row.try_get_by_index(0).ok()).unwrap_or(0) >= 1)
}
