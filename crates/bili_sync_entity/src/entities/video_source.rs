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
    pub cached_episodes: Option<String>,
    pub cache_updated_at: Option<String>,
    pub keyword_filters: Option<String>,
    pub keyword_filter_mode: Option<String>,
    pub blacklist_keywords: Option<String>,
    pub whitelist_keywords: Option<String>,
    pub keyword_case_sensitive: bool,
    pub audio_only: bool,
    pub audio_only_m4a_only: bool,
    pub flat_folder: bool,
    pub download_danmaku: bool,
    pub download_subtitle: bool,
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
