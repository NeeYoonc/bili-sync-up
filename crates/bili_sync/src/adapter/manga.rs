//! 哔哩哔哩漫画视频源适配器。
//!
//! 一部漫画 = 一个视频源（`video_source.type = 3`，`media_id` 存 `comic_id`），
//! 一话 = 一个 `video` 行，一页 = 一个 `page` 行。命名与落盘走漫画专用布局
//! （系列目录 + 每话一个 CBZ），其余流程（源管理、扫描、筛选、任务队列、
//! 状态位、通知）全部复用现有实现。

use std::path::{Path, PathBuf};
use std::pin::Pin;

use anyhow::Result;
use chrono::{DateTime, Utc};
use futures::Stream;
use sea_orm::prelude::*;
use sea_orm::ActiveValue::Set;
use tracing::info;

use bili_sync_entity::VideoSourceTrait;
use sea_orm::sea_query::SimpleExpr;

use crate::adapter::VideoSource;
use crate::bilibili::manga::Manga;
use crate::bilibili::{BiliClient, VideoInfo};

#[derive(Clone)]
pub struct MangaSource {
    pub id: i32,
    pub name: String,
    pub latest_row_at: String,
    /// B 漫作品 ID（comic_id）
    pub media_id: Option<String>,
    pub path: PathBuf,
    pub scan_deleted_videos: bool,
    pub scan_deleted_videos_once: bool,
    pub keyword_filters: Option<String>,
    pub keyword_filter_mode: Option<String>,
    pub blacklist_keywords: Option<String>,
    pub whitelist_keywords: Option<String>,
    pub keyword_case_sensitive: bool,
    pub min_duration_seconds: Option<i32>,
    pub max_duration_seconds: Option<i32>,
    pub published_after: Option<String>,
    pub published_before: Option<String>,
    pub filter_option: Option<serde_json::Value>,
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

impl MangaSource {
    pub async fn video_stream_from(
        &self,
        _bili_client: &BiliClient,
        _path: &Path,
        connection: &sea_orm::DatabaseConnection,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<VideoInfo>> + Send>>> {
        // 首次扫描（该源还没有任何视频记录）时使用全量模式，
        // 否则特典 / 老章节会被时间截断漏掉。
        let video_count = bili_sync_entity::video::Entity::find()
            .filter(bili_sync_entity::video::Column::SourceId.eq(self.id))
            .filter(bili_sync_entity::video::Column::SourceType.eq(3))
            .count(connection)
            .await?;

        let latest_row_at = if video_count == 0 {
            info!("检测到新漫画源「{}」（无历史记录），启用全量获取模式", self.name);
            None
        } else {
            Some(
                crate::utils::time_format::parse_time_string(&self.latest_row_at)
                    .unwrap_or_else(crate::utils::time_format::beijing_epoch_naive),
            )
        };

        let media_id = self
            .media_id
            .clone()
            .ok_or_else(|| anyhow::anyhow!("漫画源「{}」缺少 comic_id", self.name))?;
        let manga = Manga::new(media_id);
        Ok(Box::pin(manga.to_video_stream_incremental(latest_row_at)))
    }
}

impl VideoSourceTrait for MangaSource {
    fn get_latest_row_at(&self) -> String {
        self.latest_row_at.clone()
    }

    fn log_refresh_video_start(&self) {
        info!("开始获取漫画「{}」的更新", self.name);
    }

    fn log_refresh_video_end(&self, count: usize) {
        if count > 0 {
            info!("漫画「{}」获取更新完毕，新增 {} 话", self.name, count);
        } else {
            info!("漫画「{}」无新章节", self.name);
        }
    }

    fn log_fetch_video_start(&self) {
        info!("开始获取漫画「{}」的章节详情", self.name);
    }

    fn log_fetch_video_end(&self) {
        info!("漫画「{}」的章节详情获取完毕", self.name);
    }

    fn log_download_video_start(&self) {
        info!("开始下载漫画「{}」的章节", self.name);
    }

    fn log_download_video_end(&self) {
        info!("漫画「{}」的章节下载完毕", self.name);
    }

    fn scan_deleted_videos(&self) -> bool {
        self.scan_deleted_videos || self.scan_deleted_videos_once
    }

    fn filter_expr(&self) -> SimpleExpr {
        bili_sync_entity::video::Column::SourceId
            .eq(self.id)
            .and(bili_sync_entity::video::Column::SourceType.eq(3))
    }

    fn should_take(&self, _release_datetime: &DateTime<Utc>, _latest_row_at_string: &str) -> bool {
        // 章节顺序由 ord 决定，付费章节没有可用的发布时间，这里不做时间截断
        true
    }

    fn update_latest_row_at(&self, latest_row_at: String) -> bili_sync_entity::video_source::ActiveModel {
        let mut model = <bili_sync_entity::video_source::ActiveModel as sea_orm::ActiveModelTrait>::default();
        model.id = Set(self.id);
        model.latest_row_at = Set(latest_row_at);
        model
    }

    fn set_relation_id(&self, model: &mut bili_sync_entity::video::ActiveModel) {
        model.source_id = Set(Some(self.id));
        model.source_type = Set(Some(3));
    }
}

impl VideoSource for MangaSource {
    fn filter_expr(&self) -> SimpleExpr {
        bili_sync_entity::video::Column::SourceId
            .eq(self.id)
            .and(bili_sync_entity::video::Column::SourceType.eq(3))
    }

    fn set_relation_id(&self, model: &mut bili_sync_entity::video::ActiveModel) {
        model.source_id = Set(Some(self.id));
        model.source_type = Set(Some(3));
    }

    fn get_latest_row_at(&self) -> String {
        self.latest_row_at.clone()
    }

    fn update_latest_row_at(&self, datetime: String) -> crate::adapter::_ActiveModel {
        let mut model = <bili_sync_entity::video_source::ActiveModel as sea_orm::ActiveModelTrait>::default();
        model.id = Set(self.id);
        model.latest_row_at = Set(datetime);
        crate::adapter::_ActiveModel::Manga(Box::new(model))
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn should_take(&self, _release_datetime: &chrono::DateTime<Utc>, _latest_row_at_string: &str) -> bool {
        true
    }

    fn log_refresh_video_start(&self) {
        info!("开始获取漫画「{}」的更新", self.name);
    }

    fn log_refresh_video_end(&self, count: usize) {
        if count > 0 {
            info!("漫画「{}」获取更新完毕，新增 {} 话", self.name, count);
        } else {
            info!("漫画「{}」无新章节", self.name);
        }
    }

    fn log_fetch_video_start(&self) {
        info!("开始获取漫画「{}」的章节详情", self.name);
    }

    fn log_fetch_video_end(&self) {
        info!("漫画「{}」的章节详情获取完毕", self.name);
    }

    fn log_download_video_start(&self) {
        info!("开始下载漫画「{}」的章节", self.name);
    }

    fn log_download_video_end(&self) {
        info!("漫画「{}」的章节下载完毕", self.name);
    }

    fn scan_deleted_videos(&self) -> bool {
        self.scan_deleted_videos || self.scan_deleted_videos_once
    }

    fn source_type_display(&self) -> String {
        "漫画".to_string()
    }

    fn source_name_display(&self) -> String {
        self.name.clone()
    }

    fn get_keyword_filters(&self) -> Option<String> {
        self.keyword_filters.clone()
    }

    fn get_keyword_filter_mode(&self) -> Option<String> {
        self.keyword_filter_mode.clone()
    }

    fn get_blacklist_keywords(&self) -> Option<String> {
        self.blacklist_keywords.clone()
    }

    fn get_whitelist_keywords(&self) -> Option<String> {
        self.whitelist_keywords.clone()
    }

    fn get_keyword_case_sensitive(&self) -> bool {
        self.keyword_case_sensitive
    }

    fn get_min_duration_seconds(&self) -> Option<i32> {
        self.min_duration_seconds
    }

    fn get_max_duration_seconds(&self) -> Option<i32> {
        self.max_duration_seconds
    }

    fn get_published_after(&self) -> Option<String> {
        self.published_after.clone()
    }

    fn get_published_before(&self) -> Option<String> {
        self.published_before.clone()
    }

    fn filter_option(&self) -> Option<&serde_json::Value> {
        self.filter_option.as_ref()
    }

    fn audio_only(&self) -> bool {
        self.audio_only
    }

    fn audio_only_m4a_only(&self) -> bool {
        self.audio_only_m4a_only
    }

    fn flat_folder(&self) -> bool {
        self.flat_folder
    }

    fn split_chapters_after_download(&self) -> bool {
        self.split_chapters_after_download
    }

    fn download_charge_videos(&self) -> bool {
        self.download_charge_videos
    }

    fn download_danmaku(&self) -> bool {
        self.download_danmaku
    }

    fn download_subtitle(&self) -> bool {
        self.download_subtitle
    }

    fn download_ai_subtitle(&self) -> bool {
        self.download_ai_subtitle
    }

    fn ai_subtitle_language(&self) -> &str {
        &self.ai_subtitle_language
    }

    fn ai_rename(&self) -> bool {
        self.ai_rename
    }

    fn ai_rename_video_prompt(&self) -> &str {
        &self.ai_rename_video_prompt
    }

    fn ai_rename_audio_prompt(&self) -> &str {
        &self.ai_rename_audio_prompt
    }

    fn ai_rename_enable_multi_page(&self) -> bool {
        self.ai_rename_enable_multi_page
    }

    fn ai_rename_enable_collection(&self) -> bool {
        self.ai_rename_enable_collection
    }

    fn ai_rename_enable_bangumi(&self) -> bool {
        self.ai_rename_enable_bangumi
    }

    fn ai_rename_rename_parent_dir(&self) -> bool {
        self.ai_rename_rename_parent_dir
    }

    fn source_key(&self) -> String {
        format!("manga_{}", self.id)
    }
}
