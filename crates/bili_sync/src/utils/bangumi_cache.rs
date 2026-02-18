use anyhow::Result;
use chrono::{DateTime, Utc};
use sea_orm::sea_query::{Alias, ColumnDef, Table};
use sea_orm::{ConnectionTrait, DbConn};
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info};

/// 番剧缓存数据结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BangumiCache {
    pub season_info: serde_json::Value,
    pub episodes: Vec<serde_json::Value>,
    pub last_episode_time: Option<DateTime<Utc>>,
    pub total_episodes: usize,
}

/// 创建番剧缓存相关的数据库字段
pub async fn ensure_cache_columns(db: &DbConn) -> Result<()> {
    let backend = db.get_database_backend();

    // 检查是否已有缓存字段
    let check_sql = match backend {
        sea_orm::DatabaseBackend::Sqlite => {
            "SELECT COUNT(*) FROM pragma_table_info('video_source') WHERE name IN ('cached_episodes', 'cache_updated_at')"
        }
        _ => {
            // 其他数据库暂不支持
            return Ok(());
        }
    };

    let result: Option<i32> = db
        .query_one(sea_orm::Statement::from_string(backend, check_sql))
        .await?
        .and_then(|row| row.try_get_by_index(0).ok());

    // 如果字段已存在，跳过
    if let Some(count) = result {
        if count >= 2 {
            debug!("番剧缓存字段已存在，跳过创建");
            return Ok(());
        }
    }

    // 添加 cached_episodes 字段
    let add_cached_episodes = Table::alter()
        .table(Alias::new("video_source"))
        .add_column_if_not_exists(ColumnDef::new(Alias::new("cached_episodes")).text().null())
        .to_owned();

    // 添加 cache_updated_at 字段
    let add_cache_updated_at = Table::alter()
        .table(Alias::new("video_source"))
        .add_column_if_not_exists(ColumnDef::new(Alias::new("cache_updated_at")).date_time().null())
        .to_owned();

    // 执行迁移
    match db.execute(backend.build(&add_cached_episodes)).await {
        Ok(_) => info!("成功添加 cached_episodes 字段"),
        Err(e) => {
            if !e.to_string().contains("duplicate column") {
                error!("添加 cached_episodes 字段失败: {}", e);
                return Err(e.into());
            }
        }
    }

    match db.execute(backend.build(&add_cache_updated_at)).await {
        Ok(_) => info!("成功添加 cache_updated_at 字段"),
        Err(e) => {
            if !e.to_string().contains("duplicate column") {
                error!("添加 cache_updated_at 字段失败: {}", e);
                return Err(e.into());
            }
        }
    }

    info!("番剧缓存数据库迁移完成");
    Ok(())
}

/// 解析缓存的剧集数据
pub fn parse_cache(cached_data: &str) -> Result<BangumiCache> {
    serde_json::from_str(cached_data).map_err(|e| anyhow::anyhow!("解析缓存失败: {}", e))
}

/// 序列化缓存数据
pub fn serialize_cache(cache: &BangumiCache) -> Result<String> {
    serde_json::to_string(cache).map_err(|e| anyhow::anyhow!("序列化缓存失败: {}", e))
}

/// 检查缓存是否过期（默认24小时）
pub fn is_cache_expired(cache_updated_at: Option<DateTime<Utc>>, max_age_hours: i64) -> bool {
    match cache_updated_at {
        Some(updated_at) => {
            let now = crate::utils::time_format::beijing_now();
            let age = now.signed_duration_since(updated_at);
            age.num_hours() > max_age_hours
        }
        None => true, // 没有缓存时间，视为过期
    }
}
