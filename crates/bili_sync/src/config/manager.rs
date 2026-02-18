use std::collections::{HashMap, HashSet};

use anyhow::{anyhow, Context, Result};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter, Set, Statement,
};
use serde::Serialize;
use serde_json::{Map, Value};
use tracing::{debug, error, info, warn};

use crate::config::{Config, ConfigBundle};
use crate::utils::time_format::now_standard_string;
use bili_sync_entity::entities::{config_item, prelude::ConfigItem};

/// 配置管理器，负责配置的数据库存储和热重载
#[derive(Clone)]
pub struct ConfigManager {
    db: DatabaseConnection,
}

pub const LATEST_CONFIG_SCHEMA_VERSION: i32 = 1;

#[derive(Debug, Clone, Serialize)]
pub struct ConfigMigrationStatus {
    pub current_version: i32,
    pub latest_version: i32,
    pub pending: bool,
    pub legacy_detected: bool,
    pub last_migrated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfigMigrationReport {
    pub current_version: i32,
    pub target_version: i32,
    pub applied: bool,
    pub dry_run: bool,
    pub legacy_detected: bool,
    pub mapped_keys: Vec<String>,
    pub unmapped_keys: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Default)]
struct LegacyMigrationMeta {
    no_danmaku: bool,
    no_subtitle: bool,
}

fn describe_config_key(key: &str) -> &'static str {
    match key {
        "auth_token" => "管理页访问密钥",
        "bind_address" => "服务监听地址",
        "credential" => "B站登录凭证",
        "filter_option" => "画质与编码过滤",
        "danmaku_option" => "弹幕下载/样式设置",
        "video_name" => "视频命名模板",
        "page_name" => "分页命名模板",
        "multi_page_name" => "多P分页命名模板",
        "bangumi_name" => "番剧分页命名模板",
        "folder_structure" => "目录结构模板",
        "bangumi_folder_name" => "番剧文件夹命名模板",
        "collection_folder_mode" => "合集文件夹模式",
        "collection_unified_name" => "合集统一模式命名模板",
        "interval" => "扫描间隔（秒）",
        "upper_path" => "UP主头像缓存路径",
        "nfo_time_type" => "NFO时间类型（收藏/发布时间）",
        "nfo_config" => "NFO生成配置",
        "concurrent_limit" => "并发/限速/多线程配置",
        "concurrent_limit.download" => "旧版多线程下载配置",
        "time_format" => "时间格式",
        "cdn_sorting" => "CDN优先级排序",
        "submission_risk_control" => "UP主投稿风控配置",
        "submission_scan_strategy" => "UP主投稿源扫描策略（分批/自适应）",
        "scan_deleted_videos" => "扫描已删除视频",
        "enable_aria2_health_check" => "aria2健康检查",
        "enable_aria2_auto_restart" => "aria2自动重启",
        "aria2_health_check_interval" => "aria2健康检查间隔",
        "multi_page_use_season_structure" => "多P目录Season结构",
        "collection_use_season_structure" => "合集目录Season结构",
        "bangumi_use_season_structure" => "番剧目录Season结构",
        "notification" => "通知配置",
        "notifiers" => "旧版通知配置结构",
        "skip_option" => "旧版跳过下载选项",
        "favorite_default_path" => "收藏夹默认路径模板（旧版）",
        "collection_default_path" => "合集默认路径模板（旧版）",
        "submission_default_path" => "投稿默认路径模板（旧版）",
        "version" => "旧版配置版本号",
        "risk_control" => "风控验证配置",
        "ai_rename" => "AI重命名配置",
        _ => "未知/未定义",
    }
}
impl ConfigManager {
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db }
    }

    pub async fn get_config_schema_version(&self) -> Result<i32> {
        if let Some(value) = self.get_config_item("config_schema_version").await? {
            Ok(value.as_i64().unwrap_or(0) as i32)
        } else {
            Ok(0)
        }
    }

    pub async fn set_config_schema_version(&self, version: i32) -> Result<()> {
        self.update_config_item(
            "config_schema_version",
            Value::Number(serde_json::Number::from(version as i64)),
        )
        .await
    }

    pub async fn get_config_schema_status(&self) -> Result<ConfigMigrationStatus> {
        let current_version = self.get_config_schema_version().await?;
        let latest_version = LATEST_CONFIG_SCHEMA_VERSION;
        let legacy_detected = self.legacy_config_exists().await?;
        let pending = current_version < latest_version;
        let last_migrated_at = self
            .get_config_item("config_schema_migrated_at")
            .await?
            .and_then(|v| v.as_str().map(|s| s.to_string()));

        Ok(ConfigMigrationStatus {
            current_version,
            latest_version,
            pending,
            legacy_detected,
            last_migrated_at,
        })
    }

    pub async fn migrate_config_schema(&self, dry_run: bool) -> Result<ConfigMigrationReport> {
        let current_version = self.get_config_schema_version().await?;
        let target_version = LATEST_CONFIG_SCHEMA_VERSION;
        let mut notes = Vec::new();
        let mut mapped_keys = Vec::new();
        let mut unmapped_keys = Vec::new();
        let mut legacy_detected = false;

        if current_version >= target_version {
            notes.push("当前配置已是最新版本".to_string());
            return Ok(ConfigMigrationReport {
                current_version,
                target_version,
                applied: false,
                dry_run,
                legacy_detected: false,
                mapped_keys,
                unmapped_keys,
                notes,
            });
        }

        if let Some(raw_value) = self.load_legacy_config_value().await? {
            legacy_detected = true;
            let (normalized_value, meta, legacy_unmapped) = self.normalize_legacy_config_value(&raw_value);
            mapped_keys = normalized_value
                .as_object()
                .map(|obj| obj.keys().cloned().collect())
                .unwrap_or_default();
            unmapped_keys = legacy_unmapped.keys().cloned().collect();

            let unmapped_set: HashSet<String> = unmapped_keys.iter().cloned().collect();
            mapped_keys.retain(|key| !unmapped_set.contains(key));

            let report_suffix = if dry_run { "（预览）" } else { "" };
            let mapped_list = if mapped_keys.is_empty() {
                "无".to_string()
            } else {
                mapped_keys
                    .iter()
                    .map(|key| format!("{}({})", key, describe_config_key(key)))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let unmapped_list = if unmapped_keys.is_empty() {
                "无".to_string()
            } else {
                unmapped_keys
                    .iter()
                    .map(|key| format!("{}({})", key, describe_config_key(key)))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            info!(
                "配置迁移报告{}：已映射字段({}) = {}",
                report_suffix,
                mapped_keys.len(),
                mapped_list
            );
            info!(
                "配置迁移报告{}：未映射字段({}) = {}",
                report_suffix,
                unmapped_keys.len(),
                unmapped_list
            );

            if !dry_run {
                let legacy: Config = serde_json::from_value(normalized_value).context("从旧版配置构建新配置失败")?;
                self.save_config(&legacy)
                    .await
                    .context("旧版配置写入 config_items 失败")?;

                self.update_config_item("legacy_config_raw", raw_value).await.ok();
                if !legacy_unmapped.is_empty() {
                    self.update_config_item("legacy_config_unmapped", Value::Object(legacy_unmapped))
                        .await
                        .ok();
                }

                if let Err(e) = self
                    .apply_skip_option_to_sources(meta.no_danmaku, meta.no_subtitle)
                    .await
                {
                    warn!("应用旧版跳过配置到视频源失败: {}", e);
                }

                self.set_config_schema_version(target_version).await?;
                self.update_config_item("config_schema_migrated_at", Value::String(now_standard_string()))
                    .await
                    .ok();
            }

            notes.push("已从旧版 config 表迁移配置".to_string());
        } else {
            notes.push("未检测到旧版 config 表，标记版本更新".to_string());
            if !dry_run {
                self.set_config_schema_version(target_version).await?;
                self.update_config_item("config_schema_migrated_at", Value::String(now_standard_string()))
                    .await
                    .ok();
            }
        }

        Ok(ConfigMigrationReport {
            current_version,
            target_version,
            applied: !dry_run,
            dry_run,
            legacy_detected,
            mapped_keys,
            unmapped_keys,
            notes,
        })
    }

    /// 确保配置表存在，如果不存在则创建
    pub async fn ensure_tables_exist(&self) -> Result<()> {
        debug!("检查配置表是否存在...");

        // 创建config_items表
        let create_config_items = "
            CREATE TABLE IF NOT EXISTS config_items (
                key_name TEXT PRIMARY KEY NOT NULL,
                value_json TEXT NOT NULL,
                updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP NOT NULL
            )";

        // 创建config_changes表
        let create_config_changes = "
            CREATE TABLE IF NOT EXISTS config_changes (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                key_name TEXT NOT NULL,
                old_value TEXT,
                new_value TEXT NOT NULL,
                changed_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP NOT NULL
            )";

        // 执行SQL创建表
        self.db
            .execute_unprepared(create_config_items)
            .await
            .context("创建config_items表失败")?;
        self.db
            .execute_unprepared(create_config_changes)
            .await
            .context("创建config_changes表失败")?;

        debug!("配置表检查完成");
        Ok(())
    }

    /// 从数据库加载配置并构建 ConfigBundle
    pub async fn load_config_bundle(&self) -> Result<ConfigBundle> {
        // 尝试从数据库加载配置
        match self.load_from_database().await {
            Ok(config) => {
                debug!("从数据库加载配置成功");
                ConfigBundle::from_config(config)
            }
            Err(e) => {
                warn!("从数据库加载配置失败: {}, 尝试从TOML加载", e);
                // 如果数据库加载失败，回退到TOML配置
                let config = self.load_from_toml()?;

                // 将TOML配置迁移到数据库
                if let Err(migrate_err) = self.migrate_to_database(&config).await {
                    error!("迁移配置到数据库失败: {}", migrate_err);
                }

                ConfigBundle::from_config(config)
            }
        }
    }

    /// 从数据库加载配置
    async fn load_from_database(&self) -> Result<Config> {
        let config_items: Vec<config_item::Model> = ConfigItem::find().all(&self.db).await?;

        let config_item_count = config_items.len();
        let has_credential = config_items.iter().any(|item| item.key_name == "credential");

        if config_items.is_empty() {
            if let Some(legacy) = self.try_load_legacy_config().await? {
                info!("检测到旧版 config 表，已尝试自动转换到 config_items");
                self.save_config(&legacy)
                    .await
                    .context("旧版 config 迁移到 config_items 失败")?;
                return Ok(legacy);
            }
            return Err(anyhow!("数据库中没有配置项"));
        }

        if config_item_count < 5 || !has_credential {
            if let Some(legacy) = self.try_load_legacy_config().await? {
                info!("配置项数量过少或缺少凭证，尝试从旧版 config 迁移");
                self.save_config(&legacy)
                    .await
                    .context("旧版 config 迁移到 config_items 失败")?;
                return Ok(legacy);
            }
        }
        // 将数据库配置项转换为配置映射
        let mut config_map: HashMap<String, Value> = HashMap::new();
        for item in config_items {
            let value: Value =
                serde_json::from_str(&item.value_json).with_context(|| format!("解析配置项 {} 失败", item.key_name))?;
            config_map.insert(item.key_name, value);
        }

        // 构建完整的配置对象
        self.build_config_from_map(config_map)
    }

    /// 从配置映射构建Config对象
    fn build_config_from_map(&self, mut config_map: HashMap<String, Value>) -> Result<Config> {
        // 检测并解决配置冲突：当既有完整对象又有嵌套字段时，优先使用嵌套字段
        self.resolve_config_conflicts(&mut config_map)?;

        // 将扁平化的配置映射转换为嵌套结构
        let mut nested_map = serde_json::Map::new();

        for (key, value) in config_map {
            // 处理嵌套键，如 "notification.enable_scan_notifications"
            let parts: Vec<&str> = key.split('.').collect();

            if parts.len() == 1 {
                // 顶级键，直接插入
                nested_map.insert(key, value);
            } else {
                // 嵌套键，需要构建嵌套结构
                Self::insert_nested(&mut nested_map, &parts, value);
            }
        }

        // 将嵌套映射转换为配置对象
        let config_json = Value::Object(nested_map);

        // 添加详细的反序列化错误信息
        debug!(
            "尝试反序列化配置JSON: {}",
            serde_json::to_string_pretty(&config_json).unwrap_or_else(|_| "无法格式化JSON".to_string())
        );

        let config: Config = serde_json::from_value(config_json.clone()).map_err(|e| {
            error!("配置反序列化详细错误: {}", e);
            error!(
                "配置JSON内容: {}",
                serde_json::to_string_pretty(&config_json).unwrap_or_else(|_| "无法格式化JSON".to_string())
            );
            anyhow!("从数据库数据构建配置对象失败: {}", e)
        })?;

        Ok(config)
    }
    /// 尝试从旧版 config 表加载配置数据
    async fn try_load_legacy_config(&self) -> Result<Option<Config>> {
        let Some(raw_value) = self.load_legacy_config_value().await? else {
            return Ok(None);
        };

        let (normalized_value, meta, legacy_unmapped) = self.normalize_legacy_config_value(&raw_value);

        let legacy: Config = serde_json::from_value(normalized_value).context("从旧版配置构建新配置失败")?;

        // 保留完整旧配置，确保零损耗回溯
        self.update_config_item("legacy_config_raw", raw_value).await.ok();
        if !legacy_unmapped.is_empty() {
            self.update_config_item("legacy_config_unmapped", Value::Object(legacy_unmapped))
                .await
                .ok();
        }

        // 应用旧版全局跳过配置到现有视频源
        if let Err(e) = self
            .apply_skip_option_to_sources(meta.no_danmaku, meta.no_subtitle)
            .await
        {
            warn!("应用旧版跳过配置到视频源失败: {}", e);
        }

        Ok(Some(legacy))
    }

    async fn legacy_config_exists(&self) -> Result<bool> {
        let backend = self.db.get_database_backend();
        let exists_sql = "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='config'";
        let exists_row = self.db.query_one(Statement::from_string(backend, exists_sql)).await?;
        let exists: i64 = exists_row.and_then(|row| row.try_get_by_index(0).ok()).unwrap_or(0);
        if exists == 0 {
            return Ok(false);
        }

        let count_sql = "SELECT COUNT(*) FROM config";
        let count_row = self.db.query_one(Statement::from_string(backend, count_sql)).await?;
        let count: i64 = count_row.and_then(|row| row.try_get_by_index(0).ok()).unwrap_or(0);
        Ok(count > 0)
    }

    async fn load_legacy_config_value(&self) -> Result<Option<Value>> {
        if !self.legacy_config_exists().await? {
            return Ok(None);
        }
        let backend = self.db.get_database_backend();
        let data_sql = "SELECT data FROM config LIMIT 1";
        let data_row = self.db.query_one(Statement::from_string(backend, data_sql)).await?;
        let data: Option<String> = match data_row {
            Some(row) => row.try_get_by_index(0).ok(),
            None => None,
        };
        let Some(data) = data else {
            return Ok(None);
        };
        let raw_value: Value = serde_json::from_str(&data).context("解析旧版 config.data JSON 失败")?;
        Ok(Some(raw_value))
    }

    /// 标准化旧版配置到当前结构，返回(新配置值, 迁移元信息, 未映射字段)
    fn normalize_legacy_config_value(&self, raw: &Value) -> (Value, LegacyMigrationMeta, Map<String, Value>) {
        let mut normalized = raw.clone();
        let mut meta = LegacyMigrationMeta::default();
        let mut legacy_unmapped = Map::new();

        let Some(root) = normalized.as_object_mut() else {
            return (normalized, meta, legacy_unmapped);
        };

        // 记录可能无法直接映射的字段
        for key in [
            "favorite_default_path",
            "collection_default_path",
            "submission_default_path",
            "notifiers",
            "version",
        ] {
            if let Some(value) = root.get(key) {
                legacy_unmapped.insert(key.to_string(), value.clone());
            }
        }

        // 清理非法的 notification（确保为对象）
        if let Some(notification) = root.get("notification").cloned() {
            if !notification.is_object() {
                legacy_unmapped.insert("notification".to_string(), notification);
                root.remove("notification");
            }
        }

        // notifiers -> notification（尽量兼容旧结构，仅当为对象时迁移）
        if root.get("notification").is_none() {
            if let Some(notifiers) = root.get("notifiers").cloned() {
                if notifiers.is_object() {
                    root.insert("notification".to_string(), notifiers);
                } else {
                    legacy_unmapped.insert("notifiers".to_string(), notifiers);
                }
            }
        }

        // skip_option -> nfo_config / 下载开关
        if let Some(skip_option) = root.get("skip_option").cloned() {
            legacy_unmapped.insert("skip_option".to_string(), skip_option.clone());
            if let Some(skip) = skip_option.as_object() {
                let no_video_nfo = skip.get("no_video_nfo").and_then(|v| v.as_bool()).unwrap_or(false);
                let no_upper = skip.get("no_upper").and_then(|v| v.as_bool()).unwrap_or(false);
                let no_danmaku = skip.get("no_danmaku").and_then(|v| v.as_bool()).unwrap_or(false);
                let no_subtitle = skip.get("no_subtitle").and_then(|v| v.as_bool()).unwrap_or(false);

                meta.no_danmaku = no_danmaku;
                meta.no_subtitle = no_subtitle;

                // 合并到 nfo_config
                let nfo_config = root.entry("nfo_config").or_insert_with(|| Value::Object(Map::new()));
                if !nfo_config.is_object() {
                    legacy_unmapped.insert("nfo_config".to_string(), nfo_config.clone());
                    *nfo_config = Value::Object(Map::new());
                }
                if let Some(nfo_obj) = nfo_config.as_object_mut() {
                    if no_video_nfo {
                        nfo_obj.insert("enabled".to_string(), Value::Bool(false));
                    }
                    if no_upper {
                        nfo_obj.insert("include_actor_info".to_string(), Value::Bool(false));
                    }
                }
            }
        }

        // nfo_time_type -> nfo_config.time_type
        if let Some(nfo_time_type) = root.get("nfo_time_type").cloned() {
            let nfo_config = root.entry("nfo_config").or_insert_with(|| Value::Object(Map::new()));
            if !nfo_config.is_object() {
                legacy_unmapped.insert("nfo_config".to_string(), nfo_config.clone());
                *nfo_config = Value::Object(Map::new());
            }
            if let Some(nfo_obj) = nfo_config.as_object_mut() {
                nfo_obj.insert("time_type".to_string(), nfo_time_type);
            }
        }

        // concurrent_limit.download -> concurrent_limit.parallel_download
        if let Some(concurrent_limit_value) = root.get("concurrent_limit") {
            if !concurrent_limit_value.is_object() {
                legacy_unmapped.insert("concurrent_limit".to_string(), concurrent_limit_value.clone());
                root.remove("concurrent_limit");
            }
        }

        if let Some(concurrent_limit) = root.get_mut("concurrent_limit").and_then(|v| v.as_object_mut()) {
            if let Some(download) = concurrent_limit.get("download").cloned() {
                legacy_unmapped.insert("concurrent_limit.download".to_string(), download.clone());
            }
            if concurrent_limit.get("parallel_download").is_none() {
                if let Some(download) = concurrent_limit.remove("download") {
                    if let Some(download_obj) = download.as_object() {
                        let enabled = download_obj.get("enable").and_then(|v| v.as_bool()).unwrap_or(true);
                        let threads = download_obj.get("concurrency").and_then(|v| v.as_u64()).unwrap_or(4);
                        let use_aria2 = download_obj.get("use_aria2").and_then(|v| v.as_bool());

                        let mut parallel_download = Map::new();
                        parallel_download.insert("enabled".to_string(), Value::Bool(enabled));
                        parallel_download
                            .insert("threads".to_string(), Value::Number(serde_json::Number::from(threads)));
                        if let Some(use_aria2) = use_aria2 {
                            parallel_download.insert("use_aria2".to_string(), Value::Bool(use_aria2));
                        }

                        concurrent_limit.insert("parallel_download".to_string(), Value::Object(parallel_download));
                    }
                }
            }
        }

        (normalized, meta, legacy_unmapped)
    }

    /// 将旧版全局跳过配置同步到现有视频源下载开关
    async fn apply_skip_option_to_sources(&self, no_danmaku: bool, no_subtitle: bool) -> Result<()> {
        if !no_danmaku && !no_subtitle {
            return Ok(());
        }

        let mut updates = Vec::new();
        if no_danmaku {
            updates.push("download_danmaku = 0");
        }
        if no_subtitle {
            updates.push("download_subtitle = 0");
        }
        let set_clause = updates.join(", ");

        for table in ["collection", "favorite", "submission", "watch_later", "video_source"] {
            let sql = format!("UPDATE {} SET {}", table, set_clause);
            if let Err(e) = self.db.execute_unprepared(&sql).await {
                warn!("更新表 {} 的下载开关失败: {}", table, e);
            }
        }

        Ok(())
    }
    /// 递归插入嵌套值
    fn insert_nested(map: &mut serde_json::Map<String, Value>, parts: &[&str], value: Value) {
        if parts.is_empty() {
            return;
        }

        if parts.len() == 1 {
            map.insert(parts[0].to_string(), value);
            return;
        }

        let key = parts[0];
        let remaining = &parts[1..];

        // 确保当前键存在且是对象
        if !map.contains_key(key) {
            map.insert(key.to_string(), Value::Object(serde_json::Map::new()));
        }

        // 递归处理剩余部分
        if let Some(Value::Object(nested)) = map.get_mut(key) {
            Self::insert_nested(nested, remaining, value);
        }
    }

    /// 移除TOML文件加载 - 配置现在完全基于数据库
    fn load_from_toml(&self) -> Result<Config> {
        // 配置现在完全基于数据库，不再从TOML文件加载
        warn!("TOML配置已弃用，使用默认配置");
        Ok(Config::default())
    }

    /// 将配置保存到数据库
    pub async fn save_config(&self, config: &Config) -> Result<()> {
        // 将配置对象序列化为键值对
        let config_json = serde_json::to_value(config)?;
        let config_map = self.flatten_config_json(config_json)?;

        // 保存到数据库
        for (key, value) in config_map {
            let value_json = serde_json::to_string(&value)?;

            // 查找现有配置项
            let existing = ConfigItem::find()
                .filter(config_item::Column::KeyName.eq(&key))
                .one(&self.db)
                .await?;

            if let Some(existing_model) = existing {
                // 记录变更历史
                if let Err(e) = self
                    .record_config_change(&key, Some(&existing_model.value_json), &value_json)
                    .await
                {
                    warn!("记录配置变更历史失败: {}", e);
                }

                // 更新现有配置项
                let mut active_model: config_item::ActiveModel = existing_model.into();
                active_model.value_json = Set(value_json);
                active_model.updated_at = Set(now_standard_string());
                active_model.update(&self.db).await?;
            } else {
                // 记录变更历史（新增）
                if let Err(e) = self.record_config_change(&key, None, &value_json).await {
                    warn!("记录配置变更历史失败: {}", e);
                }

                // 创建新配置项
                let new_model = config_item::ActiveModel {
                    key_name: Set(key),
                    value_json: Set(value_json),
                    updated_at: Set(now_standard_string()),
                };
                new_model.insert(&self.db).await?;
            }
        }

        info!("配置已保存到数据库");

        Ok(())
    }

    /// 更新单个配置项
    pub async fn update_config_item(&self, key: &str, value: Value) -> Result<()> {
        // 防止写入嵌套的notification字段
        if key.starts_with("notification.") {
            warn!("拒绝写入嵌套的notification字段: {}，请使用完整的notification对象", key);
            return Ok(()); // 静默忽略，不返回错误
        }

        let value_json = serde_json::to_string(&value)?;

        // 查找现有配置项
        let existing = ConfigItem::find()
            .filter(config_item::Column::KeyName.eq(key))
            .one(&self.db)
            .await?;

        if let Some(existing_model) = existing {
            // 记录变更历史
            if let Err(e) = self
                .record_config_change(key, Some(&existing_model.value_json), &value_json)
                .await
            {
                warn!("记录配置变更历史失败: {}", e);
            }

            // 更新现有配置项
            let mut active_model: config_item::ActiveModel = existing_model.into();
            active_model.value_json = Set(value_json);
            active_model.updated_at = Set(now_standard_string());
            active_model.update(&self.db).await?;
        } else {
            // 记录变更历史
            if let Err(e) = self.record_config_change(key, None, &value_json).await {
                warn!("记录配置变更历史失败: {}", e);
            }

            // 创建新配置项
            let new_model = config_item::ActiveModel {
                key_name: Set(key.to_string()),
                value_json: Set(value_json),
                updated_at: Set(now_standard_string()),
            };
            new_model.insert(&self.db).await?;
        }

        debug!("配置项 {} 已更新", key);

        Ok(())
    }

    /// 将TOML配置迁移到数据库
    async fn migrate_to_database(&self, config: &Config) -> Result<()> {
        info!("开始迁移TOML配置到数据库");
        self.save_config(config).await?;
        info!("TOML配置迁移完成");
        Ok(())
    }

    /// 扁平化配置JSON为键值对
    fn flatten_config_json(&self, config_json: Value) -> Result<HashMap<String, Value>> {
        let mut result = HashMap::new();

        if let Value::Object(map) = config_json {
            for (key, value) in map {
                // 对于复杂对象，直接存储整个JSON值
                result.insert(key, value);
            }
        } else {
            return Err(anyhow!("配置必须是JSON对象"));
        }

        Ok(result)
    }

    /// 记录配置变更历史 (使用原生SQL)
    async fn record_config_change(&self, key: &str, old_value: Option<&str>, new_value: &str) -> Result<()> {
        let sql = "INSERT INTO config_changes (key_name, old_value, new_value, changed_at) VALUES (?, ?, ?, ?)";

        let stmt = sea_orm::Statement::from_sql_and_values(
            sea_orm::DatabaseBackend::Sqlite,
            sql,
            vec![
                key.into(),
                old_value.into(),
                new_value.into(),
                now_standard_string().into(),
            ],
        );

        self.db.execute(stmt).await?;

        // 记录当前config_changes表的记录数，用于监控
        let count_sql = "SELECT COUNT(*) as count FROM config_changes";
        let count_stmt = sea_orm::Statement::from_string(sea_orm::DatabaseBackend::Sqlite, count_sql);
        let count_result = self.db.query_one(count_stmt).await?;

        if let Some(row) = count_result {
            let count: i64 = row.try_get("", "count")?;
            debug!("config_changes表当前记录数: {}", count);
        }

        Ok(())
    }

    /// 获取单个配置项
    pub async fn get_config_item(&self, key: &str) -> Result<Option<Value>> {
        let config_item = ConfigItem::find()
            .filter(config_item::Column::KeyName.eq(key))
            .one(&self.db)
            .await?;

        if let Some(item) = config_item {
            let value: Value =
                serde_json::from_str(&item.value_json).with_context(|| format!("解析配置项 {} 失败", key))?;
            Ok(Some(value))
        } else {
            Ok(None)
        }
    }

    /// 获取配置变更历史 (使用原生SQL)
    pub async fn get_config_history(
        &self,
        key: Option<&str>,
        limit: Option<u64>,
    ) -> Result<Vec<config_item::ConfigChangeModel>> {
        let mut sql = "SELECT id, key_name, old_value, new_value, changed_at FROM config_changes".to_string();
        let mut values = Vec::new();

        if let Some(key) = key {
            sql.push_str(" WHERE key_name = ?");
            values.push(key.into());
        }

        sql.push_str(" ORDER BY changed_at DESC");

        if let Some(limit) = limit {
            sql.push_str(" LIMIT ?");
            values.push(limit.into());
        }

        let stmt = sea_orm::Statement::from_sql_and_values(sea_orm::DatabaseBackend::Sqlite, &sql, values);

        let query_result = self.db.query_all(stmt).await?;

        let mut changes = Vec::new();
        for row in query_result {
            let change = config_item::ConfigChangeModel {
                id: row.try_get::<i32>("", "id")?,
                key_name: row.try_get::<String>("", "key_name")?,
                old_value: row.try_get::<Option<String>>("", "old_value")?,
                new_value: row.try_get::<String>("", "new_value")?,
                changed_at: row.try_get::<String>("", "changed_at")?,
            };
            changes.push(change);
        }

        Ok(changes)
    }

    /// 解决配置冲突：当既有完整对象又有嵌套字段时，优先使用嵌套字段
    fn resolve_config_conflicts(&self, config_map: &mut HashMap<String, Value>) -> Result<()> {
        // 检测可能冲突的配置前缀
        let potential_conflicts = [
            "notification",
            "concurrent_limit",
            "submission_risk_control",
            "submission_scan_strategy",
        ];

        for prefix in potential_conflicts {
            let has_complete_object = config_map.contains_key(prefix);
            let nested_keys: Vec<String> = config_map
                .keys()
                .filter(|key| key.starts_with(&format!("{}.", prefix)))
                .cloned()
                .collect();
            let has_nested_fields = !nested_keys.is_empty();

            if has_complete_object && has_nested_fields {
                if prefix == "notification" {
                    // 对于notification，删除嵌套字段，保留完整对象
                    warn!(
                        "检测到配置冲突：既有完整的 {} 对象又有嵌套字段，删除嵌套字段并从数据库永久移除",
                        prefix
                    );

                    // 从内存中移除嵌套字段
                    for nested_key in &nested_keys {
                        config_map.remove(nested_key);
                    }

                    // 从数据库中永久删除嵌套字段
                    if let Err(e) = self.delete_nested_fields_from_db(prefix, &nested_keys) {
                        warn!("删除数据库中的嵌套字段失败: {}", e);
                    }
                } else {
                    // 对于其他配置，保持原有逻辑：移除完整对象，保留嵌套字段
                    warn!(
                        "检测到配置冲突：既有完整的 {} 对象又有嵌套字段，移除完整对象以解决冲突",
                        prefix
                    );
                    config_map.remove(prefix);
                }
            }
        }

        Ok(())
    }

    /// 从数据库中删除嵌套字段
    fn delete_nested_fields_from_db(&self, prefix: &str, nested_keys: &[String]) -> Result<()> {
        use tokio::runtime::Handle;

        // 创建异步任务来删除数据库记录
        let db = self.db.clone();
        let keys = nested_keys.to_vec();
        let prefix = prefix.to_string();

        // 如果在异步上下文中，直接执行；否则创建新的运行时
        if let Ok(handle) = Handle::try_current() {
            handle.spawn(async move {
                Self::delete_config_keys_async(db, keys).await;
            });
        } else {
            std::thread::spawn(move || {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    Self::delete_config_keys_async(db, keys).await;
                });
            });
        }

        info!("已标记删除 {} 的嵌套配置字段", prefix);
        Ok(())
    }

    /// 异步删除配置键
    async fn delete_config_keys_async(db: sea_orm::DatabaseConnection, keys: Vec<String>) {
        use bili_sync_entity::entities::config_item;
        use sea_orm::*;

        for key in keys {
            if let Err(e) = config_item::Entity::delete_many()
                .filter(config_item::Column::KeyName.eq(&key))
                .exec(&db)
                .await
            {
                warn!("删除配置键 {} 失败: {}", key, e);
            } else {
                info!("成功从数据库删除配置键: {}", key);
            }
        }
    }
}
