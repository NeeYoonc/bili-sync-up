//! 投稿源断点信息持久化模块
//!
//! 此模块负责将投稿源的断点信息（页码和视频索引）持久化到数据库中，
//! 确保程序重启后能够从中断的位置继续扫描。

use anyhow::Result;
use sea_orm::DatabaseConnection;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info, warn};

use crate::bilibili::submission::{SUBMISSION_PAGE_TRACKER, SUBMISSION_RESUME_TRACKER};

const CHECKPOINT_KEY: &str = "submission_checkpoints";

/// 断点信息结构
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SubmissionCheckpoints {
    /// UP主ID -> (页码, 该页已处理的视频索引)
    #[serde(default)]
    pub checkpoints: HashMap<String, (usize, usize)>,
}

/// 从数据库恢复断点信息到内存
pub async fn restore_checkpoints_from_db(db: &Arc<DatabaseConnection>) -> Result<()> {
    use bili_sync_entity::entities::{config_item, prelude::ConfigItem};
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    // 查询断点配置项
    let config_item = ConfigItem::find()
        .filter(config_item::Column::KeyName.eq(CHECKPOINT_KEY))
        .one(db.as_ref())
        .await?;

    match config_item {
        Some(item) => {
            // 反序列化JSON
            let checkpoints: SubmissionCheckpoints = serde_json::from_str(&item.value_json).unwrap_or_else(|e| {
                warn!("解析断点信息失败: {}, 将使用空的断点信息", e);
                SubmissionCheckpoints::default()
            });

            // 恢复到内存中的静态变量
            let mut tracker = SUBMISSION_PAGE_TRACKER.write().unwrap();
            *tracker = checkpoints.checkpoints;
            SUBMISSION_RESUME_TRACKER.write().unwrap().clear();

            if !tracker.is_empty() {
                info!("从数据库恢复 {} 个断点信息", tracker.len());
                for (upper_id, (page, video_idx)) in tracker.iter() {
                    debug!("恢复断点: UP主{} -> 页码{}, 视频索引{}", upper_id, page, video_idx);
                }
            } else {
                debug!("没有需要恢复的断点信息");
            }
        }
        None => {
            SUBMISSION_RESUME_TRACKER.write().unwrap().clear();
            debug!("数据库中没有断点信息配置项");
        }
    }

    Ok(())
}

/// 将内存中的断点信息保存到数据库
pub async fn save_checkpoints_to_db(db: &Arc<DatabaseConnection>) -> Result<()> {
    use bili_sync_entity::entities::{config_item, prelude::ConfigItem};
    use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, QueryFilter, Set};

    // 读取内存中的断点信息
    let checkpoints = {
        let tracker = SUBMISSION_PAGE_TRACKER.read().unwrap();
        SubmissionCheckpoints {
            checkpoints: tracker.clone(),
        }
    };

    // 序列化为JSON
    let value_json = serde_json::to_string(&checkpoints)?;

    // 查询是否已存在
    let existing = ConfigItem::find()
        .filter(config_item::Column::KeyName.eq(CHECKPOINT_KEY))
        .one(db.as_ref())
        .await?;

    if let Some(existing_item) = existing {
        // 更新现有记录
        let mut active_model: config_item::ActiveModel = existing_item.into();
        active_model.value_json = Set(value_json);
        active_model.updated_at = Set(crate::utils::time_format::now_standard_string());
        active_model.update(db.as_ref()).await?;

        if !checkpoints.checkpoints.is_empty() {
            debug!("已更新 {} 个断点信息到数据库", checkpoints.checkpoints.len());
        } else {
            debug!("清除了数据库中的断点信息");
        }
    } else if !checkpoints.checkpoints.is_empty() {
        // 创建新记录（只有当有断点信息时才创建）
        let new_item = config_item::ActiveModel {
            key_name: Set(CHECKPOINT_KEY.to_string()),
            value_json: Set(value_json),
            updated_at: Set(crate::utils::time_format::now_standard_string()),
        };
        new_item.insert(db.as_ref()).await?;
        info!("保存 {} 个断点信息到数据库", checkpoints.checkpoints.len());
    } else {
        debug!("没有断点信息需要保存");
    }

    Ok(())
}

/// 清除指定UP主的断点信息（删除视频源时使用）
pub async fn clear_submission_checkpoint(db: &Arc<DatabaseConnection>, upper_id: i64) -> Result<()> {
    let upper_id_str = upper_id.to_string();

    // 从内存中清除指定UP主的断点
    let removed = {
        let mut tracker = SUBMISSION_PAGE_TRACKER.write().unwrap();
        tracker.remove(&upper_id_str).is_some()
    };
    SUBMISSION_RESUME_TRACKER.write().unwrap().remove(&upper_id_str);

    if removed {
        info!("清除UP主 {} 的断点信息（删除视频源）", upper_id);

        // 保存更新后的断点信息到数据库
        save_checkpoints_to_db(db).await?;
    } else {
        debug!("UP主 {} 没有断点信息需要清除", upper_id);
    }

    Ok(())
}
