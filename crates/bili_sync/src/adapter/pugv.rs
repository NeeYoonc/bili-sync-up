//! B 站课程（pugv / cheese）视频源适配器。

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
use crate::bilibili::pugv::Pugv;
use crate::bilibili::{BiliClient, VideoInfo};

#[derive(Clone)]
pub struct PugvSource {
    pub id: i32,
    pub name: String,
    pub latest_row_at: String,
    pub season_id: Option<String>,
    pub ep_id: Option<String>,
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

impl PugvSource {
    pub async fn video_stream_from(
        &self,
        bili_client: &BiliClient,
        _path: &Path,
        connection: &sea_orm::DatabaseConnection,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<VideoInfo>> + Send>>> {
        // 首次扫描（该源还没有任何视频记录）时使用全量模式
        let video_count = bili_sync_entity::video::Entity::find()
            .filter(bili_sync_entity::video::Column::SourceId.eq(self.id))
            .filter(bili_sync_entity::video::Column::SourceType.eq(2))
            .count(connection)
            .await?;

        let latest_row_at = if video_count == 0 {
            info!("检测到新课程源「{}」（无历史记录），启用全量获取模式", self.name);
            None
        } else {
            Some(
                crate::utils::time_format::parse_time_string(&self.latest_row_at)
                    .unwrap_or_else(crate::utils::time_format::beijing_epoch_naive),
            )
        };

        let pugv = Pugv::new(bili_client, self.season_id.clone(), self.ep_id.clone());
        // 顺带把购买状态打到日志里，便于排查付费课时取流失败
        pugv.log_purchase_status().await;

        Ok(Box::pin(pugv.to_video_stream_incremental(latest_row_at)))
    }
}

impl VideoSourceTrait for PugvSource {
    fn get_latest_row_at(&self) -> String {
        self.latest_row_at.clone()
    }

    fn log_refresh_video_start(&self) {
        info!("开始获取课程「{}」的更新", self.name);
    }

    fn log_refresh_video_end(&self, count: usize) {
        if count > 0 {
            info!("课程「{}」获取更新完毕，新增 {} 个课时", self.name, count);
        } else {
            info!("课程「{}」无新课时", self.name);
        }
    }

    fn log_fetch_video_start(&self) {
        info!("开始获取课程「{}」的详细信息", self.name);
    }

    fn log_fetch_video_end(&self) {
        info!("课程「{}」的详细信息获取完毕", self.name);
    }

    fn log_download_video_start(&self) {
        info!("开始下载课程「{}」的视频", self.name);
    }

    fn log_download_video_end(&self) {
        info!("课程「{}」的视频下载完毕", self.name);
    }

    fn scan_deleted_videos(&self) -> bool {
        self.scan_deleted_videos || self.scan_deleted_videos_once
    }

    fn filter_expr(&self) -> SimpleExpr {
        bili_sync_entity::video::Column::SourceId
            .eq(self.id)
            .and(bili_sync_entity::video::Column::SourceType.eq(2))
    }

    fn should_take(&self, _release_datetime: &DateTime<Utc>, _latest_row_at_string: &str) -> bool {
        // 课程课时顺序固定，且未解锁课时没有发布时间，这里不做时间截断
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
        model.source_type = Set(Some(2));
    }
}

impl VideoSource for PugvSource {
    fn filter_expr(&self) -> SimpleExpr {
        bili_sync_entity::video::Column::SourceId
            .eq(self.id)
            .and(bili_sync_entity::video::Column::SourceType.eq(2))
    }

    fn set_relation_id(&self, model: &mut bili_sync_entity::video::ActiveModel) {
        model.source_id = Set(Some(self.id));
        model.source_type = Set(Some(2));
    }

    fn get_latest_row_at(&self) -> String {
        self.latest_row_at.clone()
    }

    fn update_latest_row_at(&self, datetime: String) -> crate::adapter::_ActiveModel {
        let mut model = <bili_sync_entity::video_source::ActiveModel as sea_orm::ActiveModelTrait>::default();
        model.id = Set(self.id);
        model.latest_row_at = Set(datetime);
        crate::adapter::_ActiveModel::Pugv(Box::new(model))
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn should_take(&self, _release_datetime: &chrono::DateTime<Utc>, _latest_row_at_string: &str) -> bool {
        true
    }

    fn log_refresh_video_start(&self) {
        info!("开始获取课程「{}」的更新", self.name);
    }

    fn log_refresh_video_end(&self, count: usize) {
        if count > 0 {
            info!("课程「{}」获取更新完毕，新增 {} 个课时", self.name, count);
        } else {
            info!("课程「{}」无新课时", self.name);
        }
    }

    fn log_fetch_video_start(&self) {
        info!("开始获取课程「{}」的详细信息", self.name);
    }

    fn log_fetch_video_end(&self) {
        info!("课程「{}」的详细信息获取完毕", self.name);
    }

    fn log_download_video_start(&self) {
        info!("开始下载课程「{}」的视频", self.name);
    }

    fn log_download_video_end(&self) {
        info!("课程「{}」的视频下载完毕", self.name);
    }

    fn scan_deleted_videos(&self) -> bool {
        self.scan_deleted_videos || self.scan_deleted_videos_once
    }

    fn source_type_display(&self) -> String {
        "课程".to_string()
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
        format!("pugv_{}", self.id)
    }
}
