use sea_orm::{ConnectionTrait, DbErr, Statement};
use sea_orm_migration::prelude::*;
use std::path::Path;

/// 外源任务状态位按“媒体文件真实存在 + NFO 已生成”补回填。
///
/// 背景：20260903 引入外源 B 站式状态位时只把 download_status 已经是
/// completed/skipped 的行回填为全完成；而旧版外源状态展示是按磁盘文件实时推导的，
/// 大量“文件其实完整、但 DB 文本停留在 pending”（历史中断恢复、旧版重置只改
/// 文本没删文件等）的行升级后状态位全 0，被误判为未下载并进入重下队列。
///
/// 判定条件刻意保守，避免把“正在下载/中断的半成品”误标为已完成：
/// 1. 只处理文本 pending（downloading 留给启动时的断点续传/恢复逻辑自愈，不在此改）；
/// 2. 状态位必须还是全 0（用户手动重试/重置过的行不动）；
/// 3. 媒体文件真实存在且非空；
/// 4. 同目录同名 .nfo 已生成——外源流程是“媒体完整下载成功后才生成 NFO”，
///    半成品中断（curl/原生分片直接写 output_path）不会带 NFO，因此这是
///    “这条视频曾经完整下载过”的可靠信号。
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let backend = conn.get_database_backend();
        // 列不存在说明还没有跑过 20260903（全新库/极旧库），后续迁移链会自然补齐，
        // 本迁移没有可修复对象。
        if !table_has_column(manager, "you_tube_video", "video_task_status").await? {
            return Ok(());
        }

        let sql = "SELECT id, output_path FROM you_tube_video \
                   WHERE download_status = 'pending' \
                     AND video_task_status = 0 AND page_task_status = 0 \
                     AND output_path IS NOT NULL AND output_path != ''";
        let rows = conn
            .query_all(Statement::from_string(backend, sql))
            .await?;

        let mut ids: Vec<i64> = Vec::new();
        for row in &rows {
            let id: i64 = row.try_get_by_index(0)?;
            let output_path: Option<String> = row.try_get_by_index(1)?;
            let Some(output_path) = output_path else { continue };
            // 媒体文件真实存在且非空
            let Ok(meta) = std::fs::metadata(&output_path) else { continue };
            if !meta.is_file() || meta.len() == 0 {
                continue;
            }
            // NFO 已生成才认为“曾完整下载过”：外源是媒体成功后才写 NFO，
            // 中断的半成品不会带 NFO，避免把损坏文件误标为已完成。
            let nfo_path = Path::new(&output_path).with_extension("nfo");
            if !nfo_path.is_file() || !std::fs::metadata(&nfo_path).is_ok_and(|meta| meta.len() > 0) {
                continue;
            }
            ids.push(id);
        }
        if ids.is_empty() {
            return Ok(());
        }

        // 与 20260903 相同的全完成编码：5 个子任务各 0b111 即 0x7FFF，bit31 完成标记。
        let all_ok: i64 = (1i64 << 31) | 0x7FFF;
        for chunk in ids.chunks(200) {
            let params = chunk.iter().map(|id| id.to_string()).collect::<Vec<_>>().join(",");
            let sql = format!(
                "UPDATE you_tube_video SET video_task_status = {all_ok}, page_task_status = {all_ok}, \
                 download_status = 'completed', retry_count = 0 \
                 WHERE id IN ({params})"
            );
            conn.execute(Statement::from_string(backend, sql)).await?;
        }
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // 纯数据修复迁移，不回滚。
        Ok(())
    }
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
