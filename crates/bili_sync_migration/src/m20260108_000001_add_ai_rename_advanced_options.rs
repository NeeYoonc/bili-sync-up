use sea_orm::ConnectionTrait;
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        // 为收藏夹表添加AI重命名高级选项字段
        if !column_exists(db, "favorite", "ai_rename_enable_multi_page").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Favorite::Table)
                        .add_column(
                            ColumnDef::new(Favorite::AiRenameEnableMultiPage)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .to_owned(),
                )
                .await?;
        }

        if !column_exists(db, "favorite", "ai_rename_enable_collection").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Favorite::Table)
                        .add_column(
                            ColumnDef::new(Favorite::AiRenameEnableCollection)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .to_owned(),
                )
                .await?;
        }

        if !column_exists(db, "favorite", "ai_rename_enable_bangumi").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Favorite::Table)
                        .add_column(
                            ColumnDef::new(Favorite::AiRenameEnableBangumi)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .to_owned(),
                )
                .await?;
        }

        // 为合集表添加AI重命名高级选项字段
        if !column_exists(db, "collection", "ai_rename_enable_multi_page").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Collection::Table)
                        .add_column(
                            ColumnDef::new(Collection::AiRenameEnableMultiPage)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .to_owned(),
                )
                .await?;
        }

        if !column_exists(db, "collection", "ai_rename_enable_collection").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Collection::Table)
                        .add_column(
                            ColumnDef::new(Collection::AiRenameEnableCollection)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .to_owned(),
                )
                .await?;
        }

        if !column_exists(db, "collection", "ai_rename_enable_bangumi").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Collection::Table)
                        .add_column(
                            ColumnDef::new(Collection::AiRenameEnableBangumi)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .to_owned(),
                )
                .await?;
        }

        // 为UP主投稿表添加AI重命名高级选项字段
        if !column_exists(db, "submission", "ai_rename_enable_multi_page").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Submission::Table)
                        .add_column(
                            ColumnDef::new(Submission::AiRenameEnableMultiPage)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .to_owned(),
                )
                .await?;
        }

        if !column_exists(db, "submission", "ai_rename_enable_collection").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Submission::Table)
                        .add_column(
                            ColumnDef::new(Submission::AiRenameEnableCollection)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .to_owned(),
                )
                .await?;
        }

        if !column_exists(db, "submission", "ai_rename_enable_bangumi").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Submission::Table)
                        .add_column(
                            ColumnDef::new(Submission::AiRenameEnableBangumi)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .to_owned(),
                )
                .await?;
        }

        // 为稍后再看表添加AI重命名高级选项字段
        if !column_exists(db, "watch_later", "ai_rename_enable_multi_page").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(WatchLater::Table)
                        .add_column(
                            ColumnDef::new(WatchLater::AiRenameEnableMultiPage)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .to_owned(),
                )
                .await?;
        }

        if !column_exists(db, "watch_later", "ai_rename_enable_collection").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(WatchLater::Table)
                        .add_column(
                            ColumnDef::new(WatchLater::AiRenameEnableCollection)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .to_owned(),
                )
                .await?;
        }

        if !column_exists(db, "watch_later", "ai_rename_enable_bangumi").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(WatchLater::Table)
                        .add_column(
                            ColumnDef::new(WatchLater::AiRenameEnableBangumi)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .to_owned(),
                )
                .await?;
        }

        // 为番剧表添加AI重命名高级选项字段
        if !column_exists(db, "video_source", "ai_rename_enable_multi_page").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(VideoSource::Table)
                        .add_column(
                            ColumnDef::new(VideoSource::AiRenameEnableMultiPage)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .to_owned(),
                )
                .await?;
        }

        if !column_exists(db, "video_source", "ai_rename_enable_collection").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(VideoSource::Table)
                        .add_column(
                            ColumnDef::new(VideoSource::AiRenameEnableCollection)
                                .boolean()
                                .not_null()
                                .default(false),
                        )
                        .to_owned(),
                )
                .await?;
        }

        if !column_exists(db, "video_source", "ai_rename_enable_bangumi").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(VideoSource::Table)
                        .add_column(
                            ColumnDef::new(VideoSource::AiRenameEnableBangumi)
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

        // 删除收藏夹表的AI重命名高级选项字段
        if column_exists(db, "favorite", "ai_rename_enable_multi_page").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Favorite::Table)
                        .drop_column(Favorite::AiRenameEnableMultiPage)
                        .to_owned(),
                )
                .await?;
        }

        if column_exists(db, "favorite", "ai_rename_enable_collection").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Favorite::Table)
                        .drop_column(Favorite::AiRenameEnableCollection)
                        .to_owned(),
                )
                .await?;
        }

        if column_exists(db, "favorite", "ai_rename_enable_bangumi").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Favorite::Table)
                        .drop_column(Favorite::AiRenameEnableBangumi)
                        .to_owned(),
                )
                .await?;
        }

        // 删除合集表的AI重命名高级选项字段
        if column_exists(db, "collection", "ai_rename_enable_multi_page").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Collection::Table)
                        .drop_column(Collection::AiRenameEnableMultiPage)
                        .to_owned(),
                )
                .await?;
        }

        if column_exists(db, "collection", "ai_rename_enable_collection").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Collection::Table)
                        .drop_column(Collection::AiRenameEnableCollection)
                        .to_owned(),
                )
                .await?;
        }

        if column_exists(db, "collection", "ai_rename_enable_bangumi").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Collection::Table)
                        .drop_column(Collection::AiRenameEnableBangumi)
                        .to_owned(),
                )
                .await?;
        }

        // 删除UP主投稿表的AI重命名高级选项字段
        if column_exists(db, "submission", "ai_rename_enable_multi_page").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Submission::Table)
                        .drop_column(Submission::AiRenameEnableMultiPage)
                        .to_owned(),
                )
                .await?;
        }

        if column_exists(db, "submission", "ai_rename_enable_collection").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Submission::Table)
                        .drop_column(Submission::AiRenameEnableCollection)
                        .to_owned(),
                )
                .await?;
        }

        if column_exists(db, "submission", "ai_rename_enable_bangumi").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Submission::Table)
                        .drop_column(Submission::AiRenameEnableBangumi)
                        .to_owned(),
                )
                .await?;
        }

        // 删除稍后再看表的AI重命名高级选项字段
        if column_exists(db, "watch_later", "ai_rename_enable_multi_page").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(WatchLater::Table)
                        .drop_column(WatchLater::AiRenameEnableMultiPage)
                        .to_owned(),
                )
                .await?;
        }

        if column_exists(db, "watch_later", "ai_rename_enable_collection").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(WatchLater::Table)
                        .drop_column(WatchLater::AiRenameEnableCollection)
                        .to_owned(),
                )
                .await?;
        }

        if column_exists(db, "watch_later", "ai_rename_enable_bangumi").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(WatchLater::Table)
                        .drop_column(WatchLater::AiRenameEnableBangumi)
                        .to_owned(),
                )
                .await?;
        }

        // 删除番剧表的AI重命名高级选项字段
        if column_exists(db, "video_source", "ai_rename_enable_multi_page").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(VideoSource::Table)
                        .drop_column(VideoSource::AiRenameEnableMultiPage)
                        .to_owned(),
                )
                .await?;
        }

        if column_exists(db, "video_source", "ai_rename_enable_collection").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(VideoSource::Table)
                        .drop_column(VideoSource::AiRenameEnableCollection)
                        .to_owned(),
                )
                .await?;
        }

        if column_exists(db, "video_source", "ai_rename_enable_bangumi").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(VideoSource::Table)
                        .drop_column(VideoSource::AiRenameEnableBangumi)
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }
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

#[derive(Iden)]
enum Favorite {
    Table,
    AiRenameEnableMultiPage,
    AiRenameEnableCollection,
    AiRenameEnableBangumi,
}

#[derive(Iden)]
#[allow(clippy::enum_variant_names, reason = "Iden 变体名即数据库列名，不能改名")]
enum Collection {
    Table,
    AiRenameEnableMultiPage,
    AiRenameEnableCollection,
    AiRenameEnableBangumi,
}

#[derive(Iden)]
enum Submission {
    Table,
    AiRenameEnableMultiPage,
    AiRenameEnableCollection,
    AiRenameEnableBangumi,
}

#[derive(Iden)]
enum WatchLater {
    Table,
    AiRenameEnableMultiPage,
    AiRenameEnableCollection,
    AiRenameEnableBangumi,
}

#[derive(Iden)]
enum VideoSource {
    Table,
    AiRenameEnableMultiPage,
    AiRenameEnableCollection,
    AiRenameEnableBangumi,
}
