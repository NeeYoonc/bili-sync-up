use sea_orm::{ConnectionTrait, DbErr, Statement};
use sea_orm_migration::prelude::*;

/// 把历史失败（failed）的 TikTok 图片贴重新放回待下载队列。
///
/// TikTok 的图片贴（photo post，也就是幻灯片）没有视频流：旧版本一律走视频
/// 解析与选流，yt-dlp 只能给出配乐音轨、官方 item/detail 又常返回空响应，
/// 因此这类作品**必然**下载失败并停到 `failed`（重试 4 次后不再自动重试）。
/// 新版本已按图文链路处理（下载原图与配乐并合成幻灯片 MP4）。
///
/// 由于外源的自动下载只挑 `pending`（见 `download_pending`），这些历史失败行
/// 不会自愈。这里在升级时把它们重置为 `pending`，让新链路自动补下；只处理
/// 明确带 `is_image_post = 1` 标记、来源为 TikTok 的行，不触碰普通视频与
/// 抖音图文（后者一直是正常完成的）。
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let backend = conn.get_database_backend();
        if !table_exists(manager, "you_tube_video").await? || !table_exists(manager, "you_tube_source").await? {
            return Ok(());
        }
        if !table_has_column(manager, "you_tube_video", "is_image_post").await? {
            return Ok(());
        }

        let sql = "UPDATE you_tube_video \
                   SET download_status = 'pending', retry_count = 0, error_message = NULL \
                   WHERE is_image_post = 1 \
                     AND download_status = 'failed' \
                     AND source_id IN (SELECT id FROM you_tube_source WHERE source_type LIKE 'tiktok%')";
        conn.execute(Statement::from_string(backend, sql)).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}

async fn table_exists(manager: &SchemaManager<'_>, table_name: &str) -> Result<bool, DbErr> {
    let backend = manager.get_connection().get_database_backend();
    let sql = format!(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = '{}'",
        table_name.replace('\'', "''")
    );
    let result = manager
        .get_connection()
        .query_one(Statement::from_string(backend, sql))
        .await?;
    Ok(result.and_then(|row| row.try_get_by_index(0).ok()).unwrap_or(0) >= 1)
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
