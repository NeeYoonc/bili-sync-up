//! 任务队列数据库实体

use sea_orm::entity::prelude::*;
use sea_orm::sea_query::StringLen;
use serde::{Deserialize, Serialize};

/// 任务类型枚举
#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(50))")]
pub enum TaskType {
    #[sea_orm(string_value = "delete_video_source")]
    DeleteVideoSource,
    #[sea_orm(string_value = "delete_video")]
    DeleteVideo,
    #[sea_orm(string_value = "add_video_source")]
    AddVideoSource,
    #[sea_orm(string_value = "update_config")]
    UpdateConfig,
    #[sea_orm(string_value = "reload_config")]
    ReloadConfig,
    #[sea_orm(string_value = "refresh_danmaku")]
    RefreshDanmaku,
}

/// 任务状态枚举
#[derive(Debug, Clone, PartialEq, Eq, EnumIter, DeriveActiveEnum, Serialize, Deserialize)]
#[sea_orm(rs_type = "String", db_type = "String(StringLen::N(20))")]
pub enum TaskStatus {
    #[sea_orm(string_value = "pending")]
    Pending,
    #[sea_orm(string_value = "processing")]
    Processing,
    #[sea_orm(string_value = "completed")]
    Completed,
    #[sea_orm(string_value = "failed")]
    Failed,
}

/// 任务队列数据库实体
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "task_queue")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    /// 任务类型
    pub task_type: TaskType,
    /// 任务数据（JSON格式）
    pub task_data: String,
    /// 任务状态
    pub status: TaskStatus,
    /// 重试次数
    pub retry_count: i32,
    /// 创建时间
    pub created_at: String,
    /// 更新时间
    pub updated_at: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
