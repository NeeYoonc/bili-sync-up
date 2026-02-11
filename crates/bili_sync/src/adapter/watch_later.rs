use std::path::Path;
use std::pin::Pin;

use anyhow::{Context, Result};
use bili_sync_entity::*;
use chrono::Utc;
use futures::Stream;
use sea_orm::entity::prelude::*;
use sea_orm::sea_query::SimpleExpr;
use sea_orm::ActiveValue::Set;
use sea_orm::{DatabaseConnection, Unchanged};

use crate::adapter::{VideoSource, VideoSourceEnum, _ActiveModel};
use crate::bilibili::{BiliClient, VideoInfo, WatchLater};

impl VideoSource for watch_later::Model {
    fn filter_expr(&self) -> SimpleExpr {
        video::Column::WatchLaterId.eq(self.id)
    }

    fn set_relation_id(&self, video_model: &mut video::ActiveModel) {
        video_model.watch_later_id = Set(Some(self.id));
    }

    fn path(&self) -> &Path {
        Path::new(self.path.as_str())
    }

    fn get_latest_row_at(&self) -> String {
        self.latest_row_at.clone()
    }

    fn update_latest_row_at(&self, datetime: String) -> _ActiveModel {
        _ActiveModel::WatchLater(watch_later::ActiveModel {
            id: Unchanged(self.id),
            latest_row_at: Set(datetime),
            ..Default::default()
        })
    }

    fn should_take(&self, _release_datetime: &chrono::DateTime<Utc>, _latest_row_at_string: &str) -> bool {
        // 修改稍后观看源，每次都全量拉取所有视频，不管时间戳
        true
    }

    fn log_refresh_video_start(&self) {
        info!("开始扫描稍后再看..");
    }

    fn log_refresh_video_end(&self, count: usize) {
        if count > 0 {
            info!("扫描稍后再看完成，获取到 {} 条新视频", count);
        } else {
            info!("稍后再看无新视频");
        }
    }

    fn log_fetch_video_start(&self) {
        debug!("开始填充稍后再看视频详情..");
    }

    fn log_fetch_video_end(&self) {
        debug!("填充稍后再看视频详情完成");
    }

    fn log_download_video_start(&self) {
        debug!("开始下载稍后再看视频..");
    }

    fn log_download_video_end(&self) {
        debug!("下载稍后再看视频完成");
    }

    fn scan_deleted_videos(&self) -> bool {
        self.scan_deleted_videos
    }

    fn source_type_display(&self) -> String {
        "稍后再看".to_string()
    }

    fn source_name_display(&self) -> String {
        "稍后再看".to_string()
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

    fn audio_only(&self) -> bool {
        self.audio_only
    }

    fn audio_only_m4a_only(&self) -> bool {
        self.audio_only_m4a_only
    }

    fn flat_folder(&self) -> bool {
        self.flat_folder
    }

    fn download_danmaku(&self) -> bool {
        self.download_danmaku
    }

    fn download_subtitle(&self) -> bool {
        self.download_subtitle
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
        format!("watch_later_{}", self.id)
    }
}

// 稍后观看源的初始化现在通过Web API完成，不再需要这个函数

pub(super) async fn watch_later_from<'a>(
    path: &Path,
    bili_client: &'a BiliClient,
    connection: &DatabaseConnection,
) -> Result<(
    VideoSourceEnum,
    Pin<Box<dyn Stream<Item = Result<VideoInfo>> + 'a + Send>>,
)> {
    let watch_later = WatchLater::new(bili_client);

    // 检查是否已存在，如果存在直接返回
    if let Some(existing) = watch_later::Entity::find().one(connection).await? {
        return Ok((existing.into(), Box::pin(watch_later.into_video_stream())));
    }

    // 不存在则创建新记录
    let result = watch_later::Entity::insert(watch_later::ActiveModel {
        path: Set(path.to_string_lossy().to_string()),
        created_at: Set(crate::utils::time_format::now_standard_string()),
        latest_row_at: Set("1970-01-01 00:00:00".to_string()),
        enabled: Set(true),
        scan_deleted_videos: Set(false),
        ..Default::default()
    })
    .exec(connection)
    .await?;

    Ok((
        watch_later::Entity::find_by_id(result.last_insert_id)
            .one(connection)
            .await?
            .context("watch_later not found")?
            .into(),
        Box::pin(watch_later.into_video_stream()),
    ))
}
