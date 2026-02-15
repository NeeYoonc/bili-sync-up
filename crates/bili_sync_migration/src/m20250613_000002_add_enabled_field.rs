use sea_orm::ConnectionTrait;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 为 video_source 表添加 enabled 字段
        let db = manager.get_connection();
        if table_exists(db, "video_source").await? && !column_exists(db, "video_source", "enabled").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(VideoSource::Table)
                        .add_column(
                            ColumnDef::new(VideoSource::Enabled)
                                .boolean()
                                .not_null()
                                .default(true)
                                .comment("视频源是否启用"),
                        )
                        .to_owned(),
                )
                .await?;
        }

        // 为其他视频源表也添加 enabled 字段
        // 收藏夹表
        if table_exists(db, "favorite").await? && !column_exists(db, "favorite", "enabled").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Favorite::Table)
                        .add_column(
                            ColumnDef::new(Favorite::Enabled)
                                .boolean()
                                .not_null()
                                .default(true)
                                .comment("收藏夹是否启用"),
                        )
                        .to_owned(),
                )
                .await?;
        }

        // 合集表
        if table_exists(db, "collection").await? && !column_exists(db, "collection", "enabled").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Collection::Table)
                        .add_column(
                            ColumnDef::new(Collection::Enabled)
                                .boolean()
                                .not_null()
                                .default(true)
                                .comment("合集是否启用"),
                        )
                        .to_owned(),
                )
                .await?;
        }

        // UP主投稿表
        if table_exists(db, "submission").await? && !column_exists(db, "submission", "enabled").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Submission::Table)
                        .add_column(
                            ColumnDef::new(Submission::Enabled)
                                .boolean()
                                .not_null()
                                .default(true)
                                .comment("UP主投稿是否启用"),
                        )
                        .to_owned(),
                )
                .await?;
        }

        // 稍后观看表
        if table_exists(db, "watch_later").await? && !column_exists(db, "watch_later", "enabled").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(WatchLater::Table)
                        .add_column(
                            ColumnDef::new(WatchLater::Enabled)
                                .boolean()
                                .not_null()
                                .default(true)
                                .comment("稍后观看是否启用"),
                        )
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // 删除 enabled 字段
        let db = manager.get_connection();
        if table_exists(db, "video_source").await? && column_exists(db, "video_source", "enabled").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(VideoSource::Table)
                        .drop_column(VideoSource::Enabled)
                        .to_owned(),
                )
                .await?;
        }

        if table_exists(db, "favorite").await? && column_exists(db, "favorite", "enabled").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Favorite::Table)
                        .drop_column(Favorite::Enabled)
                        .to_owned(),
                )
                .await?;
        }

        if table_exists(db, "collection").await? && column_exists(db, "collection", "enabled").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Collection::Table)
                        .drop_column(Collection::Enabled)
                        .to_owned(),
                )
                .await?;
        }

        if table_exists(db, "submission").await? && column_exists(db, "submission", "enabled").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Submission::Table)
                        .drop_column(Submission::Enabled)
                        .to_owned(),
                )
                .await?;
        }

        if table_exists(db, "watch_later").await? && column_exists(db, "watch_later", "enabled").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(WatchLater::Table)
                        .drop_column(WatchLater::Enabled)
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }
}

#[derive(DeriveIden)]
enum VideoSource {
    Table,
    Enabled,
}

#[derive(DeriveIden)]
enum Favorite {
    Table,
    Enabled,
}

#[derive(DeriveIden)]
enum Collection {
    Table,
    Enabled,
}

#[derive(DeriveIden)]
enum Submission {
    Table,
    Enabled,
}

#[derive(DeriveIden)]
enum WatchLater {
    Table,
    Enabled,
}

/// 检查SQLite表是否存在
async fn table_exists<C: ConnectionTrait>(db: &C, table_name: &str) -> Result<bool, DbErr> {
    use sea_orm::Statement;

    let sql = "SELECT 1 FROM sqlite_master WHERE type='table' AND name=? LIMIT 1";
    let stmt = Statement::from_sql_and_values(sea_orm::DatabaseBackend::Sqlite, sql, vec![table_name.into()]);
    let result = db.query_one(stmt).await?;
    Ok(result.is_some())
}

/// 检查SQLite表中是否存在指定列
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
