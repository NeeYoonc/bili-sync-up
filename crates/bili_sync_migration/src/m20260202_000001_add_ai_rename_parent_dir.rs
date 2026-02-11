use sea_orm::ConnectionTrait;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        if !column_exists(db, "favorite", "ai_rename_rename_parent_dir").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Favorite::Table)
                        .add_column(
                            ColumnDef::new(Favorite::AiRenameRenameParentDir)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .to_owned(),
                )
                .await?;
        }

        if !column_exists(db, "collection", "ai_rename_rename_parent_dir").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Collection::Table)
                        .add_column(
                            ColumnDef::new(Collection::AiRenameRenameParentDir)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .to_owned(),
                )
                .await?;
        }

        if !column_exists(db, "submission", "ai_rename_rename_parent_dir").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Submission::Table)
                        .add_column(
                            ColumnDef::new(Submission::AiRenameRenameParentDir)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .to_owned(),
                )
                .await?;
        }

        if !column_exists(db, "watch_later", "ai_rename_rename_parent_dir").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(WatchLater::Table)
                        .add_column(
                            ColumnDef::new(WatchLater::AiRenameRenameParentDir)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .to_owned(),
                )
                .await?;
        }

        if !column_exists(db, "video_source", "ai_rename_rename_parent_dir").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(VideoSource::Table)
                        .add_column(
                            ColumnDef::new(VideoSource::AiRenameRenameParentDir)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        if column_exists(db, "favorite", "ai_rename_rename_parent_dir").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Favorite::Table)
                        .drop_column(Favorite::AiRenameRenameParentDir)
                        .to_owned(),
                )
                .await?;
        }

        if column_exists(db, "collection", "ai_rename_rename_parent_dir").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Collection::Table)
                        .drop_column(Collection::AiRenameRenameParentDir)
                        .to_owned(),
                )
                .await?;
        }

        if column_exists(db, "submission", "ai_rename_rename_parent_dir").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Submission::Table)
                        .drop_column(Submission::AiRenameRenameParentDir)
                        .to_owned(),
                )
                .await?;
        }

        if column_exists(db, "watch_later", "ai_rename_rename_parent_dir").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(WatchLater::Table)
                        .drop_column(WatchLater::AiRenameRenameParentDir)
                        .to_owned(),
                )
                .await?;
        }

        if column_exists(db, "video_source", "ai_rename_rename_parent_dir").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(VideoSource::Table)
                        .drop_column(VideoSource::AiRenameRenameParentDir)
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }
}

async fn column_exists<C: ConnectionTrait>(db: &C, table_name: &str, column_name: &str) -> Result<bool, DbErr> {
    use sea_orm::Statement;

    let sql = format!("PRAGMA table_info({})", table_name);
    let result = db
        .query_all(Statement::from_string(sea_orm::DatabaseBackend::Sqlite, sql))
        .await?;

    for row in result {
        let name: String = row.try_get("", "name")?;
        if name == column_name {
            return Ok(true);
        }
    }

    Ok(false)
}

#[derive(Iden)]
enum Favorite {
    Table,
    AiRenameRenameParentDir,
}

#[derive(Iden)]
enum Collection {
    Table,
    AiRenameRenameParentDir,
}

#[derive(Iden)]
enum Submission {
    Table,
    AiRenameRenameParentDir,
}

#[derive(Iden)]
enum WatchLater {
    Table,
    AiRenameRenameParentDir,
}

#[derive(Iden)]
enum VideoSource {
    Table,
    AiRenameRenameParentDir,
}
