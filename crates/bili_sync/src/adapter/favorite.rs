use std::path::Path;
use std::pin::Pin;

use crate::utils::time_format::now_standard_string;
use anyhow::{Context, Result};
use bili_sync_entity::*;
use chrono::Utc;
use futures::Stream;
use sea_orm::entity::prelude::*;
use sea_orm::sea_query::{OnConflict, SimpleExpr};
use sea_orm::ActiveValue::Set;
use sea_orm::{DatabaseConnection, Unchanged};

use crate::adapter::{VideoSource, VideoSourceEnum, _ActiveModel};
use crate::bilibili::{BiliClient, FavoriteList, VideoInfo};

impl VideoSource for favorite::Model {
    fn filter_expr(&self) -> SimpleExpr {
        video::Column::FavoriteId.eq(self.id)
    }

    fn set_relation_id(&self, video_model: &mut video::ActiveModel) {
        video_model.favorite_id = Set(Some(self.id));
    }

    fn path(&self) -> &Path {
        Path::new(self.path.as_str())
    }

    fn get_latest_row_at(&self) -> String {
        self.latest_row_at.clone()
    }

    fn update_latest_row_at(&self, datetime: String) -> _ActiveModel {
        _ActiveModel::Favorite(favorite::ActiveModel {
            id: Unchanged(self.id),
            latest_row_at: Set(datetime),
            ..Default::default()
        })
    }

    fn should_take(&self, release_datetime: &chrono::DateTime<Utc>, latest_row_at_string: &str) -> bool {
        // 收藏夹接口按收藏时间（fav_time）从新到旧排序，可以做增量截断。
        // 但当开启“扫描已删除视频”时，需要全量拉取以确保正确性。
        if self.scan_deleted_videos {
            return true;
        }

        // 首次扫描或时间戳缺失：不做截断
        if latest_row_at_string.is_empty() || latest_row_at_string == "1970-01-01 00:00:00" {
            return true;
        }

        let beijing_tz = crate::utils::time_format::beijing_timezone();
        let release_beijing = release_datetime.with_timezone(&beijing_tz);
        let release_beijing_str = release_beijing.format("%Y-%m-%d %H:%M:%S").to_string();
        release_beijing_str.as_str() > latest_row_at_string
    }

    fn log_refresh_video_start(&self) {
        info!("开始扫描收藏夹「{}」..", self.name);
    }

    fn log_refresh_video_end(&self, count: usize) {
        if count > 0 {
            info!("扫描收藏夹「{}」完成，获取到 {} 条新视频", self.name, count);
        } else {
            info!("收藏夹「{}」无新视频", self.name);
        }
    }

    fn log_fetch_video_start(&self) {
        debug!("开始填充收藏夹「{}」视频详情..", self.name);
    }

    fn log_fetch_video_end(&self) {
        debug!("填充收藏夹「{}」视频详情完成", self.name);
    }

    fn log_download_video_start(&self) {
        debug!("开始下载收藏夹「{}」视频..", self.name);
    }

    fn log_download_video_end(&self) {
        debug!("下载收藏夹「{}」视频完成", self.name);
    }

    fn scan_deleted_videos(&self) -> bool {
        self.scan_deleted_videos
    }

    fn source_type_display(&self) -> String {
        "收藏夹".to_string()
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
        format!("favorite_{}", self.id)
    }
}

pub(super) async fn favorite_from<'a>(
    fid: &str,
    path: &Path,
    bili_client: &'a BiliClient,
    connection: &DatabaseConnection,
) -> Result<(
    VideoSourceEnum,
    Pin<Box<dyn Stream<Item = Result<VideoInfo>> + 'a + Send>>,
)> {
    let favorite = FavoriteList::new(bili_client, fid.to_owned());
    let favorite_info = favorite.get_info().await?;
    favorite::Entity::insert(favorite::ActiveModel {
        f_id: Set(favorite_info.id),
        name: Set(favorite_info.title.clone()),
        path: Set(path.to_string_lossy().to_string()),
        created_at: Set(now_standard_string()),
        latest_row_at: Set("1970-01-01 00:00:00".to_string()),
        enabled: Set(true),
        scan_deleted_videos: Set(false),
        ..Default::default()
    })
    .on_conflict(
        OnConflict::column(favorite::Column::FId)
            .update_columns([favorite::Column::Name, favorite::Column::Path])
            .to_owned(),
    )
    .exec(connection)
    .await?;
    Ok((
        favorite::Entity::find()
            .filter(favorite::Column::FId.eq(favorite_info.id))
            .one(connection)
            .await?
            .context("favorite not found")?
            .into(),
        Box::pin(favorite.into_video_stream()),
    ))
}
