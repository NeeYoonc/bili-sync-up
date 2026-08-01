use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, Default)]
// SeaQuery 默认将 `YouTubeSource` 转为 `you_tube_source`；迁移已按该名称
// 建表，实体必须与之完全一致。
#[sea_orm(table_name = "you_tube_source")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub source_type: String,
    pub name: String,
    pub url: String,
    pub path: String,
    pub enabled: bool,
    pub audio_only: bool,
    pub audio_only_m4a_only: bool,
    pub flat_folder: bool,
    pub download_danmaku: bool,
    pub download_subtitle: bool,
    pub ai_subtitle_language: String,
    pub ai_rename: bool,
    pub ai_rename_video_prompt: String,
    pub ai_rename_audio_prompt: String,
    pub ai_rename_enable_multi_page: bool,
    pub ai_rename_enable_collection: bool,
    pub ai_rename_enable_bangumi: bool,
    pub ai_rename_rename_parent_dir: bool,
    pub filter_option: Option<Json>,
    pub blacklist_keywords: Option<String>,
    pub whitelist_keywords: Option<String>,
    pub keyword_case_sensitive: bool,
    pub min_duration_seconds: Option<i32>,
    pub max_duration_seconds: Option<i32>,
    pub published_after: Option<String>,
    pub published_before: Option<String>,
    pub selected_videos: Option<String>,
    pub selected_channels: Option<String>,
    pub known_video_ids: Option<String>,
    pub last_scan_at: Option<String>,
    pub created_at: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::youtube_video::Entity")]
    YouTubeVideo,
}

impl Related<super::youtube_video::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::YouTubeVideo.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
