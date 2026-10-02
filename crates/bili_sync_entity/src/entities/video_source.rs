//! 视频源实体定义

use sea_orm::entity::prelude::*;
use sea_orm::ActiveModelBehavior;
use strum::EnumIter;

#[derive(Clone, Debug, PartialEq, Eq, DeriveActiveEnum, EnumIter, Default)]
#[sea_orm(rs_type = "i32", db_type = "Integer")]
pub enum SourceType {
    #[sea_orm(num_value = 1)]
    #[default]
    Bangumi = 1,
    /// B 站课程（pugv/cheese），如 https://www.bilibili.com/cheese/play/ss713799843
    #[sea_orm(num_value = 2)]
    Pugv = 2,
}

/// `video` 表 `source_type` 列：番剧
pub const VIDEO_SOURCE_TYPE_BANGUMI: i32 = 1;
/// `video` 表 `source_type` 列：B 站课程（pugv/cheese）
pub const VIDEO_SOURCE_TYPE_PUGV: i32 = 2;

/// 是否为「剧集型」来源（番剧 / 课程）。
///
/// 两者都带 `season_id` + `ep_id` + 集数，共用按集组织的目录结构、
/// S/E 命名、NFO 与取流时走专用接口的逻辑，因此统一判定。
#[inline]
pub fn is_episode_source_type(source_type: Option<i32>) -> bool {
    matches!(
        source_type,
        Some(VIDEO_SOURCE_TYPE_BANGUMI) | Some(VIDEO_SOURCE_TYPE_PUGV)
    )
}

/// 是否为 B 站课程
#[inline]
pub fn is_pugv_source_type(source_type: Option<i32>) -> bool {
    source_type == Some(VIDEO_SOURCE_TYPE_PUGV)
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Default)]
#[sea_orm(table_name = "video_source")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub name: String,
    pub path: String,
    pub r#type: i32,
    pub latest_row_at: String,
    pub created_at: String,
    pub season_id: Option<String>,
    pub media_id: Option<String>,
    pub ep_id: Option<String>,
    pub download_all_seasons: Option<bool>,
    pub video_name_template: Option<String>,
    pub page_name_template: Option<String>,
    pub selected_seasons: Option<String>,
    pub enabled: bool,
    pub scan_deleted_videos: bool,
    pub scan_deleted_videos_once: bool,
    pub filter_option: Option<serde_json::Value>,
    pub cached_episodes: Option<String>,
    pub cache_updated_at: Option<String>,
    pub keyword_filters: Option<String>,
    pub keyword_filter_mode: Option<String>,
    pub blacklist_keywords: Option<String>,
    pub whitelist_keywords: Option<String>,
    pub keyword_case_sensitive: bool,
    pub min_duration_seconds: Option<i32>,
    pub max_duration_seconds: Option<i32>,
    pub published_after: Option<String>,
    pub published_before: Option<String>,
    pub audio_only: bool,
    pub audio_only_m4a_only: bool,
    pub flat_folder: bool,
    pub split_chapters_after_download: bool,
    pub download_charge_videos: bool,
    pub download_danmaku: bool,
    pub download_subtitle: bool,
    pub download_ai_subtitle: bool,
    pub ai_subtitle_language: String,
    pub ai_rename: bool,
    pub ai_rename_video_prompt: String,
    pub ai_rename_audio_prompt: String,
    pub ai_rename_enable_multi_page: bool,
    pub ai_rename_enable_collection: bool,
    pub ai_rename_enable_bangumi: bool,
    pub ai_rename_rename_parent_dir: bool,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
