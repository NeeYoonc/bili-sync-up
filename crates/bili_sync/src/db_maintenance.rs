//! 数据库管理：状态概览与维护操作（设置页 → 数据库管理）。
//!
//! 所有操作直接面向 SQLite 主库 `CONFIG_DIR/data.sqlite`：
//! - 只读概览：文件大小 / WAL / 可回收空间 / 各表记录数 / YouTube 视频状态统计
//! - 清理类：图片代理缓存、AI 对话历史、任务队列历史、孤立记录
//! - 维护类：VACUUM 压缩、VACUUM INTO 快照备份（不锁库、不影响运行）

use anyhow::{bail, Context, Result};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use tracing::info;

use crate::api::request::DatabaseMaintenanceAction;
use crate::api::response::{
    DatabaseBackupInfo, DatabaseBackupListResponse, DatabaseMaintenanceResponse, DatabaseRestoreResponse,
    DatabaseStatusCount, DatabaseStatusResponse, DatabaseTableStat,
};
use crate::config::CONFIG_DIR;

/// 数据库文件路径（与 database.rs 保持一致）。
fn database_file() -> std::path::PathBuf {
    CONFIG_DIR.join("data.sqlite")
}

async fn file_size(path: &std::path::Path) -> u64 {
    tokio::fs::metadata(path).await.map(|meta| meta.len()).unwrap_or(0)
}

fn table_label(table: &str) -> &'static str {
    match table {
        "video" => "B站视频",
        "page" => "B站分P",
        "video_source" => "B站视频源",
        "you_tube_source" => "YouTube 来源",
        "you_tube_video" => "YouTube 视频",
        "task_queue" => "任务队列",
        "ai_conversation_history" => "AI 对话历史",
        "image_proxy_cache" => "图片代理缓存",
        "config_changes" => "配置变更记录",
        "config_items" => "配置项",
        "collection" => "合集",
        "favorite" => "收藏夹",
        "watch_later" => "稍后再看",
        "submission" => "UP主投稿",
        "collection_season_mapping" => "合集季度映射",
        _ => "其他表",
    }
}

const STATUS_TABLES: &[&str] = &[
    "video",
    "page",
    "video_source",
    "you_tube_source",
    "you_tube_video",
    "task_queue",
    "ai_conversation_history",
    "image_proxy_cache",
    "config_changes",
    "config_items",
    "collection",
    "favorite",
    "watch_later",
    "submission",
    "collection_season_mapping",
];

async fn table_rows(db: &DatabaseConnection, table: &str) -> i64 {
    let backend = db.get_database_backend();
    let sql = format!("SELECT COUNT(*) FROM {table}");
    db.query_one(Statement::from_string(backend, sql))
        .await
        .ok()
        .flatten()
        .and_then(|row| row.try_get_by_index::<i64>(0).ok())
        .unwrap_or(0)
}

async fn reclaimable_bytes(db: &DatabaseConnection) -> u64 {
    let backend = db.get_database_backend();
    let freelist = db
        .query_one(Statement::from_string(backend, "PRAGMA freelist_count"))
        .await
        .ok()
        .flatten()
        .and_then(|row| row.try_get_by_index::<i64>(0).ok())
        .unwrap_or(0)
        .max(0) as u64;
    let page_size = db
        .query_one(Statement::from_string(backend, "PRAGMA page_size"))
        .await
        .ok()
        .flatten()
        .and_then(|row| row.try_get_by_index::<i64>(0).ok())
        .unwrap_or(4096)
        .max(0) as u64;
    freelist.saturating_mul(page_size)
}

/// 查询数据库状态概览。
pub async fn database_status(db: &DatabaseConnection) -> Result<DatabaseStatusResponse> {
    let path = database_file();
    let db_size_bytes = file_size(&path).await;
    let wal_size_bytes = file_size(&path.with_extension("sqlite-wal")).await;
    let reclaimable = reclaimable_bytes(db).await;

    let mut tables = Vec::with_capacity(STATUS_TABLES.len());
    for table in STATUS_TABLES {
        let rows = table_rows(db, table).await;
        tables.push(DatabaseTableStat {
            table: (*table).to_string(),
            rows,
            label: table_label(table).to_string(),
        });
    }
    tables.sort_by(|left, right| right.rows.cmp(&left.rows));

    let backend = db.get_database_backend();
    let youtube_video_status = db
        .query_all(Statement::from_string(
            backend,
            "SELECT download_status AS status, COUNT(*) AS count FROM you_tube_video GROUP BY download_status ORDER BY count DESC",
        ))
        .await
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    let status = row.try_get_by_index::<String>(0).ok();
                    let count = row.try_get_by_index::<i64>(1).ok();
                    status.zip(count).map(|(status, count)| DatabaseStatusCount { status, count })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(DatabaseStatusResponse {
        path: path.display().to_string(),
        db_size_bytes,
        wal_size_bytes,
        reclaimable_bytes: reclaimable,
        tables,
        youtube_video_status,
    })
}

async fn delete_all(db: &DatabaseConnection, table: &str) -> Result<u64> {
    let backend = db.get_database_backend();
    let result = db
        .execute(Statement::from_string(backend, format!("DELETE FROM {table}")))
        .await
        .with_context(|| format!("清理 {table} 失败"))?;
    Ok(result.rows_affected())
}

async fn delete_where(db: &DatabaseConnection, sql: &str) -> Result<u64> {
    let backend = db.get_database_backend();
    let result = db
        .execute(Statement::from_string(backend, sql.to_string()))
        .await
        .with_context(|| "执行数据库清理 SQL 失败".to_string())?;
    Ok(result.rows_affected())
}

/// 执行数据库维护操作。
pub async fn run_maintenance(
    db: &DatabaseConnection,
    action: DatabaseMaintenanceAction,
) -> Result<DatabaseMaintenanceResponse> {
    match action {
        DatabaseMaintenanceAction::ClearImageCache => {
            let removed = delete_all(db, "image_proxy_cache").await?;
            info!(removed, "数据库管理：已清空图片代理缓存");
            Ok(DatabaseMaintenanceResponse {
                success: true,
                message: format!("已清空图片代理缓存 {removed} 条"),
                removed_rows: Some(removed),
                ..Default::default()
            })
        }
        DatabaseMaintenanceAction::ClearAiHistory => {
            let removed = delete_all(db, "ai_conversation_history").await?;
            info!(removed, "数据库管理：已清空 AI 对话历史");
            Ok(DatabaseMaintenanceResponse {
                success: true,
                message: format!("已清空 AI 对话历史 {removed} 条"),
                removed_rows: Some(removed),
                ..Default::default()
            })
        }
        DatabaseMaintenanceAction::ClearQueueHistory => {
            let removed = delete_where(
                db,
                "DELETE FROM task_queue WHERE status IN ('completed', 'failed')",
            )
            .await?;
            info!(removed, "数据库管理：已清理任务队列历史");
            Ok(DatabaseMaintenanceResponse {
                success: true,
                message: format!("已清理任务队列历史 {removed} 条"),
                removed_rows: Some(removed),
                ..Default::default()
            })
        }
        DatabaseMaintenanceAction::CleanOrphans => {
            let removed_youtube = delete_where(
                db,
                "DELETE FROM you_tube_video WHERE source_id NOT IN (SELECT id FROM you_tube_source)",
            )
            .await?;
            let removed_pages = delete_where(
                db,
                "DELETE FROM page WHERE video_id NOT IN (SELECT id FROM video)",
            )
            .await?;
            let removed = removed_youtube + removed_pages;
            info!(removed_youtube, removed_pages, "数据库管理：已清理孤立记录");
            Ok(DatabaseMaintenanceResponse {
                success: true,
                message: format!("已清理孤立记录 {removed} 条（YouTube 视频 {removed_youtube}，无主分P {removed_pages}）"),
                removed_rows: Some(removed),
                ..Default::default()
            })
        }
        DatabaseMaintenanceAction::Vacuum => {
            let before = file_size(&database_file()).await;
            // 先截断 WAL，避免 VACUUM 后 WAL 里残留大量旧页。
            let _ = db.execute_unprepared("PRAGMA wal_checkpoint(TRUNCATE)").await;
            db.execute_unprepared("VACUUM").await.with_context(|| "VACUUM 压缩数据库失败".to_string())?;
            let after = file_size(&database_file()).await;
            let freed = before.saturating_sub(after);
            info!(before, after, "数据库管理：VACUUM 压缩完成");
            Ok(DatabaseMaintenanceResponse {
                success: true,
                message: format!("数据库已压缩：{} -> {}（释放约 {}）", before, after, freed),
                size_before_bytes: Some(before),
                size_after_bytes: Some(after),
                ..Default::default()
            })
        }
        DatabaseMaintenanceAction::Backup => {
            let before = file_size(&database_file()).await;
            let timestamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
            let backup_path = CONFIG_DIR.join(format!("data-backup-{timestamp}.sqlite"));
            let escaped = backup_path.display().to_string().replace('\'', "''");
            // VACUUM INTO 生成一致快照，无需独占数据库，不影响运行中的写入。
            db.execute_unprepared(&format!("VACUUM INTO '{escaped}'"))
                .await
                .with_context(|| format!("备份数据库失败：{}", backup_path.display()))?;
            let backup_size = file_size(&backup_path).await;
            info!(path = %backup_path.display(), backup_size, "数据库管理：备份完成");
            Ok(DatabaseMaintenanceResponse {
                success: true,
                message: format!("数据库已备份到 {}", backup_path.display()),
                backup_path: Some(backup_path.display().to_string()),
                backup_size_bytes: Some(backup_size),
                size_before_bytes: Some(before),
                ..Default::default()
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::sqlx::sqlite::SqliteConnectOptions;
    use sea_orm::sqlx::SqlitePool;
    use sea_orm::SqlxSqliteConnector;

    /// 构造一个最小但有效的 bili-sync 备份库（含必需表）。
    async fn make_valid_backup(path: &std::path::Path) {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true);
        let pool = SqlitePool::connect_with(options).await.unwrap();
        let conn = SqlxSqliteConnector::from_sqlx_sqlite_pool(pool.clone());
        for sql in [
            "CREATE TABLE video (id INTEGER PRIMARY KEY)",
            "CREATE TABLE video_source (id INTEGER PRIMARY KEY)",
            "CREATE TABLE config_items (key TEXT PRIMARY KEY)",
            "CREATE TABLE seaql_migrations (version TEXT PRIMARY KEY)",
        ] {
            conn.execute_unprepared(sql).await.unwrap();
        }
        pool.close().await;
    }

    async fn temp_dir(prefix: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        dir
    }

    use super::*;
    use sea_orm::{ConnectionTrait, Database};

    /// 构造最小测试库：仅建维护操作涉及的表。
    async fn test_db() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:")
            .await
            .expect("创建内存测试库失败");
        for sql in [
            "CREATE TABLE you_tube_video (id INTEGER PRIMARY KEY, source_id INTEGER NOT NULL, youtube_id TEXT, url TEXT, title TEXT, uploader TEXT, download_status TEXT NOT NULL DEFAULT 'pending', retry_count INTEGER DEFAULT 0)",
            "CREATE TABLE you_tube_source (id INTEGER PRIMARY KEY, name TEXT)",
            "CREATE TABLE video (id INTEGER PRIMARY KEY, name TEXT)",
            "CREATE TABLE page (id INTEGER PRIMARY KEY, video_id INTEGER NOT NULL, cid INTEGER, pid INTEGER, name TEXT, duration INTEGER DEFAULT 0)",
            "CREATE TABLE task_queue (id INTEGER PRIMARY KEY, task_type TEXT, task_data TEXT, status TEXT, retry_count INTEGER DEFAULT 0)",
            "CREATE TABLE ai_conversation_history (id INTEGER PRIMARY KEY, source_key TEXT, role TEXT, content TEXT, order_index INTEGER DEFAULT 0)",
            "CREATE TABLE image_proxy_cache (cache_key TEXT PRIMARY KEY, url TEXT)",
        ] {
            db.execute_unprepared(sql).await.expect("建表失败");
        }
        db
    }

    #[tokio::test]
    async fn backup_timestamp_parses_name() {
        assert_eq!(
            backup_timestamp("data-backup-20260824-123456.sqlite").as_deref(),
            Some("2026-08-24 12:34:56")
        );
        assert!(backup_timestamp("data-backup-bad.sqlite").is_none());
        assert!(backup_timestamp("other.sqlite").is_none());
    }

    #[tokio::test]
    async fn validate_backup_accepts_real_db_and_rejects_junk() {
        let dir = temp_dir("bili-sync-backup-test").await;
        let valid = dir.join("valid.sqlite");
        make_valid_backup(&valid).await;
        assert!(validate_backup_is_sqlite(&valid).await.is_ok());

        let junk = dir.join("junk.sqlite");
        tokio::fs::write(&junk, b"this is not sqlite").await.unwrap();
        assert!(validate_backup_is_sqlite(&junk).await.is_err());
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn stage_restore_writes_marker_and_staged_copy() {
        let dir = temp_dir("bili-sync-restore-test").await;
        let backup = dir.join("data-backup-20260824-123456.sqlite");
        make_valid_backup(&backup).await;

        stage_restore_at(&dir, "data-backup-20260824-123456.sqlite")
            .await
            .unwrap();
        assert!(dir.join("data.restore.marker").is_file());
        assert!(dir.join("data.restore.sqlite").is_file());
        assert_eq!(
            tokio::fs::read_to_string(dir.join("data.restore.marker"))
                .await
                .unwrap(),
            "data-backup-20260824-123456.sqlite"
        );

        // 路径穿越 / 非法文件名被拒绝
        assert!(stage_restore_at(&dir, "../data.sqlite").await.is_err());
        assert!(stage_restore_at(&dir, "evil.sqlite").await.is_err());
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn status_reports_counts_and_sizes() {
        let db = test_db().await;
        for sql in [
            "INSERT INTO you_tube_video (source_id, youtube_id, url, title, uploader, download_status) VALUES (1,'a','u','t','x','completed')",
            "INSERT INTO you_tube_video (source_id, youtube_id, url, title, uploader, download_status) VALUES (2,'b','u','t','x','failed')",
            "INSERT INTO video (id, name) VALUES (1,'v')",
            "INSERT INTO page (video_id, cid, pid, name) VALUES (1, 100, 1, 'p')",
            "INSERT INTO image_proxy_cache (cache_key, url) VALUES ('k','u')",
        ] {
            db.execute_unprepared(sql).await.expect("插入失败");
        }
        let status = database_status(&db).await.expect("状态查询失败");
        assert!(status.path.contains("data.sqlite"));
        let rows = |table: &str| status.tables.iter().find(|t| t.table == table).map(|t| t.rows).unwrap_or(0);
        assert_eq!(rows("you_tube_video"), 2);
        assert_eq!(rows("video"), 1);
        assert_eq!(rows("page"), 1);
        assert_eq!(rows("image_proxy_cache"), 1);
        assert_eq!(status.youtube_video_status.len(), 2);
    }

    #[tokio::test]
    async fn maintenance_clears_tables_and_orphans() {
        let db = test_db().await;
        for sql in [
            "INSERT INTO you_tube_video (source_id, youtube_id, url, title, uploader) VALUES (1,'a','u','t','x')",
            "INSERT INTO you_tube_video (source_id, youtube_id, url, title, uploader) VALUES (99,'o','u','t','x')",
            "INSERT INTO you_tube_source (id, name) VALUES (1,'s')",
            "INSERT INTO video (id, name) VALUES (1,'v')",
            "INSERT INTO video (id, name) VALUES (2,'gone')",
            "INSERT INTO page (video_id, cid, pid, name) VALUES (1, 100, 1, 'ok')",
            "INSERT INTO page (video_id, cid, pid, name) VALUES (999, 200, 1, 'orphan')",
            "INSERT INTO task_queue (task_type, task_data, status) VALUES ('add_video_source','{}','completed')",
            "INSERT INTO task_queue (task_type, task_data, status) VALUES ('add_video_source','{}','pending')",
            "INSERT INTO ai_conversation_history (source_key, role, content) VALUES ('k','u','c')",
            "INSERT INTO image_proxy_cache (cache_key, url) VALUES ('k','u')",
        ] {
            db.execute_unprepared(sql).await.expect("插入失败");
        }

        let resp = run_maintenance(&db, DatabaseMaintenanceAction::CleanOrphans)
            .await
            .expect("清理孤立记录失败");
        assert!(resp.success);
        assert_eq!(resp.removed_rows, Some(2)); // 孤儿 youtube_video + 孤儿 page
        async fn count(db: &DatabaseConnection, sql: &str) -> i64 {
            let backend = db.get_database_backend();
            let row = db
                .query_one(Statement::from_string(backend, sql.to_string()))
                .await
                .unwrap()
                .unwrap();
            row.try_get_by_index::<i64>(0).unwrap()
        }
        assert_eq!(count(&db, "SELECT COUNT(*) FROM you_tube_video").await, 1);
        assert_eq!(count(&db, "SELECT COUNT(*) FROM page").await, 1);

        let resp = run_maintenance(&db, DatabaseMaintenanceAction::ClearQueueHistory)
            .await
            .expect("清理队列失败");
        assert_eq!(resp.removed_rows, Some(1));
        assert_eq!(count(&db, "SELECT COUNT(*) FROM task_queue").await, 1);

        let resp = run_maintenance(&db, DatabaseMaintenanceAction::ClearAiHistory)
            .await
            .expect("清理 AI 历史失败");
        assert_eq!(resp.removed_rows, Some(1));

        let resp = run_maintenance(&db, DatabaseMaintenanceAction::ClearImageCache)
            .await
            .expect("清理图片缓存失败");
        assert_eq!(resp.removed_rows, Some(1));

        let resp = run_maintenance(&db, DatabaseMaintenanceAction::Vacuum)
            .await
            .expect("VACUUM 失败");
        assert!(resp.success);
    }
}

// ===== 备份 / 恢复 =====

/// 列出配置目录下的数据库备份文件（data-backup-*.sqlite），按时间倒序。
pub async fn list_backups() -> DatabaseBackupListResponse {
    let mut backups = Vec::new();
    let Ok(mut entries) = tokio::fs::read_dir(&*CONFIG_DIR).await else {
        return DatabaseBackupListResponse::default();
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with("data-backup-") || !name.ends_with(".sqlite") {
            continue;
        }
        let size_bytes = entry
            .metadata()
            .await
            .map(|meta| meta.len())
            .unwrap_or(0);
        let created_at = if let Some(parsed) = backup_timestamp(&name) {
            parsed
        } else if let Ok(meta) = entry.metadata().await {
            meta.modified()
                .ok()
                .map(|time| {
                    chrono::DateTime::<chrono::Local>::from(time)
                        .format("%Y-%m-%d %H:%M:%S")
                        .to_string()
                })
                .unwrap_or_default()
        } else {
            String::new()
        };
        backups.push(DatabaseBackupInfo {
            name,
            path: entry.path().display().to_string(),
            size_bytes,
            created_at,
        });
    }
    backups.sort_by(|left, right| right.created_at.cmp(&left.created_at));
    DatabaseBackupListResponse { backups }
}

/// 从备份文件名解析创建时间：data-backup-YYYYMMDD-HHMMSS.sqlite
fn backup_timestamp(name: &str) -> Option<String> {
    let stem = name.strip_prefix("data-backup-")?.strip_suffix(".sqlite")?;
    chrono::NaiveDateTime::parse_from_str(stem, "%Y%m%d-%H%M%S")
        .ok()
        .map(|time| time.format("%Y-%m-%d %H:%M:%S").to_string())
}

/// 校验备份文件确实是可用的 bili-sync SQLite 数据库。
async fn validate_backup_is_sqlite(path: &std::path::Path) -> Result<()> {
    use sea_orm::sqlx::sqlite::SqliteConnectOptions;
    use sea_orm::sqlx::SqlitePool;
    use sea_orm::SqlxSqliteConnector;

    let options = SqliteConnectOptions::new()
        .filename(path)
        .read_only(true)
        .create_if_missing(false);
    let pool = SqlitePool::connect_with(options)
        .await
        .with_context(|| format!("无法打开备份文件（不是有效的 SQLite 数据库）：{}", path.display()))?;
    let conn = SqlxSqliteConnector::from_sqlx_sqlite_pool(pool.clone());
    for table in ["video", "video_source", "config_items", "seaql_migrations"] {
        let sql = format!(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='{}'",
            table.replace('\'', "''")
        );
        let count = conn
            .query_one(Statement::from_string(conn.get_database_backend(), sql))
            .await?
            .and_then(|row| row.try_get_by_index::<i64>(0).ok())
            .unwrap_or(0);
        if count < 1 {
            bail!("备份文件缺少必需的表 {table}，可能不是本程序的数据库备份");
        }
    }
    pool.close().await;
    Ok(())
}

/// 安排数据库恢复：校验备份 → 复制为暂存文件 → 写“重启后生效”标记。
pub async fn restore_backup(backup_file: &str) -> Result<DatabaseRestoreResponse> {
    let name = backup_file.trim();
    stage_restore_at(&CONFIG_DIR, name).await?;
    info!(backup = name, "数据库恢复已安排：重启后生效");
    Ok(DatabaseRestoreResponse {
        success: true,
        message: format!("已安排恢复「{name}」，重启 bili-sync 后生效"),
        backup_name: Some(name.to_string()),
        restart_required: true,
    })
}

/// `restore_backup` 的可测试实现：目标目录通过参数注入。
async fn stage_restore_at(config_dir: &std::path::Path, name: &str) -> Result<()> {
    if name.is_empty()
        || name.contains(['/', '\\'])
        || name.contains("..")
        || !name.starts_with("data-backup-")
        || !name.ends_with(".sqlite")
    {
        bail!("无效的备份文件名：{name}");
    }
    let source = config_dir.join(name);
    if !source.is_file() {
        bail!("备份文件不存在：{}", source.display());
    }
    validate_backup_is_sqlite(&source).await?;

    let staged = config_dir.join("data.restore.sqlite");
    let marker = config_dir.join("data.restore.marker");
    let _ = tokio::fs::remove_file(&staged).await;
    tokio::fs::copy(&source, &staged)
        .await
        .with_context(|| format!("复制备份文件失败：{}", source.display()))?;
    tokio::fs::write(&marker, name.as_bytes())
        .await
        .with_context(|| "写入恢复标记失败".to_string())?;
    Ok(())
}
