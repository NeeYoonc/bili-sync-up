use anyhow::Result;
use bili_sync_migration::{Migrator, MigratorTrait};
use sea_orm::sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sea_orm::sqlx::{self, Executor};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, DbErr, ExecResult, QueryResult, SqlxSqliteConnector, Statement, StreamTrait};
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;
use tokio::sync::OnceCell;
use tracing::{debug, info, warn};

use crate::config::CONFIG_DIR;

static GLOBAL_DB: OnceCell<Arc<DatabaseConnection>> = OnceCell::const_new();
static ACTIVE_DB_OPERATIONS: LazyLock<Mutex<HashMap<u64, ActiveDbOperation>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static NEXT_DB_OPERATION_ID: AtomicU64 = AtomicU64::new(1);

const SLOW_DB_OPERATION_WARN_MS: u128 = 5_000;

#[derive(Clone, Debug)]
struct ActiveDbOperation {
    name: String,
    started_at: Instant,
}

struct DbOperationGuard {
    id: u64,
    name: String,
    started_at: Instant,
    finished: bool,
}

pub struct TracedDatabaseTransaction {
    txn: sea_orm::DatabaseTransaction,
    guard: Option<DbOperationGuard>,
}

fn is_database_locked_message(message: &str) -> bool {
    message.contains("database is locked") || message.contains("Database is locked")
}

fn active_db_operation_snapshot(exclude_id: Option<u64>) -> Vec<String> {
    let now = Instant::now();
    let operations = ACTIVE_DB_OPERATIONS
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut snapshot = operations
        .iter()
        .filter(|(id, _)| Some(**id) != exclude_id)
        .map(|(id, op)| format!("#{} {} ({}ms)", id, op.name, now.duration_since(op.started_at).as_millis()))
        .collect::<Vec<_>>();
    snapshot.sort();
    snapshot
}

impl DbOperationGuard {
    fn begin(name: impl Into<String>) -> Self {
        let id = NEXT_DB_OPERATION_ID.fetch_add(1, Ordering::Relaxed);
        let name = name.into();
        let started_at = Instant::now();

        ACTIVE_DB_OPERATIONS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(
                id,
                ActiveDbOperation {
                    name: name.clone(),
                    started_at,
                },
            );

        let occupied = active_db_operation_snapshot(Some(id));
        if !occupied.is_empty() {
            warn!(
                "数据库操作开始时检测到 {} 个进行中的数据库操作，current={}, active=[{}]",
                occupied.len(),
                name,
                occupied.join(" | ")
            );
        } else {
            debug!("数据库操作开始: {}", name);
        }

        Self {
            id,
            name,
            started_at,
            finished: false,
        }
    }

    fn on_error<E: std::fmt::Display>(&self, stage: &str, error: &E) {
        let error_text = error.to_string();
        if is_database_locked_message(&error_text) {
            let occupied = active_db_operation_snapshot(Some(self.id));
            warn!(
                "数据库操作遇到锁等待/冲突: op={}, stage={}, elapsed={}ms, active=[{}], error={}",
                self.name,
                stage,
                self.started_at.elapsed().as_millis(),
                if occupied.is_empty() {
                    "none".to_string()
                } else {
                    occupied.join(" | ")
                },
                error_text
            );
        } else {
            warn!(
                "数据库操作失败: op={}, stage={}, elapsed={}ms, error={}",
                self.name,
                stage,
                self.started_at.elapsed().as_millis(),
                error_text
            );
        }
    }

    fn finish(mut self, stage: &str) {
        self.finished = true;
        ACTIVE_DB_OPERATIONS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);

        let elapsed_ms = self.started_at.elapsed().as_millis();
        if elapsed_ms >= SLOW_DB_OPERATION_WARN_MS {
            warn!(
                "数据库操作结束较慢: op={}, stage={}, elapsed={}ms",
                self.name, stage, elapsed_ms
            );
        } else {
            debug!("数据库操作结束: op={}, stage={}, elapsed={}ms", self.name, stage, elapsed_ms);
        }
    }
}

impl Drop for DbOperationGuard {
    fn drop(&mut self) {
        if self.finished {
            return;
        }

        ACTIVE_DB_OPERATIONS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);

        warn!(
            "数据库操作未显式提交/回滚即离开作用域: op={}, elapsed={}ms",
            self.name,
            self.started_at.elapsed().as_millis()
        );
    }
}

impl TracedDatabaseTransaction {
    fn new(txn: sea_orm::DatabaseTransaction, guard: DbOperationGuard) -> Self {
        Self {
            txn,
            guard: Some(guard),
        }
    }

    pub async fn commit(mut self) -> anyhow::Result<()> {
        match self.txn.commit().await {
            Ok(_) => {
                if let Some(guard) = self.guard.take() {
                    guard.finish("commit");
                }
                Ok(())
            }
            Err(error) => {
                if let Some(guard) = self.guard.as_ref() {
                    guard.on_error("commit", &error);
                }
                Err(error.into())
            }
        }
    }

    #[allow(dead_code)]
    pub async fn rollback(mut self) -> anyhow::Result<()> {
        match self.txn.rollback().await {
            Ok(_) => {
                if let Some(guard) = self.guard.take() {
                    guard.finish("rollback");
                }
                Ok(())
            }
            Err(error) => {
                if let Some(guard) = self.guard.as_ref() {
                    guard.on_error("rollback", &error);
                }
                Err(error.into())
            }
        }
    }
}

impl Deref for TracedDatabaseTransaction {
    type Target = sea_orm::DatabaseTransaction;

    fn deref(&self) -> &Self::Target {
        &self.txn
    }
}

impl DerefMut for TracedDatabaseTransaction {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.txn
    }
}

#[async_trait::async_trait]
impl ConnectionTrait for TracedDatabaseTransaction {
    fn get_database_backend(&self) -> DbBackend {
        self.txn.get_database_backend()
    }

    async fn execute(&self, stmt: Statement) -> Result<ExecResult, DbErr> {
        self.txn.execute(stmt).await
    }

    async fn execute_unprepared(&self, sql: &str) -> Result<ExecResult, DbErr> {
        self.txn.execute_unprepared(sql).await
    }

    async fn query_one(&self, stmt: Statement) -> Result<Option<QueryResult>, DbErr> {
        self.txn.query_one(stmt).await
    }

    async fn query_all(&self, stmt: Statement) -> Result<Vec<QueryResult>, DbErr> {
        self.txn.query_all(stmt).await
    }

    fn support_returning(&self) -> bool {
        self.txn.support_returning()
    }

    fn is_mock_connection(&self) -> bool {
        self.txn.is_mock_connection()
    }
}

impl StreamTrait for TracedDatabaseTransaction {
    type Stream<'a>
        = <sea_orm::DatabaseTransaction as StreamTrait>::Stream<'a>
    where
        Self: 'a;

    fn stream<'a>(
        &'a self,
        stmt: Statement,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Self::Stream<'a>, DbErr>> + 'a + Send>> {
        self.txn.stream(stmt)
    }
}

pub async fn run_traced_db_operation<F, T, E>(name: impl Into<String>, future: F) -> std::result::Result<T, E>
where
    F: std::future::Future<Output = std::result::Result<T, E>>,
    E: std::fmt::Display,
{
    let guard = DbOperationGuard::begin(name);
    match future.await {
        Ok(result) => {
            guard.finish("complete");
            Ok(result)
        }
        Err(error) => {
            guard.on_error("complete", &error);
            Err(error)
        }
    }
}

pub async fn begin_traced_transaction(
    connection: &sea_orm::DatabaseConnection,
    operation_name: impl Into<String>,
) -> anyhow::Result<TracedDatabaseTransaction> {
    use sea_orm::TransactionTrait;

    let guard = DbOperationGuard::begin(operation_name);
    match connection.begin().await {
        Ok(txn) => Ok(TracedDatabaseTransaction::new(txn, guard)),
        Err(error) => {
            guard.on_error("begin", &error);
            Err(error.into())
        }
    }
}

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
    auto_compact_image_proxy_cache(&connection).await?;

    // 显式关闭连接池，确保释放所有数据库锁
    pool.close().await;
    debug!("迁移完成，已关闭迁移连接池");

    Ok(())
}

/// 确保 page 表有 ai_renamed 字段
async fn auto_compact_image_proxy_cache(connection: &DatabaseConnection) -> Result<()> {
    const MIN_FREE_PAGES_FOR_VACUUM: i64 = 8192; // ~32MB when page_size=4KB
    const MIN_FREE_RATIO_FOR_VACUUM: f64 = 0.20;

    let backend = connection.get_database_backend();
    let table_exists_sql = "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='image_proxy_cache'";
    let table_exists = connection
        .query_one(sea_orm::Statement::from_string(backend, table_exists_sql))
        .await?
        .and_then(|row| row.try_get_by_index::<i64>(0).ok())
        .unwrap_or(0)
        >= 1;
    if !table_exists {
        return Ok(());
    }

    if sqlite_table_has_column(connection, "image_proxy_cache", "image_data").await? {
        connection
            .execute(sea_orm::Statement::from_string(
                backend,
                "UPDATE image_proxy_cache SET image_data = X'' WHERE length(image_data) > 0",
            ))
            .await?;
    }

    let freelist_count = connection
        .query_one(sea_orm::Statement::from_string(backend, "PRAGMA freelist_count"))
        .await?
        .and_then(|row| row.try_get_by_index::<i64>(0).ok())
        .unwrap_or(0);
    let page_count = connection
        .query_one(sea_orm::Statement::from_string(backend, "PRAGMA page_count"))
        .await?
        .and_then(|row| row.try_get_by_index::<i64>(0).ok())
        .unwrap_or(0);
    let page_size = connection
        .query_one(sea_orm::Statement::from_string(backend, "PRAGMA page_size"))
        .await?
        .and_then(|row| row.try_get_by_index::<i64>(0).ok())
        .unwrap_or(4096);

    if page_count <= 0 {
        return Ok(());
    }

    let free_ratio = freelist_count as f64 / page_count as f64;
    let reclaimable_mb = freelist_count.saturating_mul(page_size) / (1024 * 1024);
    if freelist_count < MIN_FREE_PAGES_FOR_VACUUM && free_ratio < MIN_FREE_RATIO_FOR_VACUUM {
        debug!(
            "跳过自动VACUUM: freelist_count={}, page_count={}, free_ratio={:.2}%, reclaimable={}MB",
            freelist_count,
            page_count,
            free_ratio * 100.0,
            reclaimable_mb
        );
        return Ok(());
    }

    info!(
        "触发自动VACUUM: freelist_count={}, page_count={}, free_ratio={:.2}%, reclaimable={}MB",
        freelist_count,
        page_count,
        free_ratio * 100.0,
        reclaimable_mb
    );

    if let Err(e) = connection.execute_unprepared("PRAGMA wal_checkpoint(TRUNCATE)").await {
        debug!("自动VACUUM前WAL checkpoint失败（继续尝试VACUUM）: {}", e);
    }

    if let Err(e) = connection.execute_unprepared("VACUUM").await {
        warn!("自动VACUUM执行失败（不影响启动）: {}", e);
        return Ok(());
    }

    info!("自动VACUUM执行完成");
    Ok(())
}

async fn sqlite_table_has_column(connection: &DatabaseConnection, table_name: &str, column_name: &str) -> Result<bool> {
    let backend = connection.get_database_backend();
    let sql = format!(
        "SELECT COUNT(*) FROM pragma_table_info('{}') WHERE name = '{}'",
        table_name.replace('\'', "''"),
        column_name.replace('\'', "''")
    );
    let count = connection
        .query_one(sea_orm::Statement::from_string(backend, sql))
        .await?
        .and_then(|row| row.try_get_by_index::<i64>(0).ok())
        .unwrap_or(0);
    Ok(count >= 1)
}

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
    operation_name: impl Into<String>,
) -> anyhow::Result<TracedDatabaseTransaction> {
    use sea_orm::{ConnectionTrait, TransactionTrait};

    let operation_name = operation_name.into();
    let guard = DbOperationGuard::begin(format!("{} [write-lock]", operation_name));

    // 确保锁定表存在
    let _ = connection
        .execute_unprepared("CREATE TABLE IF NOT EXISTS _write_lock (id INTEGER PRIMARY KEY, ts INTEGER)")
        .await;
    let _ = connection
        .execute_unprepared("INSERT OR IGNORE INTO _write_lock (id, ts) VALUES (1, 0)")
        .await;

    // 开始事务
    let txn = match connection.begin().await {
        Ok(txn) => txn,
        Err(error) => {
            guard.on_error("begin", &error);
            return Err(error.into());
        }
    };

    // 立即更新锁定表，强制获取写锁
    // 如果其他事务持有锁，这里会等待 busy_timeout
    if let Err(error) = txn
        .execute_unprepared("UPDATE _write_lock SET ts = strftime('%s', 'now') WHERE id = 1")
        .await
    {
        guard.on_error("acquire_write_lock", &error);
        return Err(error.into());
    }

    Ok(TracedDatabaseTransaction::new(txn, guard))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{self, Write};
    use std::path::Path;
    use std::sync::{Arc, Mutex as StdMutex};
    use tracing::Level;
    use tracing_subscriber::fmt::MakeWriter;
    use uuid::Uuid;

    #[derive(Clone, Default)]
    struct SharedLogBuffer(Arc<StdMutex<Vec<u8>>>);

    struct SharedLogWriter(Arc<StdMutex<Vec<u8>>>);

    impl<'a> MakeWriter<'a> for SharedLogBuffer {
        type Writer = SharedLogWriter;

        fn make_writer(&'a self) -> Self::Writer {
            SharedLogWriter(self.0.clone())
        }
    }

    impl Write for SharedLogWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl SharedLogBuffer {
        fn contents(&self) -> String {
            String::from_utf8(
                self.0
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone(),
            )
            .unwrap_or_default()
        }
    }

    async fn create_test_connection(path: &Path, busy_timeout_ms: u64) -> anyhow::Result<DatabaseConnection> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .busy_timeout(std::time::Duration::from_millis(busy_timeout_ms));

        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;

        Ok(SqlxSqliteConnector::from_sqlx_sqlite_pool(pool))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn traced_db_operation_logs_active_lock_holder_on_sqlite_lock() -> anyhow::Result<()> {
        ACTIVE_DB_OPERATIONS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();

        let logs = SharedLogBuffer::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_ansi(false)
            .without_time()
            .with_max_level(Level::DEBUG)
            .finish();
        let _subscriber_guard = tracing::subscriber::set_default(subscriber);

        let db_path = std::env::temp_dir().join(format!("bili-sync-db-trace-{}.sqlite", Uuid::new_v4()));
        let connection_1 = create_test_connection(&db_path, 50).await?;
        let connection_2 = create_test_connection(&db_path, 50).await?;

        connection_1
            .execute_unprepared("CREATE TABLE test_records (id INTEGER PRIMARY KEY, value TEXT)")
            .await?;

        let lock_holder = begin_write_transaction(&connection_1, "test.lock_holder").await?;

        let err = run_traced_db_operation("test.contender", async {
            connection_2
                .execute_unprepared("INSERT INTO test_records (value) VALUES ('contender')")
                .await
        })
        .await
        .expect_err("第二个写操作应当被锁住");

        assert!(
            is_database_locked_message(&err.to_string()),
            "expected sqlite lock error, got: {err}"
        );

        lock_holder.rollback().await?;

        let log_output = logs.contents();
        assert!(
            log_output.contains("数据库操作开始时检测到"),
            "expected active operation start warning, got logs: {log_output}"
        );
        assert!(
            log_output.contains("数据库操作遇到锁等待/冲突"),
            "expected lock conflict warning, got logs: {log_output}"
        );
        assert!(
            log_output.contains("test.lock_holder [write-lock]"),
            "expected lock holder name in logs, got logs: {log_output}"
        );
        assert!(
            log_output.contains("test.contender"),
            "expected contender name in logs, got logs: {log_output}"
        );

        drop(connection_2);
        drop(connection_1);
        let _ = std::fs::remove_file(&db_path);

        Ok(())
    }
}
