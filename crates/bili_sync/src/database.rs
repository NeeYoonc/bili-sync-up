use anyhow::Result;
use bili_sync_migration::{Migrator, MigratorTrait};
use sea_orm::sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sea_orm::sqlx::{self, Executor};
use sea_orm::{DatabaseConnection, SqlxSqliteConnector};
use std::sync::Arc;
use tokio::sync::OnceCell;
use tracing::debug;

use crate::config::CONFIG_DIR;

static GLOBAL_DB: OnceCell<Arc<DatabaseConnection>> = OnceCell::const_new();

fn database_path() -> std::path::PathBuf {
    // 确保配置目录存在
    if !CONFIG_DIR.exists() {
        std::fs::create_dir_all(&*CONFIG_DIR).expect("创建配置目录失败");
    }
    CONFIG_DIR.join("data.sqlite")
}

/// 创建 SQLite 连接选项（带所有优化配置）
fn create_sqlite_options() -> SqliteConnectOptions {
    SqliteConnectOptions::new()
        .filename(database_path())
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(std::time::Duration::from_secs(90))  // 与上游一致
        .optimize_on_close(true, None)  // 连接关闭时自动优化查询统计
        .pragma("cache_size", "-65536")
        .pragma("temp_store", "MEMORY")
        .pragma("mmap_size", "1073741824")
        .pragma("wal_autocheckpoint", "1000")
}

async fn database_connection() -> Result<DatabaseConnection> {
    // 创建连接池，使用 after_connect 回调确保每个连接都执行额外的PRAGMA
    let pool = SqlitePoolOptions::new()
        .max_connections(50)  // 与上游一致
        .min_connections(5)   // 与上游一致
        .acquire_timeout(std::time::Duration::from_secs(90))  // 与上游一致
        .idle_timeout(std::time::Duration::from_secs(600))
        .max_lifetime(std::time::Duration::from_secs(3600))
        .after_connect(|conn, _meta| {
            Box::pin(async move {
                // 每个新连接都执行这些PRAGMA，确保设置生效
                conn.execute("PRAGMA busy_timeout = 90000;").await?;
                conn.execute("PRAGMA journal_mode = WAL;").await?;
                conn.execute("PRAGMA synchronous = NORMAL;").await?;
                conn.execute("PRAGMA optimize;").await?;

                // 用 DEBUG 级别验证设置是否生效
                let row: (i64,) = sqlx::query_as("PRAGMA busy_timeout;")
                    .fetch_one(&mut *conn)
                    .await?;
                tracing::debug!("新数据库连接已创建，busy_timeout = {}ms", row.0);

                Ok(())
            })
        })
        .connect_with(create_sqlite_options())
        .await?;

    // 立即验证连接池中的连接配置
    {
        let mut conn = pool.acquire().await?;
        let row: (i64,) = sqlx::query_as("PRAGMA busy_timeout;").fetch_one(&mut *conn).await?;
        tracing::debug!("验证连接池 busy_timeout = {}ms", row.0);
    }

    // 转换为 SeaORM 的 DatabaseConnection
    let connection = SqlxSqliteConnector::from_sqlx_sqlite_pool(pool);

    debug!("SQLite 连接池已创建，20个连接，每个都应用了 WAL模式、60秒busy_timeout、64MB缓存、1GB mmap");

    Ok(connection)
}

async fn migrate_database() -> Result<()> {
    // 检查数据库文件是否存在，不存在则会在连接时自动创建
    let db_path = CONFIG_DIR.join("data.sqlite");
    debug!("数据库文件路径: {}", db_path.display());
    if !db_path.exists() {
        debug!("数据库文件不存在，将创建新的数据库");
    } else {
        debug!("检测到现有数据库文件，将在必要时应用迁移");
    }

    // 为迁移创建单连接池（避免多连接导致的迁移顺序问题）
    // 同样应用 busy_timeout 等优化配置
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(create_sqlite_options())
        .await?;

    let connection = SqlxSqliteConnector::from_sqlx_sqlite_pool(pool.clone());

    // 确保所有迁移都应用
    Migrator::up(&connection, None).await?;

    // 显式关闭连接池，确保释放所有数据库锁
    pool.close().await;
    debug!("迁移完成，已关闭迁移连接池");

    Ok(())
}

/// 确保 page 表有 ai_renamed 字段
async fn ensure_ai_renamed_column(connection: &DatabaseConnection) -> Result<()> {
    use sea_orm::ConnectionTrait;

    let backend = connection.get_database_backend();

    // 检查是否已有 ai_renamed 字段
    let check_sql = "SELECT COUNT(*) FROM pragma_table_info('page') WHERE name = 'ai_renamed'";
    let result: Option<i32> = connection
        .query_one(sea_orm::Statement::from_string(backend, check_sql))
        .await?
        .and_then(|row| row.try_get_by_index(0).ok());

    if let Some(count) = result {
        if count >= 1 {
            debug!("page.ai_renamed 字段已存在");
            return Ok(());
        }
    }

    // 添加 ai_renamed 字段
    let add_sql = "ALTER TABLE page ADD COLUMN ai_renamed INTEGER DEFAULT 0";
    match connection
        .execute(sea_orm::Statement::from_string(backend, add_sql))
        .await
    {
        Ok(_) => info!("成功添加 page.ai_renamed 字段"),
        Err(e) => {
            if !e.to_string().contains("duplicate column") {
                return Err(e.into());
            }
        }
    }

    Ok(())
}

/// 预热数据库，将关键数据加载到内存映射中
async fn preheat_database(connection: &DatabaseConnection) -> Result<()> {
    use sea_orm::ConnectionTrait;

    // 预热关键表，触发内存映射加载
    let tables = vec![
        "video",
        "page",
        "collection",
        "favorite",
        "submission",
        "watch_later",
        "video_source",
    ];

    for table in tables {
        match connection
            .execute_unprepared(&format!("SELECT COUNT(*) FROM {}", table))
            .await
        {
            Ok(result) => {
                debug!("预热表 {} 完成，行数: {:?}", table, result.rows_affected());
            }
            Err(e) => {
                debug!("预热表 {} 失败（可能不存在）: {}", table, e);
            }
        }
    }

    // 触发索引加载
    let _ = connection
        .execute_unprepared("SELECT * FROM video WHERE id > 0 LIMIT 1")
        .await;
    let _ = connection
        .execute_unprepared("SELECT * FROM page WHERE id > 0 LIMIT 1")
        .await;

    debug!("数据库预热完成，关键数据已加载到内存映射");
    Ok(())
}

/// 进行数据库迁移并获取数据库连接，供外部使用
pub async fn setup_database() -> DatabaseConnection {
    migrate_database().await.expect("数据库迁移失败");
    let connection = database_connection().await.expect("获取数据库连接失败");

    // 执行番剧缓存相关的数据库迁移
    if let Err(e) = crate::utils::bangumi_cache::ensure_cache_columns(&connection).await {
        tracing::warn!("番剧缓存数据库迁移失败: {}", e);
    }

    // 添加 page.ai_renamed 字段
    if let Err(e) = ensure_ai_renamed_column(&connection).await {
        tracing::warn!("添加 ai_renamed 字段失败: {}", e);
    }

    // 预热数据库，加载热数据到内存映射
    if let Err(e) = preheat_database(&connection).await {
        tracing::warn!("数据库预热失败: {}", e);
    }

    // 设置全局数据库引用
    let connection_arc = Arc::new(connection.clone());
    let _ = GLOBAL_DB.set(connection_arc);

    connection
}

/// 获取全局数据库连接
pub fn get_global_db() -> Option<Arc<DatabaseConnection>> {
    GLOBAL_DB.get().cloned()
}

/// 开始一个事务并立即获取写锁
/// 通过更新锁定表来强制获取写锁，避免 SQLITE_BUSY_SNAPSHOT 问题
pub async fn begin_write_transaction(
    connection: &sea_orm::DatabaseConnection,
) -> anyhow::Result<sea_orm::DatabaseTransaction> {
    use sea_orm::{ConnectionTrait, TransactionTrait};

    // 确保锁定表存在
    let _ = connection
        .execute_unprepared("CREATE TABLE IF NOT EXISTS _write_lock (id INTEGER PRIMARY KEY, ts INTEGER)")
        .await;
    let _ = connection
        .execute_unprepared("INSERT OR IGNORE INTO _write_lock (id, ts) VALUES (1, 0)")
        .await;

    // 开始事务
    let txn = connection.begin().await?;

    // 立即更新锁定表，强制获取写锁
    // 如果其他事务持有锁，这里会等待 busy_timeout
    txn.execute_unprepared("UPDATE _write_lock SET ts = strftime('%s', 'now') WHERE id = 1")
        .await?;

    Ok(txn)
}
