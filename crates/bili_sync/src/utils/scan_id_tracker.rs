use anyhow::Result;
use sea_orm::DatabaseConnection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::debug;

/// 扫描源ID跟踪器，用于记录每个源类型的最后扫描ID
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LastScannedIds {
    // 记录每种类型源的最大ID（用于识别新源）
    #[serde(default)]
    pub collection: Option<i32>,
    #[serde(default)]
    pub favorite: Option<i32>,
    #[serde(default)]
    pub submission: Option<i32>,
    #[serde(default)]
    pub watch_later: Option<i32>,
    #[serde(default)]
    pub bangumi: Option<i32>,
    #[serde(default)]
    pub pugv: Option<i32>,
    #[serde(default)]
    pub manga: Option<i32>,

    // 记录每种类型源上次处理的ID（用于断点续传）
    #[serde(default)]
    pub last_processed_collection: Option<i32>,
    #[serde(default)]
    pub last_processed_favorite: Option<i32>,
    #[serde(default)]
    pub last_processed_submission: Option<i32>,
    #[serde(default)]
    pub last_processed_watch_later: Option<i32>,
    #[serde(default)]
    pub last_processed_bangumi: Option<i32>,
    #[serde(default)]
    pub last_processed_pugv: Option<i32>,
    #[serde(default)]
    pub last_processed_manga: Option<i32>,

    /// 合集/收藏夹的最近扫描状态。
    ///
    /// 这两类源没有独立的数据库状态列，因此与源扫描游标一起保存在配置 JSON 中。
    /// key 形如 `Collection:123` / `Favorite:456`，旧版本缺少该字段时会自动使用空表。
    #[serde(default)]
    pub source_scan_states: HashMap<String, SourceScanState>,
}

const CONFIG_KEY: &str = "last_scanned_ids";

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SourceScanState {
    #[serde(default)]
    pub last_scan_at: Option<String>,
    #[serde(default)]
    pub next_scan_at: Option<String>,
    #[serde(default)]
    pub no_update_streak: i32,
}

fn source_scan_state_key(source_type: SourceType, id: i32) -> String {
    let source_type = match source_type {
        SourceType::Collection => "Collection",
        SourceType::Favorite => "Favorite",
        SourceType::Submission => "Submission",
        SourceType::WatchLater => "WatchLater",
        SourceType::Bangumi => "Bangumi",
        SourceType::Pugv => "Pugv",
        SourceType::Manga => "Manga",
    };
    format!("{source_type}:{id}")
}

/// 从数据库获取最后扫描的ID记录
pub async fn get_last_scanned_ids(db: &Arc<DatabaseConnection>) -> Result<LastScannedIds> {
    use bili_sync_entity::entities::{config_item, prelude::ConfigItem};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    // 查询配置项
    let config_item = ConfigItem::find()
        .filter(config_item::Column::KeyName.eq(CONFIG_KEY))
        .one(db.as_ref())
        .await?;

    match config_item {
        Some(item) => {
            // 解析JSON值
            let ids: LastScannedIds = serde_json::from_str(&item.value_json)?;
            Ok(ids)
        }
        None => {
            // 配置项不存在，返回默认值
            Ok(LastScannedIds::default())
        }
    }
}

/// 更新最后扫描的ID记录到数据库
pub async fn update_last_scanned_ids(db: &Arc<DatabaseConnection>, ids: &LastScannedIds) -> Result<()> {
    use bili_sync_entity::entities::{config_item, prelude::ConfigItem};
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

    let value_json = serde_json::to_string(ids)?;

    // 查询是否已存在
    let existing = ConfigItem::find()
        .filter(config_item::Column::KeyName.eq(CONFIG_KEY))
        .one(db.as_ref())
        .await?;

    if let Some(existing_item) = existing {
        // 更新现有记录
        let mut active_model: config_item::ActiveModel = existing_item.into();
        active_model.value_json = Set(value_json);
        active_model.updated_at = Set(crate::utils::time_format::now_standard_string());
        active_model.update(db.as_ref()).await?;
    } else {
        // 创建新记录
        let new_item = config_item::ActiveModel {
            key_name: Set(CONFIG_KEY.to_string()),
            value_json: Set(value_json),
            updated_at: Set(crate::utils::time_format::now_standard_string()),
        };
        new_item.insert(db.as_ref()).await?;
    }

    Ok(())
}

/// 视频源信息，包含ID和其他必要信息
#[derive(Debug, Clone)]
pub struct VideoSourceWithId {
    pub id: i32,
    pub args: crate::adapter::Args,
    pub path: std::path::PathBuf,
    pub source_type: SourceType,
    /// 该源上次被扫描的时间；用于分批扫描时按最久未扫描优先轮转。
    pub last_scan_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceType {
    Collection,
    Favorite,
    Submission,
    WatchLater,
    Bangumi,
    Pugv,
    Manga,
}

/// 将视频源按新旧分组，并支持断点续传
pub fn group_sources_by_new_old(
    sources: Vec<VideoSourceWithId>,
    last_scanned_ids: &LastScannedIds,
) -> (Vec<VideoSourceWithId>, Vec<VideoSourceWithId>) {
    let mut new_sources = Vec::new();
    let mut old_sources = Vec::new();

    for source in sources {
        let (max_id, last_processed_id) = match source.source_type {
            SourceType::Collection => (last_scanned_ids.collection, last_scanned_ids.last_processed_collection),
            SourceType::Favorite => (last_scanned_ids.favorite, last_scanned_ids.last_processed_favorite),
            SourceType::Submission => (last_scanned_ids.submission, last_scanned_ids.last_processed_submission),
            SourceType::WatchLater => (
                last_scanned_ids.watch_later,
                last_scanned_ids.last_processed_watch_later,
            ),
            SourceType::Bangumi => (last_scanned_ids.bangumi, last_scanned_ids.last_processed_bangumi),
            SourceType::Pugv => (last_scanned_ids.pugv, last_scanned_ids.last_processed_pugv),
            SourceType::Manga => (last_scanned_ids.manga, last_scanned_ids.last_processed_manga),
        };

        // 如果没有记录（首次运行）或ID大于最大ID，则为新源
        if max_id.is_none() || source.id > max_id.unwrap() {
            new_sources.push(source);
        } else {
            // 旧源：检查是否有断点需要恢复
            let has_checkpoint = match source.source_type {
                SourceType::Submission => {
                    // 检查页码tracker中是否存在该源
                    use crate::adapter::Args;
                    if let Args::Submission { upper_id } = &source.args {
                        let tracker = crate::bilibili::submission::SUBMISSION_PAGE_TRACKER.read().unwrap();
                        tracker.contains_key(upper_id)
                    } else {
                        false
                    }
                }
                _ => false,
            };

            // 如果有断点或者还未处理，则添加到旧源列表
            if has_checkpoint || last_processed_id.is_none() || source.id > last_processed_id.unwrap() {
                if has_checkpoint {
                    debug!(
                        "检测到断点恢复源 (ID: {}, 类型: {:?})，包含在扫描列表中",
                        source.id, source.source_type
                    );
                }
                old_sources.push(source);
            } else {
                // 已处理过的源，跳过
                debug!("跳过已处理的源 (ID: {}, 类型: {:?})", source.id, source.source_type);
            }
        }
    }

    // 对旧源按ID排序，确保从小到大处理
    old_sources.sort_by_key(|s| s.id);

    (new_sources, old_sources)
}

/// 记录本轮扫描的最大ID和当前处理的ID
pub struct MaxIdRecorder {
    max_ids: HashMap<SourceType, i32>,
    current_processed_ids: HashMap<SourceType, i32>,
}

impl MaxIdRecorder {
    pub fn new() -> Self {
        Self {
            max_ids: HashMap::new(),
            current_processed_ids: HashMap::new(),
        }
    }

    /// 记录一个源的ID（已成功处理）
    pub fn record(&mut self, source_type: SourceType, id: i32) {
        // 更新最大ID
        self.max_ids
            .entry(source_type)
            .and_modify(|max_id| {
                if id > *max_id {
                    *max_id = id;
                }
            })
            .or_insert(id);

        // 更新当前处理的ID（用于断点续传）
        self.current_processed_ids.insert(source_type, id);
    }

    /// 获取记录的最大ID和处理ID，更新到LastScannedIds中
    pub fn merge_into(&self, last_scanned_ids: &mut LastScannedIds) {
        // 更新最大ID
        for (source_type, &max_id) in &self.max_ids {
            match source_type {
                SourceType::Collection => {
                    last_scanned_ids.collection = Some(max_id.max(last_scanned_ids.collection.unwrap_or(0)));
                }
                SourceType::Favorite => {
                    last_scanned_ids.favorite = Some(max_id.max(last_scanned_ids.favorite.unwrap_or(0)));
                }
                SourceType::Submission => {
                    last_scanned_ids.submission = Some(max_id.max(last_scanned_ids.submission.unwrap_or(0)));
                }
                SourceType::WatchLater => {
                    last_scanned_ids.watch_later = Some(max_id.max(last_scanned_ids.watch_later.unwrap_or(0)));
                }
                SourceType::Bangumi => {
                    last_scanned_ids.bangumi = Some(max_id.max(last_scanned_ids.bangumi.unwrap_or(0)));
                }
                SourceType::Pugv => {
                    last_scanned_ids.pugv = Some(max_id.max(last_scanned_ids.pugv.unwrap_or(0)));
                }
                SourceType::Manga => {
                    last_scanned_ids.manga = Some(max_id.max(last_scanned_ids.manga.unwrap_or(0)));
                }
            }
        }

        // 更新当前处理的ID（用于断点续传）
        for (source_type, &processed_id) in &self.current_processed_ids {
            match source_type {
                SourceType::Collection => {
                    last_scanned_ids.last_processed_collection = Some(processed_id);
                }
                SourceType::Favorite => {
                    last_scanned_ids.last_processed_favorite = Some(processed_id);
                }
                SourceType::Submission => {
                    last_scanned_ids.last_processed_submission = Some(processed_id);
                }
                SourceType::WatchLater => {
                    last_scanned_ids.last_processed_watch_later = Some(processed_id);
                }
                SourceType::Bangumi => {
                    last_scanned_ids.last_processed_bangumi = Some(processed_id);
                }
                SourceType::Pugv => {
                    last_scanned_ids.last_processed_pugv = Some(processed_id);
                }
                SourceType::Manga => {
                    last_scanned_ids.last_processed_manga = Some(processed_id);
                }
            }
        }
    }
}

impl LastScannedIds {
    pub fn source_scan_state(&self, source_type: SourceType, id: i32) -> Option<&SourceScanState> {
        self.source_scan_states.get(&source_scan_state_key(source_type, id))
    }

    pub fn set_source_scan_state(&mut self, source_type: SourceType, id: i32, state: SourceScanState) {
        self.source_scan_states
            .insert(source_scan_state_key(source_type, id), state);
    }

    /// 重置所有last_processed_id，使下次扫描从头开始
    pub fn reset_all_processed_ids(&mut self) {
        self.last_processed_collection = None;
        self.last_processed_favorite = None;
        self.last_processed_submission = None;
        self.last_processed_watch_later = None;
        self.last_processed_bangumi = None;
        self.last_processed_pugv = None;
        self.last_processed_manga = None;
    }
}

#[cfg(test)]
mod tests {
    use super::{LastScannedIds, SourceScanState, SourceType};

    #[test]
    fn legacy_last_scanned_ids_without_scan_states_deserializes() {
        let ids: LastScannedIds = serde_json::from_str(r#"{"collection":1190,"favorite":9}"#)
            .expect("旧版扫描游标 JSON 应能兼容");

        assert_eq!(ids.collection, Some(1190));
        assert_eq!(ids.favorite, Some(9));
        assert!(ids.source_scan_states.is_empty());
    }

    #[test]
    fn collection_and_favorite_scan_states_use_distinct_keys() {
        let mut ids = LastScannedIds::default();
        let state = SourceScanState {
            last_scan_at: Some("2026-11-04 10:00:00".to_string()),
            next_scan_at: None,
            no_update_streak: 2,
        };

        ids.set_source_scan_state(SourceType::Collection, 1, state.clone());

        assert_eq!(
            ids.source_scan_state(SourceType::Collection, 1)
                .map(|value| value.last_scan_at.as_deref()),
            Some(Some("2026-11-04 10:00:00"))
        );
        assert!(ids.source_scan_state(SourceType::Favorite, 1).is_none());
    }
}
