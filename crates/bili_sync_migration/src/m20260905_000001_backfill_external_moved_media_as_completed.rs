use sea_orm::{ConnectionTrait, DbErr, Statement};
use sea_orm_migration::prelude::*;
use std::path::Path;

/// 外源“媒体已被移走/归档”的已完成行补回状态位。
///
/// 20260904 的回填要求“媒体文件当前仍在盘上”，导致另一类历史行仍被漏掉：
/// 用户下载完成后把视频文件移走归档（output_path 指向旧位置、文件已不存在），
/// 这些行在旧版里同样显示为已完成，升级后却被判为未下载并整批重新下载。
///
/// 关键信号：旧版只有“整条下载成功”才会写入 you_tube_video.output_path
/// （失败分支只改状态与错误信息，不动 output_path）。因此对位仍为全 0 的
/// pending 行，只要 output_path 非空，就代表这条视频曾经完整下载成功过：
/// - 文件已不在盘上（被移走/归档/清理）→ 标为已完成，不重复下载；
/// - 文件在盘且 NFO 已生成 → 标为已完成（与 20260904 语义一致，幂等）；
/// - 文件在盘但 NFO 缺失（下载中断的半成品）→ 不动，留给续传/重下自愈。
#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        let backend = conn.get_database_backend();
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
            let media = Path::new(&output_path);
            let nfo_path = media.with_extension("nfo");
            match std::fs::metadata(media) {
                // 文件已不在盘上：output_path 非空即代表曾经完整下载成功过
                //（旧版只在成功时写 output_path），媒体被移走/归档不重复下载。
                Err(_) => ids.push(id),
                Ok(meta) => {
                    // 文件在盘：要求 NFO 也已生成（曾完整跑完下载流程），
                    // 避免把中断留下的半成品误标为已完成。
                    if meta.is_file()
                        && meta.len() > 0
                        && nfo_path.is_file()
                        && std::fs::metadata(&nfo_path).is_ok_and(|meta| meta.len() > 0)
                    {
                        ids.push(id);
                    }
                }
            }
        }
        if ids.is_empty() {
            return Ok(());
        }

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
