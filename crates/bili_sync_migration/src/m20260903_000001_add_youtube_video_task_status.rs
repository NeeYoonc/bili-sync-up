use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

/// 外源视频对齐 B 站：为 you_tube_video 增加“子任务状态位”。
///
/// `video_task_status` / `page_task_status` 与 B 站 video/page 的
/// download_status 编码完全一致：u32 从低位起每 3bit 一个子任务，
/// bit31 为整条完成标记（全部子任务置 7 时由 Status::set 自动打上）。
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        add_column_if_missing(
            manager,
            "you_tube_video",
            YouTubeVideo::Table,
            "video_task_status",
            ColumnDef::new(YouTubeVideo::VideoTaskStatus)
                .big_integer()
                .not_null()
                .default(0)
                .to_owned(),
        )
        .await?;
        add_column_if_missing(
            manager,
            "you_tube_video",
            YouTubeVideo::Table,
            "page_task_status",
            ColumnDef::new(YouTubeVideo::PageTaskStatus)
                .big_integer()
                .not_null()
                .default(0)
                .to_owned(),
        )
        .await?;

        // 回填：已完成 / 主动跳过 / 付费占位 全部视为子任务完成。
        // 5 个子任务各 0b111：0x7FFF；bit31 完成标记。
        let all_ok: i64 = (1i64 << 31) | 0x7FFF;
        let sql = format!(
            "UPDATE you_tube_video SET video_task_status = {all_ok}, page_task_status = {all_ok}              WHERE download_status IN ('completed','skipped') AND video_task_status = 0"
        );
        manager.get_connection().execute_unprepared(&sql).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        drop_column_if_exists(
            manager,
            "you_tube_video",
            YouTubeVideo::Table,
            "video_task_status",
            YouTubeVideo::VideoTaskStatus,
        )
        .await?;
        drop_column_if_exists(
            manager,
            "you_tube_video",
            YouTubeVideo::Table,
            "page_task_status",
            YouTubeVideo::PageTaskStatus,
        )
        .await?;
        Ok(())
    }
}

async fn add_column_if_missing<T>(
    manager: &SchemaManager<'_>,
    table_name: &str,
    table: T,
    column_name: &str,
    column_def: ColumnDef,
) -> Result<(), DbErr>
where
    T: IntoIden + Clone + 'static,
{
    if !table_has_column(manager, table_name, column_name).await? {
        manager
            .alter_table(Table::alter().table(table).add_column(column_def).to_owned())
            .await?;
    }

    Ok(())
}

async fn drop_column_if_exists<T, C>(
    manager: &SchemaManager<'_>,
    table_name: &str,
    table: T,
    column_name: &str,
    column: C,
) -> Result<(), DbErr>
where
    T: IntoIden + Clone + 'static,
    C: IntoIden + 'static,
{
    if table_has_column(manager, table_name, column_name).await? {
        manager
            .alter_table(Table::alter().table(table).drop_column(column).to_owned())
            .await?;
    }

    Ok(())
}

async fn table_has_column(manager: &SchemaManager<'_>, table_name: &str, column_name: &str) -> Result<bool, DbErr> {
    let backend = manager.get_connection().get_database_backend();
    let sql = format!(
        "SELECT COUNT(*) FROM pragma_table_info('{}') WHERE name = '{}'",
        table_name.replace('\'', "''"),
        column_name.replace('\'', "''")
    );
    let result = manager
        .get_connection()
        .query_one(Statement::from_string(backend, sql))
        .await?;
    Ok(result.and_then(|row| row.try_get_by_index(0).ok()).unwrap_or(0) >= 1)
}

#[derive(Iden, Clone)]
enum YouTubeVideo {
    Table,
    VideoTaskStatus,
    PageTaskStatus,
}
