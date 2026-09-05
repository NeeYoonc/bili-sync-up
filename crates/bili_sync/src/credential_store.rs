//! 外部平台（YouTube/抖音/TikTok）登录凭证的数据库存储。
//!
//! 与 B 站凭证（`config_items.credential`）同思路：凭证入库而不是散落在配置目录的
//! 单独文件里。相对 B 站凭证，这里额外做三件事：
//!
//! - 独立表 `external_credentials`：凭证不走 `config_changes` 历史、不进入配置加载
//!   流程与 debug 日志，避免敏感数据泄露和数据库膨胀；
//! - 进程内缓存：`get` 是同步读取，兼容现有大量同步读取点（签名、请求拼装等）；
//! - 影子文件桥：yt-dlp 的 `--cookies`、Node 签名器必须传真实文件路径，这类场景
//!   会把数据库里的内容同步到系统临时目录（`%TEMP%/bili-sync-external/`），用完即
//!   可丢弃，不作为持久化凭证保存。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result};
use sea_orm::{ConnectionTrait, DatabaseConnection, Statement};
use tracing::{debug, info, warn};

use crate::database::get_global_db;

/// 外部平台凭证的数据库键名。
pub mod keys {
    /// YouTube Netscape cookies.txt 全文
    pub const YOUTUBE_COOKIES: &str = "youtube.cookies";
    /// 抖音 Netscape cookies.txt 全文
    pub const DOUYIN_COOKIES: &str = "douyin.cookies";
    /// 抖音 secsdk 签名会话（localStorage + ua + href，JSON 序列化）
    pub const DOUYIN_SECSDK: &str = "douyin.secsdk";
    /// 抖音 msToken（程序补发或导入）
    pub const DOUYIN_MSTOKEN: &str = "douyin.mstoken";
    /// 抖音 webid 设备标识
    pub const DOUYIN_WEBID: &str = "douyin.webid";
    /// 抖音 verify_fp 设备指纹
    pub const DOUYIN_VERIFY_FP: &str = "douyin.verify_fp";
    /// TikTok Netscape cookies.txt 全文
    pub const TIKTOK_COOKIES: &str = "tiktok.cookies";
    /// TikTok localStorage（webmssdk 签名会话，JSON 序列化）
    pub const TIKTOK_LOCALSTORAGE: &str = "tiktok.localstorage";
    /// TikTok 手动设置的账号 secUid
    pub const TIKTOK_SECUID: &str = "tiktok.secuid";
    /// TikTok 手动设置的设备 ID（webid）
    pub const TIKTOK_WEBID: &str = "tiktok.webid";

    /// 所有平台凭证键，供启动加载与迁移扫描使用。
    pub const ALL: &[&str] = &[
        YOUTUBE_COOKIES,
        DOUYIN_COOKIES,
        DOUYIN_SECSDK,
        DOUYIN_MSTOKEN,
        DOUYIN_WEBID,
        DOUYIN_VERIFY_FP,
        TIKTOK_COOKIES,
        TIKTOK_LOCALSTORAGE,
        TIKTOK_SECUID,
        TIKTOK_WEBID,
    ];
}

struct CredentialEntry {
    value: String,
    updated_at_unix: i64,
}

static CACHE: OnceLock<Mutex<HashMap<String, CredentialEntry>>> = OnceLock::new();

fn cache() -> &'static Mutex<HashMap<String, CredentialEntry>> {
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now_standard() -> String {
    crate::utils::time_format::now_standard_string()
}

/// 确保外部凭证表存在。
pub async fn ensure_table(db: &DatabaseConnection) -> Result<()> {
    db.execute_unprepared(
        "CREATE TABLE IF NOT EXISTS external_credentials (
            key TEXT PRIMARY KEY,
            value_json TEXT NOT NULL,
            updated_at TEXT NOT NULL
        )",
    )
    .await
    .context("创建 external_credentials 表失败")?;
    Ok(())
}

/// 启动时把数据库里的全部外部凭证加载进内存缓存。
pub async fn init(db: &DatabaseConnection) -> Result<()> {
    ensure_table(db).await?;
    let rows = db
        .query_all(Statement::from_string(
            db.get_database_backend(),
            "SELECT key, value_json, updated_at FROM external_credentials",
        ))
        .await
        .context("读取 external_credentials 失败")?;
    let mut map = cache().lock().unwrap_or_else(|e| e.into_inner());
    map.clear();
    let mut loaded = 0usize;
    for row in rows {
        let key: String = row.try_get("", "key")?;
        let value_json: String = row.try_get("", "value_json")?;
        let updated_at_text: String = row.try_get("", "updated_at")?;
        let updated_at_unix = chrono::NaiveDateTime::parse_from_str(&updated_at_text, "%Y-%m-%d %H:%M:%S")
            .map(|naive| naive.and_utc().timestamp() - 8 * 3600)
            .unwrap_or_else(|_| chrono::Utc::now().timestamp());
        if let Ok(serde_json::Value::String(value)) = serde_json::from_str(&value_json) {
            map.insert(key, CredentialEntry { value, updated_at_unix });
            loaded += 1;
        } else {
            warn!(key, "外部凭证条目格式异常，已忽略");
        }
    }
    drop(map);
    debug!(loaded, "外部平台凭证已加载到内存缓存");
    Ok(())
}

/// 同步读取某个外部凭证（无则返回 None）。
pub fn get(key: &str) -> Option<String> {
    cache().lock().unwrap_or_else(|e| e.into_inner()).get(key).map(|entry| entry.value.clone())
}

/// 某个外部凭证最近写入时间的 unix 秒；未缓存时返回 None。
pub fn updated_at(key: &str) -> Option<i64> {
    cache().lock().unwrap_or_else(|e| e.into_inner()).get(key).map(|entry| entry.updated_at_unix)
}

/// 同步判断某个外部凭证是否存在。
pub fn has(key: &str) -> bool {
    get(key).is_some_and(|value| !value.trim().is_empty())
}

/// 写入外部凭证：更新内存缓存、写数据库、同步影子文件。
pub async fn set(key: &str, value: &str) -> Result<()> {
    let encoded = serde_json::to_string(value).context("序列化外部凭证失败")?;
    let db = get_global_db().context("数据库尚未就绪")?;
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO external_credentials (key, value_json, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT(key) DO UPDATE SET value_json = excluded.value_json, updated_at = excluded.updated_at",
        vec![key.into(), encoded.into(), now_standard().into()],
    );
    db.execute(statement).await.context("写入外部凭证失败")?;
    cache().lock().unwrap_or_else(|e| e.into_inner()).insert(
        key.to_string(),
        CredentialEntry {
            value: value.to_string(),
            updated_at_unix: chrono::Utc::now().timestamp(),
        },
    );
    if let Err(error) = sync_shadow(key, value) {
        warn!(key, error = %error, "同步外部凭证影子文件失败（不影响数据库凭证）");
    }
    Ok(())
}

/// 删除某个外部凭证（内存缓存 + 数据库 + 影子文件）。
pub async fn delete(key: &str) -> Result<()> {
    let db = get_global_db().context("数据库尚未就绪")?;
    let statement = Statement::from_sql_and_values(
        db.get_database_backend(),
        "DELETE FROM external_credentials WHERE key = ?",
        vec![key.into()],
    );
    db.execute(statement).await.context("删除外部凭证失败")?;
    cache().lock().unwrap_or_else(|e| e.into_inner()).remove(key);
    let _ = std::fs::remove_file(shadow_path(key));
    Ok(())
}

/// 外部凭证影子文件目录（系统临时目录，不作为持久化凭证）。
fn shadow_dir() -> PathBuf {
    std::env::temp_dir().join("bili-sync-external")
}

/// 某个凭证对应的影子文件路径。
pub fn shadow_path(key: &str) -> PathBuf {
    let name = key.replace('.', "_").replace('/', "_");
    shadow_dir().join(format!("{name}.txt"))
}

/// 同步把凭证内容写到影子文件（yt-dlp `--cookies` / Node 签名器需要真实路径）。
pub fn sync_shadow(key: &str, value: &str) -> std::io::Result<PathBuf> {
    let path = shadow_path(key);
    std::fs::create_dir_all(path.parent().expect("影子文件必须有父目录"))?;
    std::fs::write(&path, value.as_bytes())?;
    Ok(path)
}

/// 迁移旧版单独文件凭证到数据库：数据库已有同 key 凭证则跳过；文件不存在或
/// 内容不合法则跳过；成功迁移后删除旧文件。
pub async fn migrate_file_to_db(
    key: &str,
    path: &Path,
    is_valid: impl Fn(&str) -> bool,
) -> Result<bool> {
    if has(key) {
        return Ok(false);
    }
    let contents = match tokio::fs::read_to_string(path).await {
        Ok(contents) => contents,
        Err(_) => return Ok(false),
    };
    if !is_valid(&contents) {
        return Ok(false);
    }
    set(key, &contents).await?;
    let _ = tokio::fs::remove_file(path).await;
    info!(key, path = %path.display(), "已将旧版外部凭证文件迁移到数据库并删除");
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_roundtrip() {
        let cache = cache();
        {
            let mut guard = cache.lock().unwrap();
            guard.insert(
                "test.key".to_string(),
                CredentialEntry { value: "value".to_string(), updated_at_unix: 0 },
            );
        }
        assert_eq!(get("test.key").as_deref(), Some("value"));
        assert_eq!(updated_at("test.key"), Some(0));
        {
            let mut guard = cache.lock().unwrap();
            guard.remove("test.key");
        }
    }

    #[test]
    fn shadow_path_is_safe_and_stable() {
        let path = shadow_path("youtube.cookies");
        assert!(path.to_string_lossy().contains("bili-sync-external"));
        assert_eq!(shadow_path("youtube.cookies"), shadow_path("youtube.cookies"));
        assert_ne!(shadow_path("youtube.cookies"), shadow_path("tiktok.cookies"));
    }
}

/// 启动时把旧版单独文件凭证迁移到数据库（YouTube/抖音/TikTok）。
pub async fn migrate_legacy_credentials_on_startup() {
    if let Err(error) = crate::youtube::migrate_legacy_youtube_credentials().await {
        warn!(error = %error, "迁移旧版 YouTube 凭证文件到数据库失败");
    }
    if let Err(error) = crate::douyin::migrate_legacy_douyin_credentials().await {
        warn!(error = %error, "迁移旧版抖音凭证文件到数据库失败");
    }
    if let Err(error) = crate::tiktok::migrate_legacy_tiktok_credentials().await {
        warn!(error = %error, "迁移旧版 TikTok 凭证文件到数据库失败");
    }
}
