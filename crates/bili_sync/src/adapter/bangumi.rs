use std::path::Path;
use std::path::PathBuf;
use std::pin::Pin;

use anyhow::Result;
use chrono::{DateTime, Utc};
use futures::Stream;
use sea_orm::prelude::*;
use sea_orm::ActiveValue::Set;
use tracing::debug;
use tracing::info;

use bili_sync_entity::VideoSourceTrait;
use sea_orm::sea_query::SimpleExpr;

use crate::adapter::VideoSource;
use crate::bilibili::bangumi::Bangumi;
use crate::bilibili::{BiliClient, VideoInfo};

#[derive(Clone)]
pub struct BangumiSource {
    pub id: i32,
    pub name: String,
    pub latest_row_at: String,
    pub season_id: Option<String>,
    pub media_id: Option<String>,
    pub ep_id: Option<String>,
    pub path: PathBuf,
    pub download_all_seasons: bool,
    pub page_name_template: Option<String>,
    pub selected_seasons: Option<Vec<String>>,
    pub scan_deleted_videos: bool,
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

impl BangumiSource {
    /// 渲染番剧的 page_name，优先使用全局 bangumi_name 配置
    /// 智能检测同一集是否有多个版本，自动添加title后缀避免冲突
    pub async fn render_page_name(
        &self,
        video_model: &bili_sync_entity::video::Model,
        page_model: &bili_sync_entity::page::Model,
        db: &sea_orm::DatabaseConnection,
        api_title: Option<&str>, // 新增参数：API提供的番剧标题
    ) -> Result<String> {
        use crate::utils::format_arg::bangumi_page_format_args;

        // 获取最新的配置，而不是使用静态全局配置
        let current_config = crate::config::reload_config();

        // 优先级：全局 bangumi_name > 番剧自己的 page_name > 默认格式
        let mut template = if !current_config.bangumi_name.is_empty() {
            current_config.bangumi_name.to_string()
        } else if let Some(ref page_name) = self.page_name_template {
            page_name.clone()
        } else {
            "{{title}} S{{season_pad}}E{{pid_pad}} - {{ptitle}}".to_string()
        };

        // 智能检测：检查同一番剧源的同一集是否有多个不同版本
        if !template.contains("{{title}}") && !template.contains("{{ title }}") {
            let should_add_title = self.check_multiple_versions(video_model, db).await;

            if should_add_title {
                // 如果检测到多版本，自动添加title后缀
                template = format!("{}-{{{{title}}}}", template);
                info!(
                    "智能检测到番剧第{}集存在多个版本，自动添加title后缀: {}",
                    video_model.episode_number.unwrap_or(page_model.pid),
                    video_model.name
                );
            }
        }

        let format_args = bangumi_page_format_args(video_model, page_model, api_title);

        // 使用ConfigBundle的模板渲染功能以保持一致性
        let final_name = if template == current_config.bangumi_name {
            // 如果使用的是全局配置的模板，直接使用ConfigBundle渲染
            crate::config::with_config(|bundle| bundle.render_bangumi_template(&format_args))?
        } else {
            // 如果使用的是自定义模板，创建临时的handlebars实例
            let mut handlebars = handlebars::Handlebars::new();
            // 注册 truncate 辅助函数
            handlebars.register_helper(
                "truncate",
                Box::new(
                    |h: &handlebars::Helper,
                     _: &handlebars::Handlebars,
                     _: &handlebars::Context,
                     _: &mut handlebars::RenderContext,
                     out: &mut dyn handlebars::Output|
                     -> handlebars::HelperResult {
                        let s = h.param(0).and_then(|v| v.value().as_str()).unwrap_or("");
                        let len = h.param(1).and_then(|v| v.value().as_u64()).unwrap_or(0) as usize;
                        let result = if s.chars().count() > len {
                            s.chars().take(len).collect::<String>()
                        } else {
                            s.to_string()
                        };
                        out.write(&result)?;
                        Ok(())
                    },
                ),
            );
            crate::utils::filenamify::filenamify(&handlebars.render_template(&template, &format_args)?)
        };

        Ok(final_name)
    }

    /// 智能检测同一番剧源的同一集是否存在多个版本
    /// 通过检查相同episode_number的视频数量来判断
    async fn check_multiple_versions(
        &self,
        video_model: &bili_sync_entity::video::Model,
        db: &sea_orm::DatabaseConnection,
    ) -> bool {
        use bili_sync_entity::video;
        use sea_orm::*;

        let source_id = self.id;
        let episode_number = video_model.episode_number.unwrap_or(0);
        let season_id = &video_model.season_id;

        // 查询同一番剧源、同一季度、同一集数的视频数量
        let mut query = video::Entity::find()
            .filter(video::Column::SourceId.eq(source_id))
            .filter(video::Column::SourceType.eq(1)); // 番剧类型

        // 如果有episode_number，使用episode_number过滤
        if episode_number > 0 {
            query = query.filter(video::Column::EpisodeNumber.eq(episode_number));
        }

        // 如果有season_id，使用season_id过滤
        if let Some(season_id_value) = season_id {
            query = query.filter(video::Column::SeasonId.eq(season_id_value));
        }

        match query.count(db).await {
            Ok(count) => {
                debug!("番剧源{} 第{}集 共有{}个版本", source_id, episode_number, count);
                count > 1
            }
            Err(e) => {
                debug!("检查多版本失败: {}", e);
                false
            }
        }
    }

    /// 从缓存获取视频流
    pub async fn video_stream_from_cache(
        &self,
        cached_data: &str,
        latest_row_at: Option<DateTime<Utc>>,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<VideoInfo>> + Send>>> {
        use crate::bilibili::VideoInfo;
        use crate::utils::bangumi_cache::parse_cache;
        use async_stream::try_stream;

        let cache = parse_cache(cached_data)?;

        // 从缓存创建视频流
        let season_info = cache.season_info.clone();
        let episodes = cache.episodes;
        let season_id = self.season_id.clone();
        let _path = self.path.clone();

        Ok(Box::pin(try_stream! {
            // 从缓存的season_info中提取信息
            let cover = season_info["cover"].as_str().unwrap_or_default().to_string();
            let title = season_info["title"].as_str().unwrap_or_default().to_string();
            let intro = season_info["evaluate"].as_str().unwrap_or_default().to_string();
            let show_season_type = season_info["show_season_type"].as_i64().map(|v| v as i32);
            let actors = season_info["actors"].as_str().map(|s| s.to_string());

            // 使用缓存的数据生成视频流
            for episode in episodes {
                let pub_time = episode["pub_time"].as_i64().unwrap_or(0);
                let pub_datetime = DateTime::<Utc>::from_timestamp(pub_time, 0);

                // 如果设置了时间过滤，跳过旧剧集
                if let (Some(latest), Some(pub_dt)) = (latest_row_at, pub_datetime) {
                    if pub_dt <= latest {
                        continue;
                    }
                }

                // 检查是否为预告片
                if episode["section_type"].as_i64().unwrap_or(0) == 1 {
                    continue; // 跳过预告片
                }

                // 从缓存的剧集数据构建VideoInfo
                let bvid = episode["bvid"].as_str().unwrap_or_default().to_string();
                let ep_title = episode["title"].as_str().unwrap_or_default().to_string();
                let long_title = episode["long_title"].as_str().unwrap_or_default().to_string();
                let share_copy = episode["share_copy"].as_str().map(|s| s.to_string());
                let cid = episode["cid"].as_i64().unwrap_or(0);
                let _duration = episode["duration"].as_i64().unwrap_or(0) / 1000; // 毫秒转秒
                let episode_cover = episode["cover"].as_str().unwrap_or(&cover).to_string();

                // 解析集数
                let episode_number = ep_title.parse::<i32>().ok();

                let video_info = VideoInfo::Bangumi {
                    title: if long_title.is_empty() { title.clone() } else { long_title },
                    bvid: bvid.clone(),
                    cid: cid.to_string(),
                    ep_id: episode["id"].as_i64().unwrap_or(0).to_string(),
                    cover: episode_cover,
                    season_id: season_id.clone().unwrap_or_default(),
                    intro: intro.clone(),
                    pubtime: pub_datetime.unwrap_or_default(),
                    episode_number,
                    share_copy,
                    show_season_type,
                    show_title: Some(ep_title.clone()),
                    season_number: None, // 从缓存中无法获取季度编号
                    actors: actors.clone(),
                };

                yield video_info;
            }
        }))
    }

    pub async fn video_stream_from(
        &self,
        bili_client: &BiliClient,
        _path: &Path,
        connection: &sea_orm::DatabaseConnection,
    ) -> Result<Pin<Box<dyn Stream<Item = Result<VideoInfo>> + Send>>> {
        use crate::utils::bangumi_cache::is_cache_expired;
        use bili_sync_entity::video_source;

        // 获取当前番剧源的缓存信息
        let source_model = video_source::Entity::find_by_id(self.id)
            .one(connection)
            .await?
            .ok_or_else(|| anyhow::anyhow!("番剧源不存在"))?;

        // 检查是否是首次扫描：如果该源没有任何视频记录，应该进行全量获取
        let video_count = bili_sync_entity::video::Entity::find()
            .filter(bili_sync_entity::video::Column::SourceId.eq(self.id))
            .filter(bili_sync_entity::video::Column::SourceType.eq(1)) // 番剧类型
            .count(connection)
            .await?;

        let latest_row_at = if video_count == 0 {
            // 首次扫描，使用全量模式
            debug!("检测到新番剧源（无历史记录），启用全量获取模式");
            None
        } else {
            // 已有记录，使用增量模式
            Some(
                crate::utils::time_format::parse_time_string(&self.latest_row_at)
                    .unwrap_or_else(|| chrono::DateTime::from_timestamp(0, 0).unwrap().naive_utc())
                    .and_utc(),
            )
        };

        let bangumi = Bangumi::new(
            bili_client,
            self.media_id.clone(),
            self.season_id.clone(),
            self.ep_id.clone(),
        );

        // 检查缓存是否可用
        let use_cache = if let (Some(_cached_episodes), Some(cache_updated_at)) =
            (&source_model.cached_episodes, source_model.cache_updated_at)
        {
            // 检查缓存是否过期（默认24小时）
            let cache_updated_at_utc = crate::utils::time_format::parse_time_string(&cache_updated_at)
                .unwrap_or_else(|| chrono::DateTime::from_timestamp(0, 0).unwrap().naive_utc())
                .and_utc();
            if !is_cache_expired(Some(cache_updated_at_utc), 24) {
                // 尝试轻量级更新检查
                match bangumi.check_update(Some(cache_updated_at_utc)).await {
                    Ok((has_update, _)) => {
                        if !has_update {
                            // 没有更新，可以使用缓存
                            info!("番剧 {} 无更新，使用缓存数据", self.name);
                            true
                        } else {
                            info!("番剧 {} 检测到更新，需要重新获取", self.name);
                            false
                        }
                    }
                    Err(e) => {
                        warn!("检查番剧更新失败: {}，将重新获取完整数据", e);
                        false
                    }
                }
            } else {
                debug!("番剧缓存已过期，需要重新获取");
                false
            }
        } else {
            debug!("番剧无缓存，需要获取完整数据");
            false
        };

        if use_cache && source_model.cached_episodes.is_some() {
            // 使用缓存数据
            let cached_data = source_model.cached_episodes.unwrap();
            match self.video_stream_from_cache(&cached_data, latest_row_at).await {
                Ok(stream) => {
                    info!("成功从缓存加载番剧数据");
                    return Ok(stream);
                }
                Err(e) => {
                    warn!("从缓存加载番剧数据失败: {}，回退到API获取", e);
                }
            }
        }

        // 缓存不可用或加载失败，从API获取
        let mode_desc = if latest_row_at.is_some() { "增量" } else { "全量" };

        if self.download_all_seasons {
            debug!(
                "正在{}获取所有季度的番剧内容（时间过滤: {:?}）",
                mode_desc, latest_row_at
            );
            Ok(Box::pin(bangumi.to_all_seasons_video_stream_incremental(latest_row_at)))
        } else if let Some(ref selected_seasons) = self.selected_seasons {
            // 如果有选中的季度，只下载选中的季度
            debug!(
                "正在{}获取选中的 {} 个季度的番剧内容（时间过滤: {:?}）",
                mode_desc,
                selected_seasons.len(),
                latest_row_at
            );
            Ok(Box::pin(bangumi.to_selected_seasons_video_stream_incremental(
                selected_seasons.clone(),
                latest_row_at,
            )))
        } else {
            debug!(
                "正在{}获取当前季度的番剧内容（时间过滤: {:?}）",
                mode_desc, latest_row_at
            );
            // 单季度：获取并缓存season信息
            Ok(Box::pin(bangumi.to_video_stream_incremental(latest_row_at)))
        }
    }

    // 番剧源的初始化现在通过Web API完成，不再需要这个函数
}

impl VideoSourceTrait for BangumiSource {
    fn get_latest_row_at(&self) -> String {
        self.latest_row_at.clone()
    }

    fn log_refresh_video_start(&self) {
        info!("开始获取番剧 {} 的更新", self.name);
    }

    fn log_refresh_video_end(&self, count: usize) {
        if count > 0 {
            info!("番剧 {} 获取更新完毕，新增 {} 个视频", self.name, count);
        } else {
            info!("番剧 {} 无新视频", self.name);
        }
    }

    fn log_fetch_video_start(&self) {
        debug!("开始获取番剧 {} 的详细信息", self.name);
    }

    fn log_fetch_video_end(&self) {
        debug!("番剧 {} 的详细信息获取完毕", self.name);
    }

    fn log_download_video_start(&self) {
        debug!("开始下载番剧 {} 的视频", self.name);
    }

    fn log_download_video_end(&self) {
        debug!("番剧 {} 的视频下载完毕", self.name);
    }

    fn scan_deleted_videos(&self) -> bool {
        self.scan_deleted_videos
    }

    fn filter_expr(&self) -> SimpleExpr {
        bili_sync_entity::video::Column::SourceId
            .eq(self.id)
            .and(bili_sync_entity::video::Column::SourceType.eq(1))
    }

    fn should_take(&self, _release_datetime: &DateTime<Utc>, _latest_row_at_string: &str) -> bool {
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
        model.source_type = Set(Some(1));
    }
}

impl VideoSource for BangumiSource {
    fn filter_expr(&self) -> SimpleExpr {
        bili_sync_entity::video::Column::SourceId
            .eq(self.id)
            .and(bili_sync_entity::video::Column::SourceType.eq(1))
    }

    fn set_relation_id(&self, model: &mut bili_sync_entity::video::ActiveModel) {
        model.source_id = Set(Some(self.id));
        model.source_type = Set(Some(1));
    }

    fn get_latest_row_at(&self) -> String {
        self.latest_row_at.clone()
    }

    fn update_latest_row_at(&self, datetime: String) -> crate::adapter::_ActiveModel {
        let mut model = <bili_sync_entity::video_source::ActiveModel as sea_orm::ActiveModelTrait>::default();
        model.id = Set(self.id);
        model.latest_row_at = Set(datetime);
        crate::adapter::_ActiveModel::Bangumi(Box::new(model))
    }

    fn path(&self) -> &Path {
        &self.path
    }

    // 总是返回true，表示应该下载所有番剧内容，不管发布时间
    fn should_take(&self, _release_datetime: &chrono::DateTime<Utc>, _latest_row_at_string: &str) -> bool {
        true
    }

    fn log_refresh_video_start(&self) {
        info!("开始获取番剧 {} 的更新", self.name);
    }

    fn log_refresh_video_end(&self, count: usize) {
        if count > 0 {
            info!("番剧 {} 获取更新完毕，新增 {} 个视频", self.name, count);
        } else {
            info!("番剧 {} 无新视频", self.name);
        }
    }

    fn log_fetch_video_start(&self) {
        debug!("开始获取番剧 {} 的详细信息", self.name);
    }

    fn log_fetch_video_end(&self) {
        debug!("番剧 {} 的详细信息获取完毕", self.name);
    }

    fn log_download_video_start(&self) {
        debug!("开始下载番剧 {} 的视频", self.name);
    }

    fn log_download_video_end(&self) {
        debug!("番剧 {} 的视频下载完毕", self.name);
    }

    fn scan_deleted_videos(&self) -> bool {
        self.scan_deleted_videos
    }

    fn source_type_display(&self) -> String {
        "番剧".to_string()
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
        format!("bangumi_{}", self.id)
    }
}
