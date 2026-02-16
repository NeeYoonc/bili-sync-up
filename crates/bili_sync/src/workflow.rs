use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use bili_sync_entity::*;
use chrono::Datelike;
use futures::stream::FuturesUnordered;
use futures::{Stream, StreamExt, TryStreamExt};
use sea_orm::entity::prelude::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{DatabaseBackend, Statement, TransactionTrait};
use tokio::fs;
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::utils::time_format::{now_naive, now_standard_string, parse_time_string};

// 全局番剧季度标题缓存
lazy_static::lazy_static! {
    pub static ref SEASON_TITLE_CACHE: Arc<Mutex<HashMap<String, String>>> = Arc::new(Mutex::new(HashMap::new()));
    static ref SUBMISSION_COLLECTION_META_CACHE: Arc<Mutex<HashMap<i64, HashMap<String, SubmissionCollectionMeta>>>> =
        Arc::new(Mutex::new(HashMap::new()));
}

#[derive(Debug, Clone)]
struct SubmissionCollectionMeta {
    name: String,
    cover: Option<String>,
    description: Option<String>,
}

use crate::adapter::{video_source_from, Args, VideoSource, VideoSourceEnum};
use crate::bilibili::{
    BestStream, BiliClient, BiliError, Collection, CollectionItem, CollectionType, Dimension, PageInfo,
    Stream as VideoStream, Video, VideoInfo,
};
use crate::config::ARGS;
use crate::error::{DownloadAbortError, ExecutionStatus, ProcessPageError};
use crate::task::{DeleteVideoTask, VIDEO_DELETE_TASK_QUEUE};
use crate::unified_downloader::UnifiedDownloader;
use crate::utils::format_arg::{collection_unified_page_format_args, page_format_args, video_format_args};
use crate::utils::model::{
    create_pages, create_videos, filter_unfilled_videos, filter_unhandled_video_pages,
    get_failed_videos_in_current_cycle, update_pages_model, update_videos_model,
};
use crate::utils::nfo::NFO;
use crate::utils::notification::NewVideoInfo;
use crate::utils::scan_collector::create_new_video_info;
use crate::utils::status::{PageStatus, VideoStatus, STATUS_OK};

fn is_bili_request_failed_404(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<BiliError>()
            .is_some_and(|e| matches!(e, BiliError::RequestFailed(-404, _)))
    })
}

fn is_submission_ugc_collection_video(video_source: &VideoSourceEnum, video_model: &video::Model) -> bool {
    matches!(video_source, VideoSourceEnum::Submission(_))
        && video_model.source_submission_id.is_some()
        && video_model
            .season_id
            .as_ref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
}

// 新增：番剧季信息结构体
#[derive(Debug, Clone)]
pub struct SeasonInfo {
    pub title: String,
    pub episodes: Vec<EpisodeInfo>,
    // API扩展字段
    pub alias: Option<String>,                 // 别名
    pub evaluate: Option<String>,              // 剧情简介
    pub rating: Option<f32>,                   // 评分 (如9.6)
    pub rating_count: Option<i64>,             // 评分人数
    pub areas: Vec<String>,                    // 制作地区 (如"中国大陆")
    pub actors: Option<String>,                // 声优演员信息 (格式化字符串)
    pub styles: Vec<String>,                   // 类型标签 (如"科幻", "机战")
    pub total_episodes: Option<i32>,           // 总集数
    pub status: Option<String>,                // 播出状态 (如"完结", "连载中")
    pub cover: Option<String>,                 // 季度封面图URL (竖版)
    pub series_cover: Option<String>,          // 系列根目录封面（优先使用第一季竖版封面）
    pub new_ep_cover: Option<String>,          // 新EP封面图URL (来自new_ep.cover)
    pub horizontal_cover_1610: Option<String>, // 16:10横版封面URL
    pub horizontal_cover_169: Option<String>,  // 16:9横版封面URL
    pub bkg_cover: Option<String>,             // 背景图URL (专门的背景图)
    pub media_id: Option<i64>,                 // 媒体ID
    pub season_id: String,                     // 季度ID
    pub publish_time: Option<String>,          // 发布时间
    pub total_views: Option<i64>,              // 总播放量
    pub total_favorites: Option<i64>,          // 总收藏数
    pub show_season_type: Option<i32>,         // 番剧季度类型
}

#[derive(Debug, Clone)]
pub struct EpisodeInfo {
    pub ep_id: String,
    pub cid: i64,
    pub duration: u32, // 秒
}

/// 创建一个配置了 truncate 辅助函数的 handlebars 实例
///
/// 完整地处理某个视频来源，返回新增的视频数量和视频信息
pub async fn process_video_source(
    args: &Args,
    bili_client: &BiliClient,
    path: &Path,
    connection: &DatabaseConnection,
    downloader: &UnifiedDownloader,
    token: CancellationToken,
) -> Result<(usize, Vec<NewVideoInfo>)> {
    // 记录当前处理的参数和路径
    if let Args::Bangumi {
        season_id,
        media_id: _,
        ep_id: _,
    } = &args
    {
        // 尝试从API获取真实的番剧标题
        let title = if let Some(season_id) = season_id {
            // 如果有season_id，尝试获取番剧标题
            get_season_title_from_api(bili_client, season_id, token.clone())
                .await
                .unwrap_or_else(|| {
                    // API获取失败，回退到路径名
                    path.file_name()
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or_else(|| "未知番剧".to_string())
                })
        } else {
            // 没有season_id，使用路径名
            path.file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| "未知番剧".to_string())
        };
        info!("处理番剧下载: {}", title);
    }

    // 定义一个辅助函数来处理-101错误并重试
    let retry_with_refresh = |error_msg: String| async move {
        if error_msg.contains("status code: -101") || error_msg.contains("账号未登录") {
            warn!("检测到登录状态过期，尝试刷新凭据...");
            if let Err(refresh_err) = bili_client.check_refresh().await {
                error!("刷新凭据失败：{:#}", refresh_err);
                return Err(refresh_err);
            } else {
                info!("凭据刷新成功，将重试操作");
                return Ok(());
            }
        }
        Err(anyhow::anyhow!("非登录状态错误，无需刷新凭据"))
    };

    // 从参数中获取视频列表的 Model 与视频流
    let (video_source, video_streams) =
        match video_source_from(args, path, bili_client, connection, Some(token.clone())).await {
            Ok(result) => result,
            Err(e) => {
                let error_msg = format!("{:#}", e);
                if retry_with_refresh(error_msg).await.is_ok() {
                    // 刷新成功，重试
                    video_source_from(args, path, bili_client, connection, Some(token.clone())).await?
                } else {
                    return Err(e);
                }
            }
        };

    // 从视频流中获取新视频的简要信息，写入数据库，并获取新增视频数量和信息
    let (new_video_count, new_videos) =
        match refresh_video_source(&video_source, video_streams, connection, token.clone(), bili_client).await {
            Ok(result) => result,
            Err(e) => {
                let error_msg = format!("{:#}", e);
                if retry_with_refresh(error_msg).await.is_ok() {
                    // 刷新成功，重新获取视频流并重试
                    let (_, video_streams) =
                        video_source_from(args, path, bili_client, connection, Some(token.clone())).await?;
                    refresh_video_source(&video_source, video_streams, connection, token.clone(), bili_client).await?
                } else {
                    return Err(e);
                }
            }
        };

    // Guard: skip further steps if paused/cancelled or no new videos in this round
    if crate::task::TASK_CONTROLLER.is_paused() || token.is_cancelled() {
        info!("任务已暂停/取消，跳过详情与下载阶段");
        return Ok((new_video_count, new_videos));
    }
    if new_video_count == 0 {
        let has_unfilled = !filter_unfilled_videos(video_source.filter_expr(), connection)
            .await?
            .is_empty();
        let has_unhandled = !filter_unhandled_video_pages(video_source.filter_expr(), connection)
            .await?
            .is_empty();
        let has_failed = !get_failed_videos_in_current_cycle(video_source.filter_expr(), connection)
            .await?
            .is_empty();
        if !(has_unfilled || has_unhandled || has_failed) {
            info!("本轮未发现新视频，且无待处理任务，跳过详情与下载阶段");
            return Ok((new_video_count, new_videos));
        } else {
            info!("本轮未发现新视频，但存在待处理任务（重置/未完成/可重试），继续执行下载阶段");
        }
    }

    // 单独请求视频详情接口，获取视频的详情信息与所有的分页，写入数据库
    if let Err(e) = fetch_video_details(bili_client, &video_source, connection, token.clone()).await {
        // 新增：检查是否为风控导致的下载中止
        if e.downcast_ref::<DownloadAbortError>().is_some() {
            error!("获取视频详情时触发风控，已终止当前视频源的处理，停止所有后续扫描");
            // 风控时应该返回错误，中断整个扫描循环，而不是继续处理下一个视频源
            return Err(e);
        }

        let error_msg = format!("{:#}", e);
        if retry_with_refresh(error_msg).await.is_ok() {
            // 刷新成功，重试
            fetch_video_details(bili_client, &video_source, connection, token.clone()).await?;
        } else {
            return Err(e);
        }
    }

    if ARGS.scan_only {
        warn!("已开启仅扫描模式，跳过视频下载..");
    } else {
        // 从数据库中查找所有未下载的视频与分页，下载并处理
        if let Err(e) =
            download_unprocessed_videos(bili_client, &video_source, connection, downloader, token.clone()).await
        {
            let error_msg = format!("{:#}", e);
            if retry_with_refresh(error_msg).await.is_ok() {
                // 刷新成功，重试（继续使用原有的取消令牌）
                download_unprocessed_videos(bili_client, &video_source, connection, downloader, token.clone()).await?;
            } else {
                return Err(e);
            }
        }

        // 新增：循环内重试失败的视频
        // 在当前扫描循环结束前，对失败的视频进行一次额外的重试机会
        if let Err(e) =
            retry_failed_videos_once(bili_client, &video_source, connection, downloader, token.clone()).await
        {
            warn!("循环内重试失败的视频时出错: {:#}", e);
            // 重试失败不中断主流程，继续执行
        }

        // 批量 AI 重命名：在所有视频下载完成后统一执行
        // 这样可以避免单个视频重命名时导致的路径冲突问题
        if let Err(e) = batch_ai_rename_for_source(&video_source, connection).await {
            warn!("批量 AI 重命名失败: {:#}", e);
        }

        // 注意：一致性检查已移除
        // 批量处理模式下，所有文件在同一会话中统一命名，天然保证一致性
        // 额外的一致性检查反而可能产生误判（如将含有详细信息的文件名错误地"简化"）
    }
    Ok((new_video_count, new_videos))
}

/// 更新番剧缓存
async fn update_bangumi_cache(
    source_id: i32,
    connection: &DatabaseConnection,
    bili_client: &BiliClient,
    season_info: Option<SeasonInfo>,
) -> Result<()> {
    use crate::utils::bangumi_cache::{serialize_cache, BangumiCache};
    use bili_sync_entity::video_source;
    use sea_orm::ActiveValue::Set;

    // 获取番剧源信息
    let source = video_source::Entity::find_by_id(source_id)
        .one(connection)
        .await?
        .ok_or_else(|| anyhow::anyhow!("番剧源不存在"))?;

    // 如果没有提供season_info，尝试从API获取
    let season_info = if let Some(info) = season_info {
        info
    } else if let Some(season_id) = &source.season_id {
        // 从API获取完整的season信息
        match get_season_info_from_api(bili_client, season_id, CancellationToken::new()).await {
            Ok(info) => info,
            Err(e) => {
                warn!("获取番剧季信息失败，跳过缓存更新: {}", e);
                return Ok(());
            }
        }
    } else {
        debug!("番剧源 {} 没有season_id，跳过缓存更新", source_id);
        return Ok(());
    };

    // 构建episodes数组
    let mut episodes = Vec::new();

    // 查询该番剧源的所有视频和分页信息
    let videos_with_pages = bili_sync_entity::video::Entity::find()
        .filter(bili_sync_entity::video::Column::SourceId.eq(source_id))
        .filter(bili_sync_entity::video::Column::SourceType.eq(1))
        .find_with_related(bili_sync_entity::page::Entity)
        .all(connection)
        .await?;

    // 从数据库记录构建episodes信息
    for (video, pages) in &videos_with_pages {
        if let Some(page) = pages.first() {
            let mut episode = serde_json::json!({
                "id": video.ep_id.as_ref().and_then(|s| s.parse::<i64>().ok()).unwrap_or(0),
                "aid": video.bvid.clone(), // 暂时使用bvid，实际应该是aid
                "bvid": video.bvid.clone(),
                "cid": page.cid,
                "title": video.episode_number.map(|n| n.to_string()).unwrap_or_else(|| video.name.clone()),
                "long_title": video.name.clone(),
                "cover": video.cover.clone(),
                "duration": page.duration as i64 * 1000, // 秒转毫秒
                "pub_time": video.pubtime.and_utc().timestamp(),
                "section_type": 0, // 正片
            });

            // 如果有share_copy，添加到episode中
            if let Some(share_copy) = &video.share_copy {
                episode["share_copy"] = serde_json::Value::String(share_copy.clone());
            }

            episodes.push(episode);
        }
    }

    // 如果没有视频数据，使用API提供的episodes
    if episodes.is_empty() && !season_info.episodes.is_empty() {
        for ep_info in &season_info.episodes {
            episodes.push(serde_json::json!({
                "id": ep_info.ep_id.parse::<i64>().unwrap_or(0),
                "cid": ep_info.cid,
                "duration": ep_info.duration as i64 * 1000, // 秒转毫秒
                "section_type": 0,
            }));
        }
    }

    // 构建season_info JSON
    let season_json = serde_json::json!({
        "title": season_info.title,
        "cover": season_info.cover,
        "evaluate": season_info.evaluate,
        "show_season_type": season_info.show_season_type,
        "actors": season_info.actors,
        "rating": season_info.rating,
        "areas": season_info.areas,
        "styles": season_info.styles,
        "total": season_info.total_episodes,
        "new_ep": {
            "cover": season_info.new_ep_cover,
        },
        "horizontal_cover_1610": season_info.horizontal_cover_1610,
        "horizontal_cover_169": season_info.horizontal_cover_169,
        "bkg_cover": season_info.bkg_cover,
    });

    // 获取最新的剧集时间
    let last_episode_time = videos_with_pages.iter().map(|(v, _)| v.pubtime.and_utc()).max();

    // 创建缓存对象
    let cache = BangumiCache {
        season_info: season_json,
        episodes: episodes.clone(),
        last_episode_time,
        total_episodes: season_info.total_episodes.unwrap_or(episodes.len() as i32) as usize,
    };

    // 序列化缓存
    let cache_json = serialize_cache(&cache)?;

    // 更新数据库
    let active_model = video_source::ActiveModel {
        id: Set(source_id),
        cached_episodes: Set(Some(cache_json)),
        cache_updated_at: Set(Some(crate::utils::time_format::now_standard_string())),
        ..Default::default()
    };

    active_model.update(connection).await?;

    // 触发异步同步到内存DB

    info!(
        "番剧源 {} ({}) 缓存更新成功，共 {} 集",
        source_id,
        season_info.title,
        episodes.len()
    );

    Ok(())
}

/// 请求接口，获取视频列表中所有新添加的视频信息，将其写入数据库
pub async fn refresh_video_source<'a>(
    video_source: &VideoSourceEnum,
    video_streams: Pin<Box<dyn Stream<Item = Result<VideoInfo>> + 'a + Send>>,
    connection: &DatabaseConnection,
    token: CancellationToken,
    bili_client: &BiliClient,
) -> Result<(usize, Vec<NewVideoInfo>)> {
    video_source.log_refresh_video_start();
    let latest_row_at_string = video_source.get_latest_row_at();
    let latest_row_at = crate::utils::time_format::parse_time_string(&latest_row_at_string)
        .unwrap_or_else(|| chrono::DateTime::from_timestamp(0, 0).unwrap().naive_utc())
        .and_utc();
    let mut max_datetime = latest_row_at;

    async fn ingest_batch(
        video_source: &VideoSourceEnum,
        connection: &DatabaseConnection,
        videos_info: Vec<VideoInfo>,
        count: &mut usize,
        new_videos: &mut Vec<NewVideoInfo>,
    ) -> Result<()> {
        // 获取插入前的视频数量
        let before_count = get_video_count_for_source(video_source, connection).await?;

        // 先收集需要的视频信息（包括集数信息和ep_id）
        let mut temp_video_infos = Vec::new();
        for video_info in &videos_info {
            let (title, bvid, upper_name, episode_num, ep_id) = match video_info {
                VideoInfo::Detail { title, bvid, upper, .. } => {
                    (title.clone(), bvid.clone(), upper.name.clone(), None, None)
                }
                VideoInfo::Favorite { title, bvid, upper, .. } => {
                    (title.clone(), bvid.clone(), upper.name.clone(), None, None)
                }
                VideoInfo::Collection { title, bvid, arc, .. } => {
                    // 从arc字段中提取upper信息
                    let upper_name = arc
                        .as_ref()
                        .and_then(|a| a["author"]["name"].as_str())
                        .unwrap_or("未知")
                        .to_string();
                    (title.clone(), bvid.clone(), upper_name, None, None)
                }
                VideoInfo::WatchLater { title, bvid, upper, .. } => {
                    (title.clone(), bvid.clone(), upper.name.clone(), None, None)
                }
                VideoInfo::Submission { title, bvid, .. } => {
                    // Submission 没有 upper 信息，使用默认值
                    (title.clone(), bvid.clone(), "未知".to_string(), None, None)
                }
                VideoInfo::Dynamic { title, bvid, .. } => (title.clone(), bvid.clone(), "未知".to_string(), None, None),
                VideoInfo::Bangumi {
                    title,
                    bvid,
                    episode_number,
                    ep_id,
                    ..
                } => {
                    // Bangumi 包含 ep_id 信息，用于唯一标识
                    (
                        title.clone(),
                        bvid.clone(),
                        "番剧".to_string(),
                        *episode_number,
                        Some(ep_id.clone()),
                    )
                }
            };
            temp_video_infos.push((title, bvid, upper_name, episode_num, ep_id));
        }

        // 获取所有视频的BVID，用于后续判断哪些是新增的
        let video_bvids: Vec<String> = videos_info
            .iter()
            .map(|v| match v {
                VideoInfo::Detail { bvid, .. } => bvid.clone(),
                VideoInfo::Favorite { bvid, .. } => bvid.clone(),
                VideoInfo::Collection { bvid, .. } => bvid.clone(),
                VideoInfo::WatchLater { bvid, .. } => bvid.clone(),
                VideoInfo::Submission { bvid, .. } => bvid.clone(),
                VideoInfo::Dynamic { bvid, .. } => bvid.clone(),
                VideoInfo::Bangumi { bvid, .. } => bvid.clone(),
            })
            .collect();

        create_videos(videos_info, video_source, connection).await?;

        // 获取插入后的视频数量，计算实际新增数量
        let after_count = get_video_count_for_source(video_source, connection).await?;
        let new_count = after_count - before_count;
        *count += new_count;

        // 如果有新增视频，通过查询数据库来确定哪些是新增的
        if new_count > 0 {
            // 查询这批视频中哪些是新插入的（根据创建时间）
            let now = crate::utils::time_format::beijing_now();
            let recent_threshold = now - chrono::Duration::seconds(10); // 10秒内创建的视频

            let newly_inserted = video::Entity::find()
                .filter(video_source.filter_expr())
                .filter(video::Column::Bvid.is_in(video_bvids.clone()))
                .filter(video::Column::CreatedAt.gte(recent_threshold.format("%Y-%m-%d %H:%M:%S").to_string()))
                .all(connection)
                .await?;

            debug!("查询到 {} 个新插入的视频记录", newly_inserted.len());

            // 为每个新插入的视频创建通知信息
            for new_video in newly_inserted {
                // 查找对应的视频信息，对番剧使用ep_id进行精确匹配
                let video_info_idx = if new_video.source_type == Some(1) && new_video.ep_id.is_some() {
                    // 番剧：使用ep_id匹配
                    temp_video_infos.iter().position(
                        |(_, _, _, _, ep_id): &(String, String, String, Option<i32>, Option<String>)| {
                            ep_id.as_ref() == new_video.ep_id.as_ref()
                        },
                    )
                } else {
                    // 其他类型：使用bvid匹配
                    temp_video_infos
                        .iter()
                        .position(|(_, bvid, _, _, _)| bvid == &new_video.bvid)
                };

                if let Some(idx) = video_info_idx {
                    let (title, _, _upper_name, bangumi_episode, _) = &temp_video_infos[idx];

                    // 使用数据库中的发布时间（已经是北京时间）
                    let pubtime = new_video.pubtime.format("%Y-%m-%d %H:%M:%S").to_string();

                    // 获取集数信息
                    let episode_number = if let Some(ep) = bangumi_episode {
                        // 番剧：使用从VideoInfo中获取的集数
                        Some(*ep)
                    } else {
                        // 其他类型：使用数据库中的episode_number字段
                        new_video.episode_number
                    };

                    let mut info = create_new_video_info(title, &new_video.bvid);
                    info.pubtime = Some(pubtime);
                    info.episode_number = episode_number;
                    info.video_id = Some(new_video.id);
                    new_videos.push(info);
                }
            }

            debug!("实际收集到 {} 个新视频信息用于推送", new_videos.len());
        }

        Ok(())
    }

    let mut count = 0;
    let mut new_videos = Vec::new();
    let mut buffer: Vec<VideoInfo> = Vec::with_capacity(10);
    let mut skipped_first_old = false;
    let mut video_streams = video_streams;

    while let Some(res) = video_streams.next().await {
        // 在处理每条视频前检查取消状态
        if token.is_cancelled() || crate::task::TASK_CONTROLLER.is_paused() {
            warn!("视频源处理过程中检测到取消/暂停信号，停止处理");
            break;
        }

        let video_info = match res {
            Ok(v) => v,
            Err(e) => {
                // 尽量保留已拉取到的内容，避免因为后续分页失败而完全丢弃本轮进度
                if !buffer.is_empty() {
                    let videos_info = std::mem::take(&mut buffer);
                    ingest_batch(video_source, connection, videos_info, &mut count, &mut new_videos).await?;
                }
                return Err(e);
            }
        };

        // 虽然 video_streams 是从新到旧的，但由于此处是分页请求，极端情况下可能发生访问完第一页时插入了两整页视频的情况
        // 此时获取到的第二页视频比第一页的还要新，因此为了确保正确，理应对每一页的第一个视频进行时间比较
        // 但在 streams 的抽象下，无法判断具体是在哪里分页的，所以暂且对每个视频都进行比较，应该不会有太大性能损失
        let release_datetime = video_info.release_datetime();
        if release_datetime > &max_datetime {
            max_datetime = *release_datetime;
        }

        // 增量截断：遇到旧视频则结束扫描
        if !video_source.should_take(release_datetime, latest_row_at_string.as_str()) {
            if !skipped_first_old && video_source.allow_skip_first_old() {
                skipped_first_old = true;
                continue;
            }
            break;
        }

        buffer.push(video_info);
        if buffer.len() >= 10 {
            let videos_info = std::mem::take(&mut buffer);
            ingest_batch(video_source, connection, videos_info, &mut count, &mut new_videos).await?;
        }
    }

    if !buffer.is_empty() && !(token.is_cancelled() || crate::task::TASK_CONTROLLER.is_paused()) {
        let videos_info = std::mem::take(&mut buffer);
        ingest_batch(video_source, connection, videos_info, &mut count, &mut new_videos).await?;
    }
    if max_datetime != latest_row_at {
        // 转换为北京时间的标准字符串格式
        let beijing_datetime = max_datetime.with_timezone(&crate::utils::time_format::beijing_timezone());
        let beijing_datetime_string = beijing_datetime.format("%Y-%m-%d %H:%M:%S").to_string();
        video_source
            .update_latest_row_at(beijing_datetime_string)
            .save(connection)
            .await?;
    }

    // 番剧源：更新缓存
    if let VideoSourceEnum::BangumiSource(bangumi_source) = video_source {
        // 检查是否需要更新缓存
        let should_update_cache = if count > 0 || max_datetime != latest_row_at {
            // 有新视频或时间更新，说明获取了新数据
            true
        } else {
            // 检查缓存是否存在
            let source_model = bili_sync_entity::video_source::Entity::find_by_id(bangumi_source.id)
                .one(connection)
                .await?;

            if let Some(source) = source_model {
                // 如果缓存不存在，需要创建
                source.cached_episodes.is_none()
            } else {
                false
            }
        };

        if should_update_cache {
            update_bangumi_cache(bangumi_source.id, connection, bili_client, None).await?;
        }
    }

    // 合集源：检查并更新 episode_number
    if let VideoSourceEnum::Collection(collection_source) = video_source {
        // 检查是否有视频缺少 episode_number
        let videos_without_episode = video::Entity::find()
            .filter(video::Column::CollectionId.eq(collection_source.id))
            .filter(video::Column::EpisodeNumber.is_null())
            .count(connection)
            .await?;

        if videos_without_episode > 0 {
            info!(
                "合集「{}」有 {} 个视频缺少集数序号，正在从API获取正确顺序...",
                collection_source.name, videos_without_episode
            );
            // 获取任意一个视频的bvid来触发更新
            if let Some(any_video) = video::Entity::find()
                .filter(video::Column::CollectionId.eq(collection_source.id))
                .one(connection)
                .await?
            {
                match get_collection_video_episode_number(connection, collection_source.id, &any_video.bvid).await {
                    Ok(_) => {
                        info!("合集「{}」的视频集数序号已更新", collection_source.name);
                    }
                    Err(e) => {
                        warn!("更新合集「{}」视频集数序号失败: {}", collection_source.name, e);
                    }
                }
            }
        }
    }

    video_source.log_refresh_video_end(count);
    debug!("workflow返回: count={}, new_videos.len()={}", count, new_videos.len());
    Ok((count, new_videos))
}

/// 筛选出所有未获取到全部信息的视频，尝试补充其详细信息
pub async fn fetch_video_details(
    bili_client: &BiliClient,
    video_source: &VideoSourceEnum,
    connection: &DatabaseConnection,
    token: CancellationToken,
) -> Result<()> {
    // Early exit when paused/cancelled
    if crate::task::TASK_CONTROLLER.is_paused() || token.is_cancelled() {
        info!("任务已暂停/取消，跳过视频详情阶段");
        return Ok(());
    }
    video_source.log_fetch_video_start();

    // 投稿源兜底：当详情接口未返回ugc_season时，使用UP主 lists(合集/系列) 的归属关系回填。
    // key=bvid, value=(collection_key, episode_number)
    let (submission_source_id, submission_collection_membership): (Option<i32>, Arc<HashMap<String, (String, i32)>>) =
        if let VideoSourceEnum::Submission(submission_source) = video_source {
            match build_submission_collection_membership_map(bili_client, submission_source.upper_id, token.clone())
                .await
            {
                Ok(map) => {
                    if !map.is_empty() {
                        info!(
                            "投稿源「{}」已加载合集/系列归属映射 {} 条",
                            submission_source.upper_name,
                            map.len()
                        );
                    }
                    (Some(submission_source.id), Arc::new(map))
                }
                Err(err) => {
                    warn!("加载投稿合集/系列归属映射失败，将仅使用详情接口结果: {}", err);
                    (Some(submission_source.id), Arc::new(HashMap::new()))
                }
            }
        } else {
            (None, Arc::new(HashMap::new()))
        };

    if let Some(source_id) = submission_source_id {
        if !submission_collection_membership.is_empty() {
            match backfill_submission_collection_membership(connection, source_id, &submission_collection_membership)
                .await
            {
                Ok(updated) if updated > 0 => {
                    info!("投稿源归属回填完成：本轮修正 {} 条历史视频归属", updated);
                }
                Ok(_) => {}
                Err(err) => warn!("投稿源归属回填失败（不影响后续流程）: {}", err),
            }
        }
    }

    let videos_model = filter_unfilled_videos(video_source.filter_expr(), connection).await?;

    // 分离出番剧和普通视频
    let (bangumi_videos, normal_videos): (Vec<_>, Vec<_>) =
        videos_model.into_iter().partition(|v| v.source_type == Some(1));

    // 优化后的番剧信息获取 - 使用数据库缓存和按季分组
    if !bangumi_videos.is_empty() {
        info!("开始处理 {} 个番剧视频", bangumi_videos.len());

        // 按 season_id 分组番剧视频
        let mut videos_by_season: HashMap<String, Vec<video::Model>> = HashMap::new();
        let mut videos_without_season = Vec::new();

        for video in bangumi_videos {
            if let Some(season_id) = &video.season_id {
                videos_by_season.entry(season_id.clone()).or_default().push(video);
            } else {
                videos_without_season.push(video);
            }
        }

        // 处理每个季
        for (season_id, videos) in videos_by_season {
            // 首先尝试获取番剧季标题用于日志显示
            let season_title = get_season_title_from_api(bili_client, &season_id, token.clone()).await;
            let display_name = season_title.as_deref().unwrap_or(&season_id);

            info!(
                "处理番剧季 {} 「{}」的 {} 个视频",
                season_id,
                display_name,
                videos.len()
            );

            // 1. 首先从现有数据库中查找该季已有的分集信息
            let mut existing_episodes =
                get_existing_episodes_for_season(connection, &season_id, bili_client, token.clone()).await?;

            // 2. 检查哪些ep_id还没有信息
            let missing_ep_ids: Vec<String> = videos
                .iter()
                .filter_map(|v| v.ep_id.as_ref())
                .filter(|ep_id| !existing_episodes.contains_key(*ep_id))
                .cloned()
                .collect();

            // 3. 只对缺失的信息发起API请求（每个季只请求一次）
            if !missing_ep_ids.is_empty() {
                info!(
                    "需要从API获取番剧季 {} 「{}」的信息（包含 {} 个新分集）",
                    season_id,
                    display_name,
                    missing_ep_ids.len()
                );

                match get_season_info_from_api(bili_client, &season_id, token.clone()).await {
                    Ok(season_info) => {
                        // 将新获取的信息添加到映射中
                        for episode in season_info.episodes {
                            existing_episodes.insert(episode.ep_id, (episode.cid, episode.duration));
                        }
                        debug!("成功获取番剧季 {} 「{}」的完整信息", season_id, season_info.title);
                    }
                    Err(e) => {
                        error!("获取番剧季 {} 「{}」信息失败: {}", season_id, display_name, e);
                        // 即使API失败，已有缓存的分集仍可正常处理
                    }
                }
            } else {
                info!(
                    "番剧季 {} 「{}」的所有分集信息已缓存，无需API请求",
                    season_id, display_name
                );
            }

            // 4. 使用合并后的信息处理所有视频
            for video_model in videos {
                if let Err(e) = process_bangumi_video(
                    bili_client,
                    video_model,
                    &existing_episodes,
                    connection,
                    video_source,
                    token.clone(),
                )
                .await
                {
                    error!("处理番剧视频失败: {}", e);
                }
            }
        }

        // 处理没有season_id的番剧视频（回退到原逻辑）
        if !videos_without_season.is_empty() {
            warn!(
                "发现 {} 个缺少season_id的番剧视频，使用原有逻辑处理",
                videos_without_season.len()
            );
            for video_model in videos_without_season {
                let txn = connection.begin().await?;

                let (actual_cid, duration) = if let Some(ep_id) = &video_model.ep_id {
                    match get_bangumi_info_from_api(bili_client, ep_id, token.clone()).await {
                        Some(info) => info,
                        None => {
                            error!("番剧 {} (EP{}) 信息获取失败，将跳过弹幕下载", &video_model.name, ep_id);
                            (-1, 1440)
                        }
                    }
                } else {
                    error!("番剧 {} 缺少EP ID，无法获取详细信息", &video_model.name);
                    (-1, 1440)
                };

                let page_info = PageInfo {
                    cid: actual_cid,
                    page: 1,
                    name: video_model.name.clone(),
                    duration,
                    first_frame: None,
                    dimension: None,
                };

                create_pages(vec![page_info], &video_model, &txn).await?;

                let mut video_active_model: bili_sync_entity::video::ActiveModel = video_model.into();
                video_source.set_relation_id(&mut video_active_model);
                video_active_model.single_page = Set(Some(true));
                video_active_model.tags = Set(Some(serde_json::Value::Array(vec![])));
                video_active_model.save(&txn).await?;
                txn.commit().await?;
            }
        }
    }

    // 处理普通视频 - 使用并发处理优化性能
    if !normal_videos.is_empty() {
        info!("开始并发处理 {} 个普通视频的详情", normal_videos.len());

        // 使用信号量控制并发数
        let current_config = crate::config::reload_config();
        let semaphore = Semaphore::new(current_config.concurrent_limit.video);

        let tasks = normal_videos
            .into_iter()
            .map(|video_model| {
                let semaphore = &semaphore;
                let token = token.clone();
                let submission_collection_membership = Arc::clone(&submission_collection_membership);
                async move {
                    // 获取许可以控制并发
                    let _permit = tokio::select! {
                        biased;
                        _ = token.cancelled() => return Err(anyhow!("Download cancelled")),
                        permit = semaphore.acquire() => permit.context("acquire semaphore failed")?,
                    };

                    let video = Video::new(bili_client, video_model.bvid.clone());
                    let info: Result<_> = tokio::select! {
                        biased;
                        _ = token.cancelled() => return Err(anyhow!("Download cancelled")),
                        res = async { Ok((video.get_tags().await?, video.get_view_info().await?)) } => res,
                    };
                    match info {
                        Err(e) => {
                            // 新增：检查是否为风控错误
                            let classified_error = crate::error::ErrorClassifier::classify_error(&e);
                            if classified_error.error_type == crate::error::ErrorType::RiskControl {
                                error!(
                                    "获取视频 {} - {} 的详细信息时触发风控: {}",
                                    &video_model.bvid, &video_model.name, classified_error.message
                                );
                                // 返回一个特定的错误来中止整个批处理
                                return Err(anyhow!(DownloadAbortError()));
                            }

                            error!(
                                "获取视频 {} - {} 的详细信息失败，错误为：{:#}",
                                &video_model.bvid, &video_model.name, e
                            );
                            if is_bili_request_failed_404(&e) {
                                // 404：视频已被删除/不可访问
                                // 若这是“重置详情”导致的回填（数据库里已有 page.path），则需要：
                                // - 跳过本次重置
                                // - 恢复为未重置（把状态置为完成，避免每轮都重复尝试）
                                // - 提醒用户
                                use sea_orm::sea_query::Expr;
                                use sea_orm::{Set, Unchanged};

                                let video_id = video_model.id;
                                let (pages_total, pages_with_path) = tokio::try_join!(
                                    page::Entity::find()
                                        .filter(page::Column::VideoId.eq(video_id))
                                        .count(connection),
                                    page::Entity::find()
                                        .filter(page::Column::VideoId.eq(video_id))
                                        .filter(page::Column::Path.is_not_null())
                                        .count(connection),
                                )?;

                                let should_restore_unreset = pages_with_path > 0;

                                // 无论是否需要恢复，都把视频标记为无效（避免后续流程继续处理）
                                let txn = connection.begin().await?;
                                video::Entity::update(video::ActiveModel {
                                    id: Unchanged(video_id),
                                    valid: Set(false),
                                    // 如果 single_page 被重置为空，基于现有 pages 数量做一个合理回填，避免反复进入详情阶段
                                    single_page: Set(Some(pages_total <= 1)),
                                    // 这里不写 download_status，统一在后续 restore 分支中写入
                                    ..Default::default()
                                })
                                .exec(&txn)
                                .await?;

                                if should_restore_unreset {
                                    warn!(
                                        "视频「{}」({}) 已在B站删除/不可访问，已跳过重置并恢复为未重置状态",
                                        &video_model.name, &video_model.bvid
                                    );

                                    let ok_video_status: u32 = VideoStatus::from([STATUS_OK; 5]).into();
                                    let ok_page_status: u32 = PageStatus::from([STATUS_OK; 5]).into();

                                    video::Entity::update(video::ActiveModel {
                                        id: Unchanged(video_id),
                                        download_status: Set(ok_video_status),
                                        ..Default::default()
                                    })
                                    .exec(&txn)
                                    .await?;

                                    page::Entity::update_many()
                                        .col_expr(page::Column::DownloadStatus, Expr::value(ok_page_status))
                                        .filter(page::Column::VideoId.eq(video_id))
                                        .exec(&txn)
                                        .await?;
                                } else {
                                    warn!(
                                        "视频「{}」({}) 已在B站删除/不可访问，已标记为无效并跳过处理",
                                        &video_model.name, &video_model.bvid
                                    );
                                }

                                txn.commit().await?;
                            } else {
                                // 非404错误发送通知（404是视频被删除的正常情况）
                                let video_name = video_model.name.clone();
                                let bvid = video_model.bvid.clone();
                                let error_msg = format!("{:#}", e);
                                tokio::spawn(async move {
                                    use crate::utils::notification::send_error_notification;
                                    if let Err(notify_err) = send_error_notification(
                                        "API请求失败",
                                        &format!("获取视频详细信息失败"),
                                        Some(&format!("视频: {}\nBVID: {}\n错误: {}", video_name, bvid, error_msg)),
                                    )
                                    .await
                                    {
                                        tracing::warn!("发送API错误通知失败: {}", notify_err);
                                    }
                                });
                            }
                        }
                        Ok((tags, mut view_info)) => {
                            let VideoInfo::Detail {
                                pages,
                                staff,
                                ref is_upower_exclusive,
                                ref is_upower_play,
                                ..
                            } = &mut view_info
                            else {
                                unreachable!()
                            };

                            // 革命性充电视频检测：基于API返回的upower字段进行精确判断
                            if let (Some(true), Some(false)) = (is_upower_exclusive, is_upower_play) {
                                info!("「{}」检测到充电专享视频（未充电），将自动删除", &video_model.name);
                                // 创建自动删除任务
                                let delete_task = DeleteVideoTask {
                                    video_id: video_model.id,
                                    task_id: format!("auto_delete_upower_{}", video_model.id),
                                };

                                if let Err(delete_err) =
                                    VIDEO_DELETE_TASK_QUEUE.enqueue_task(delete_task, connection).await
                                {
                                    error!("创建充电视频删除任务失败「{}」: {:#}", &video_model.name, delete_err);
                                } else {
                                    debug!("充电视频删除任务已加入队列「{}」", &video_model.name);
                                }

                                // 跳过后续处理，返回成功完成这个视频的处理
                                return Ok(());
                            }

                            // 日志记录upower字段状态（仅debug级别）
                            if is_upower_exclusive.is_some() || is_upower_play.is_some() {
                                debug!(
                                    "视频「{}」upower状态: exclusive={:?}, play={:?}",
                                    &video_model.name, is_upower_exclusive, is_upower_play
                                );
                            }

                            let pages = std::mem::take(pages);
                            let pages_len = pages.len();

                            // 提取第一个page的cid用于更新video表
                            let first_page_cid = pages.first().map(|p| p.cid);

                            // 调试日志：检查staff信息
                            if let Some(staff_list) = staff {
                                debug!("视频 {} 有staff信息，成员数量: {}", video_model.bvid, staff_list.len());
                                for staff_member in staff_list.iter() {
                                    debug!(
                                        "  - staff: mid={}, title={}, name={}",
                                        staff_member.mid, staff_member.title, staff_member.name
                                    );
                                }
                            } else {
                                debug!("视频 {} 没有staff信息", video_model.bvid);
                            }

                            // 检查是否为合作视频，支持submission和收藏夹来源
                            let mut video_model_mut = video_model.clone();
                            let mut collaboration_video_updated = false;

                            // 使用写事务函数（立即获取写锁，避免 SQLITE_BUSY_SNAPSHOT）
                            let txn = crate::database::begin_write_transaction(connection).await?;

                            if let Some(staff_list) = staff {
                                debug!("视频 {} 有staff信息，成员数量: {}", video_model.bvid, staff_list.len());
                                for staff_member in staff_list.iter() {
                                    debug!(
                                        "  - staff: mid={}, title={}, name={}",
                                        staff_member.mid, staff_member.title, staff_member.name
                                    );
                                }

                                if staff_list.len() > 1 {
                                    debug!(
                                        "发现合作视频：bvid={}, staff_count={}",
                                        video_model.bvid,
                                        staff_list.len()
                                    );

                                    // 查找所有可能的订阅UP主
                                    let mut matched_submission: Option<submission::Model> = None;

                                    // 1. 如果是submission来源，直接使用source_submission_id
                                    if let Some(source_submission_id) = video_model.source_submission_id {
                                        debug!("submission来源视频，source_submission_id: {}", source_submission_id);
                                        if let Ok(Some(submission)) =
                                            submission::Entity::find_by_id(source_submission_id).one(&txn).await
                                        {
                                            debug!(
                                                "找到来源submission: {} ({})",
                                                submission.upper_name, submission.upper_id
                                            );
                                            // 检查这个submission的UP主是否在staff列表中
                                            if staff_list.iter().any(|staff| staff.mid == submission.upper_id) {
                                                debug!(
                                                    "submission UP主 {} ({}) 在staff列表中",
                                                    submission.upper_name, submission.upper_id
                                                );
                                                matched_submission = Some(submission);
                                            } else {
                                                debug!(
                                                    "submission UP主 {} ({}) 不在staff列表中",
                                                    submission.upper_name, submission.upper_id
                                                );
                                            }
                                        } else {
                                            debug!(
                                                "找不到source_submission_id对应的submission记录: {}",
                                                source_submission_id
                                            );
                                        }
                                    } else {
                                        debug!("非submission来源视频，检查staff中是否有已订阅的UP主");
                                        // 2. 如果不是submission来源（如收藏夹），查找所有subscription中匹配的UP主
                                        for staff_member in staff_list.iter() {
                                            debug!(
                                                "检查staff成员 {} ({}) 是否已订阅",
                                                staff_member.name, staff_member.mid
                                            );
                                            if let Ok(Some(submission)) = submission::Entity::find()
                                                .filter(submission::Column::UpperId.eq(staff_member.mid))
                                                .filter(submission::Column::Enabled.eq(true))
                                                .one(&txn)
                                                .await
                                            {
                                                debug!(
                                                    "在staff中找到已订阅的UP主：{} ({})",
                                                    staff_member.name, staff_member.mid
                                                );
                                                matched_submission = Some(submission);
                                                break;
                                            } else {
                                                debug!("staff成员 {} ({}) 未订阅", staff_member.name, staff_member.mid);
                                            }
                                        }
                                    }

                                    // 如果找到了匹配的订阅UP主，进行归类
                                    if let Some(submission) = matched_submission {
                                        // 从staff信息中找到匹配UP主的头像
                                        let matched_staff_face = staff_list
                                            .iter()
                                            .find(|staff| staff.mid == submission.upper_id)
                                            .map(|staff| staff.face.clone())
                                            .unwrap_or_default();

                                        debug!(
                                            "为合作视频匹配UP主头像: {} -> {}",
                                            submission.upper_name, matched_staff_face
                                        );

                                        // 使用submission的信息更新视频，包括正确的头像
                                        video_model_mut.upper_id = submission.upper_id;
                                        video_model_mut.upper_name = submission.upper_name.clone();
                                        video_model_mut.upper_face = matched_staff_face;
                                        collaboration_video_updated = true;
                                        info!(
                                            "合作视频 {} 归类到订阅UP主「{}」(来源：{})",
                                            video_model_mut.bvid,
                                            submission.upper_name,
                                            if video_model.source_submission_id.is_some() {
                                                "投稿订阅"
                                            } else {
                                                "收藏夹"
                                            }
                                        );
                                    } else {
                                        debug!("staff列表中没有找到已订阅的UP主");
                                    }
                                } else {
                                    debug!("staff列表只有{}个成员，不是合作视频", staff_list.len());
                                }
                            } else {
                                debug!("视频 {} 没有staff信息", video_model.bvid);
                            }

                            // 将分页信息写入数据库
                            create_pages(pages, &video_model_mut, &txn).await?;
                            let mut video_active_model = view_info.into_detail_model(video_model_mut.clone());
                            video_source.set_relation_id(&mut video_active_model);
                            video_active_model.single_page = Set(Some(pages_len == 1));
                            video_active_model.tags = Set(Some(serde_json::to_value(tags)?));

                            // 投稿合集/系列兜底：详情接口未返回season_id时，尝试用lists归属回填。
                            // 这样可避免“本应属于合集/系列的视频”落到单独视频目录。
                            let season_id_missing = video_model_mut
                                .season_id
                                .as_ref()
                                .map(|s| s.trim().is_empty())
                                .unwrap_or(true);
                            if season_id_missing {
                                if let Some((fallback_collection_key, fallback_episode_number)) =
                                    submission_collection_membership.get(&video_model_mut.bvid)
                                {
                                    video_active_model.season_id = Set(Some(fallback_collection_key.clone()));
                                    if video_model_mut.episode_number.is_none() {
                                        video_active_model.episode_number = Set(Some(*fallback_episode_number));
                                    }
                                    debug!(
                                        "投稿归属回填: bvid={}, season_id={}, episode_number={}",
                                        video_model_mut.bvid, fallback_collection_key, fallback_episode_number
                                    );
                                }
                            }

                            // 更新video表的cid字段（从第一个page获取）
                            if let Some(cid) = first_page_cid {
                                video_active_model.cid = Set(Some(cid));
                                debug!("更新视频 {} 的cid: {}", video_model_mut.bvid, cid);
                            }

                            // 只有合作视频更新时才覆盖upper信息，保持其他视频的API更新不被影响
                            if collaboration_video_updated {
                                debug!("合作视频检测到更新，覆盖upper信息到数据库");
                                video_active_model.upper_id = Set(video_model_mut.upper_id);
                                video_active_model.upper_name = Set(video_model_mut.upper_name.clone());
                                video_active_model.upper_face = Set(video_model_mut.upper_face.clone());
                            } else {
                                debug!("非合作视频或未发生更新，保持API返回的upper信息");
                            }

                            video_active_model.save(&txn).await?;
                            txn.commit().await?;
                        }
                    };
                    Ok::<_, anyhow::Error>(())
                }
            })
            .collect::<FuturesUnordered<_>>();

        // 并发执行所有任务
        let mut stream = tasks;
        while let Some(res) = stream.next().await {
            if let Err(e) = res {
                // 使用错误分类器进行统一处理
                #[allow(clippy::needless_borrow)]
                let classified_error = crate::error::ErrorClassifier::classify_error(&e);

                if classified_error.error_type == crate::error::ErrorType::UserCancelled {
                    info!("视频详情获取因用户暂停而终止: {}", classified_error.message);
                    return Err(e); // 直接返回暂停错误，不取消其他任务
                }

                let error_msg = e.to_string();

                if e.downcast_ref::<DownloadAbortError>().is_some() || error_msg.contains("Download cancelled") {
                    token.cancel();
                    // drain the rest of the tasks
                    while stream.next().await.is_some() {}
                    return Err(e);
                }
                // for other errors, just log and continue
                error!("获取视频详情时发生错误: {:#}", e);
            }
        }
        info!("完成普通视频详情处理");
    }
    video_source.log_fetch_video_end();
    Ok(())
}

/// 下载所有未处理成功的视频
pub async fn download_unprocessed_videos(
    bili_client: &BiliClient,
    video_source: &VideoSourceEnum,
    connection: &DatabaseConnection,
    downloader: &UnifiedDownloader,
    token: CancellationToken,
) -> Result<()> {
    // Early exit when paused/cancelled
    if crate::task::TASK_CONTROLLER.is_paused() || token.is_cancelled() {
        info!("任务已暂停/取消，跳过下载阶段");
        return Ok(());
    }
    video_source.log_download_video_start();
    let current_config = crate::config::reload_config();
    let semaphore = Semaphore::new(current_config.concurrent_limit.video);
    let unhandled_videos_pages = filter_unhandled_video_pages(video_source.filter_expr(), connection).await?;

    // 只有当有未处理视频时才显示日志
    if !unhandled_videos_pages.is_empty() {
        info!(
            "开始下载阶段：{}「{}」发现 {} 个待处理视频，开始执行下载/修复任务…",
            video_source.source_type_display(),
            video_source.source_name_display(),
            unhandled_videos_pages.len()
        );
    }

    let mut assigned_upper = HashSet::new();
    let mut assigned_bangumi_seasons = HashSet::new();
    let tasks = unhandled_videos_pages
        .into_iter()
        .map(|(video_model, pages_model)| {
            let should_download_upper = if let Some(season_id) = &video_model.season_id {
                // 番剧：基于season_id判断是否需要生成series级别文件
                let season_key = format!("season_{}", season_id);
                let should_download = !assigned_bangumi_seasons.contains(&season_key);
                assigned_bangumi_seasons.insert(season_key);
                debug!(
                    "番剧视频「{}」season_id={}, should_download_upper={}",
                    video_model.name, season_id, should_download
                );
                should_download
            } else {
                // 普通视频：基于upper_id判断
                let should_download = !assigned_upper.contains(&video_model.upper_id);
                assigned_upper.insert(video_model.upper_id);
                should_download
            };
            debug!("下载视频: {}", video_model.name);
            download_video_pages(
                bili_client,
                video_source,
                video_model,
                pages_model,
                connection,
                &semaphore,
                downloader,
                should_download_upper,
                token.clone(),
            )
        })
        .collect::<FuturesUnordered<_>>();
    let mut download_aborted = false;
    let mut succeeded_count = 0usize;
    let mut failed_count = 0usize;
    let mut skipped_count = 0usize;
    let mut stream = tasks;
    // 使用循环和select来处理任务，以便在检测到取消信号时立即停止
    while let Some(res) = stream.next().await {
        match res {
            Ok(model) => {
                if download_aborted {
                    continue;
                }
                // 只有当 video_status 的 completed 标记位为 true 时，才算真正下载完成。
                // 避免“用户暂停/取消时，任务被误判为成功完成”。
                let video_name = match &model.name {
                    sea_orm::ActiveValue::Set(v) | sea_orm::ActiveValue::Unchanged(v) => v.clone(),
                    _ => "未知".to_string(),
                };
                let completed = match &model.download_status {
                    sea_orm::ActiveValue::Set(v) | sea_orm::ActiveValue::Unchanged(v) => {
                        VideoStatus::from(*v).get_completed()
                    }
                    _ => false,
                };
                if completed {
                    succeeded_count += 1;
                } else {
                    skipped_count += 1;
                    info!("下载任务未完成（可能因用户暂停/取消），不计入成功: {}", video_name);
                }
                // 任务成功完成，更新数据库
                if let Err(db_err) = update_videos_model(vec![model.clone()], connection).await {
                    error!("更新数据库失败: {:#}", db_err);

                    // 发送数据库错误通知（异步执行，不阻塞主流程）
                    use sea_orm::ActiveValue;
                    let video_bvid = match &model.bvid {
                        ActiveValue::Set(v) | ActiveValue::Unchanged(v) => v.clone(),
                        _ => "未知".to_string(),
                    };
                    let error_msg = format!("{:#}", db_err);
                    tokio::spawn(async move {
                        use crate::utils::notification::send_error_notification;
                        if let Err(e) = send_error_notification(
                            "数据库错误",
                            &error_msg,
                            Some(&format!("视频: {}\nBVID: {}", video_name, video_bvid)),
                        )
                        .await
                        {
                            tracing::warn!("发送数据库错误通知失败: {}", e);
                        }
                    });
                }
            }
            Err(e) => {
                let error_msg = e.to_string();

                // 调试：输出完整的错误信息
                debug!("检查下载错误消息: '{}'", error_msg);
                debug!("完整错误链: {:#}", e);
                debug!("是否包含'任务已暂停': {}", error_msg.contains("任务已暂停"));
                debug!("是否包含'停止下载': {}", error_msg.contains("停止下载"));
                debug!(
                    "是否包含'Download cancelled': {}",
                    error_msg.contains("Download cancelled")
                );

                // 检查是否是暂停导致的失败，只有在任务暂停时才将 Download cancelled 视为暂停错误
                if error_msg.contains("任务已暂停")
                    || error_msg.contains("停止下载")
                    || error_msg.contains("用户主动暂停任务")
                    || (error_msg.contains("Download cancelled") && crate::task::TASK_CONTROLLER.is_paused())
                {
                    skipped_count += 1;
                    info!("下载任务因用户暂停而终止: {}", error_msg);
                    continue; // 跳过暂停相关的错误，不触发风控
                }

                if e.downcast_ref::<DownloadAbortError>().is_some() || error_msg.contains("Download cancelled") {
                    if !download_aborted {
                        debug!("检测到风控或取消信号，开始中止所有下载任务");
                        token.cancel(); // 立即取消所有其他正在运行的任务
                        download_aborted = true;
                    }
                } else {
                    // 检查是否为暂停相关错误
                    let error_msg = e.to_string();
                    if error_msg.contains("用户主动暂停任务") || error_msg.contains("任务已暂停") {
                        skipped_count += 1;
                        info!("下载任务因用户暂停而终止");
                    } else {
                        failed_count += 1;
                        // 任务返回了非中止的错误
                        error!("下载任务失败: {:#}", e);
                    }
                }
            }
        }
    }

    if download_aborted {
        error!("下载触发风控，已终止所有任务，停止所有后续扫描");

        // 自动重置风控导致的失败任务
        if let Err(reset_err) = auto_reset_risk_control_failures(connection).await {
            error!("自动重置风控失败任务时出错: {:#}", reset_err);
        }

        video_source.log_download_video_end();
        // 风控时返回错误，中断整个扫描循环
        bail!(DownloadAbortError());
    }
    video_source.log_download_video_end();

    if succeeded_count > 0 || failed_count > 0 || skipped_count > 0 {
        info!(
            "下载阶段完成：{}「{}」已处理 {} 个视频（成功 {}，失败 {}，跳过 {}）",
            video_source.source_type_display(),
            video_source.source_name_display(),
            succeeded_count + failed_count + skipped_count,
            succeeded_count,
            failed_count,
            skipped_count
        );
    }
    Ok(())
}

/// 视频源下载完成后批量执行 AI 重命名
///
/// 此函数在所有视频下载完成后调用，避免了单个视频重命名时可能导致的
/// 文件夹路径冲突问题（例如两个视频在同一子文件夹中，第一个重命名后第二个找不到文件）
///
/// 使用批量 API 调用（每次 10 个文件），减少 API 请求次数
pub async fn batch_ai_rename_for_source(video_source: &VideoSourceEnum, connection: &DatabaseConnection) -> Result<()> {
    use crate::utils::ai_rename::{self, AiRenameContext, FileToRename};

    let cfg = crate::config::reload_config();
    let is_bangumi = matches!(video_source, VideoSourceEnum::BangumiSource(_));
    let is_collection = matches!(video_source, VideoSourceEnum::Collection(_));

    // 检查是否需要执行 AI 重命名（全局开关 + 视频源开关）
    if !video_source.ai_rename() || !cfg.ai_rename.enabled {
        return Ok(());
    }

    // 检查番剧/合集的独立开关（使用视频源自己的配置）
    if is_bangumi && !video_source.ai_rename_enable_bangumi() {
        debug!("[{}] 番剧AI重命名已禁用，跳过", video_source.source_key());
        return Ok(());
    }
    if is_collection && !video_source.ai_rename_enable_collection() {
        debug!("[{}] 合集AI重命名已禁用，跳过", video_source.source_key());
        return Ok(());
    }

    let source_key = video_source.source_key();
    info!("[{}] 开始批量 AI 重命名", source_key);

    // 获取该视频源下所有已完成下载的视频和分页
    let videos_with_pages: Vec<(video::Model, Vec<page::Model>)> = video::Entity::find()
        .filter(video_source.filter_expr())
        .filter(video::Column::Valid.eq(true))
        .filter(video::Column::Deleted.eq(0))
        .find_with_related(page::Entity)
        .all(connection)
        .await?;

    if videos_with_pages.is_empty() {
        debug!("[{}] 没有需要重命名的视频", source_key);
        return Ok(());
    }

    // 获取视频源自定义提示词
    let video_prompt_override = video_source.ai_rename_video_prompt();
    let audio_prompt_override = video_source.ai_rename_audio_prompt();

    let source_type = match video_source {
        VideoSourceEnum::Favorite(_) => "收藏夹",
        VideoSourceEnum::Collection(_) => "合集",
        VideoSourceEnum::Submission(_) => "投稿",
        VideoSourceEnum::WatchLater(_) => "稍后再看",
        VideoSourceEnum::BangumiSource(_) => "番剧",
    };

    let mut renamed_count = 0;
    let mut skipped_count = 0;
    let mut failed_count = 0;

    // 第一阶段：收集所有需要重命名的文件
    let mut video_files: Vec<FileToRename> = Vec::new();
    let mut audio_files: Vec<FileToRename> = Vec::new();

    for (video_model, pages) in &videos_with_pages {
        // 检查多P视频开关（使用视频源自己的配置）
        let is_multi_page = video_model.single_page.unwrap_or(true) == false;
        if is_multi_page && !video_source.ai_rename_enable_multi_page() {
            debug!(
                "[{}] 跳过多P视频: {} (多P视频AI重命名已禁用)",
                source_key, video_model.name
            );
            skipped_count += pages.len();
            continue;
        }

        for page_model in pages {
            // 重新从数据库查询最新的 page 信息（可能被前面的迭代更新了路径）
            let latest_page = match page::Entity::find_by_id(page_model.id).one(connection).await {
                Ok(Some(p)) => p,
                Ok(None) => {
                    debug!("[{}] 跳过: page_id={} 在数据库中不存在", source_key, page_model.id);
                    skipped_count += 1;
                    continue;
                }
                Err(e) => {
                    warn!("[{}] 查询 page_id={} 失败: {}", source_key, page_model.id, e);
                    skipped_count += 1;
                    continue;
                }
            };

            // 只处理有路径的分页（已下载）
            let page_path_str = match &latest_page.path {
                Some(p) if !p.is_empty() => p.clone(),
                _ => {
                    skipped_count += 1;
                    continue;
                }
            };

            let page_path = std::path::Path::new(&page_path_str);

            // 检查文件是否存在
            if !page_path.exists() {
                debug!("[{}] 跳过不存在的文件: {}", source_key, page_path_str);
                skipped_count += 1;
                continue;
            }

            // 获取文件名和扩展名
            let current_stem = match page_path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => {
                    skipped_count += 1;
                    continue;
                }
            };

            let file_ext = page_path
                .extension()
                .and_then(|s| s.to_str())
                .unwrap_or("mp4")
                .to_string();

            // 检查是否已经被 AI 重命名过（通过数据库字段判断）
            if latest_page.ai_renamed.unwrap_or(0) == 1 {
                debug!("[{}] 跳过已重命名的文件: {}", source_key, current_stem);
                skipped_count += 1;
                continue;
            }

            let is_audio = matches!(file_ext.as_str(), "m4a" | "mp3" | "flac" | "aac" | "ogg");

            // 构建 AI 重命名上下文
            let ctx = AiRenameContext {
                title: video_model.name.clone(),
                desc: video_model.intro.clone(),
                owner: video_model.upper_name.clone(),
                tname: String::new(),
                duration: latest_page.duration as u32,
                pubdate: video_model.pubtime.format("%Y-%m-%d").to_string(),
                dimension: match (latest_page.width, latest_page.height) {
                    (Some(w), Some(h)) => format!("{}x{}", w, h),
                    _ => String::new(),
                },
                part_name: latest_page.name.clone(),
                ugc_season: None,
                copyright: String::new(),
                view: 0,
                pid: latest_page.pid,
                episode_number: video_model.episode_number,
                source_type: source_type.to_string(),
                is_audio,
                sort_index: None,
                bvid: video_model.bvid.clone(),
            };

            let file_info = FileToRename {
                path: page_path.to_path_buf(),
                current_stem,
                ext: file_ext,
                ctx,
                page_id: latest_page.id,
                video_id: video_model.id,
                bvid: video_model.bvid.clone(),
                single_page: video_model.single_page.unwrap_or(false),
                flat_folder: video_source.flat_folder(),
            };

            if is_audio {
                audio_files.push(file_info);
            } else {
                video_files.push(file_info);
            }
        }
    }

    info!(
        "[{}] 收集完成: {} 个视频文件, {} 个音频文件待重命名",
        source_key,
        video_files.len(),
        audio_files.len()
    );

    // 第二阶段：按批次处理视频文件（每批 10 个）
    let batch_size = 10;
    let video_prompt_hint = if !video_prompt_override.is_empty() {
        video_prompt_override
    } else {
        &cfg.ai_rename.video_prompt_hint
    };
    let audio_prompt_hint = if !audio_prompt_override.is_empty() {
        audio_prompt_override
    } else {
        &cfg.ai_rename.audio_prompt_hint
    };

    // 处理视频文件
    for (batch_idx, batch) in video_files.chunks(batch_size).enumerate() {
        info!(
            "[{}] 处理视频批次 {}/{}: {} 个文件",
            source_key,
            batch_idx + 1,
            (video_files.len() + batch_size - 1) / batch_size,
            batch.len()
        );

        match ai_rename::ai_generate_filenames_batch(&cfg.ai_rename, &source_key, batch, video_prompt_hint).await {
            Ok(new_names) => {
                for (file, new_stem) in batch.iter().zip(new_names.iter()) {
                    match apply_ai_rename(video_source, connection, &source_key, file, new_stem, &cfg).await {
                        Ok(true) => renamed_count += 1,
                        Ok(false) => skipped_count += 1,
                        Err(e) => {
                            warn!("[{}] 重命名失败 {}: {}", source_key, file.current_stem, e);
                            failed_count += 1;
                        }
                    }
                }
            }
            Err(e) => {
                warn!("[{}] 视频批次 {} API 调用失败: {}", source_key, batch_idx + 1, e);
                failed_count += batch.len();
            }
        }
    }

    // 处理音频文件
    for (batch_idx, batch) in audio_files.chunks(batch_size).enumerate() {
        info!(
            "[{}] 处理音频批次 {}/{}: {} 个文件",
            source_key,
            batch_idx + 1,
            (audio_files.len() + batch_size - 1) / batch_size,
            batch.len()
        );

        match ai_rename::ai_generate_filenames_batch(&cfg.ai_rename, &source_key, batch, audio_prompt_hint).await {
            Ok(new_names) => {
                for (file, new_stem) in batch.iter().zip(new_names.iter()) {
                    match apply_ai_rename(video_source, connection, &source_key, file, new_stem, &cfg).await {
                        Ok(true) => renamed_count += 1,
                        Ok(false) => skipped_count += 1,
                        Err(e) => {
                            warn!("[{}] 重命名失败 {}: {}", source_key, file.current_stem, e);
                            failed_count += 1;
                        }
                    }
                }
            }
            Err(e) => {
                warn!("[{}] 音频批次 {} API 调用失败: {}", source_key, batch_idx + 1, e);
                failed_count += batch.len();
            }
        }
    }

    info!(
        "[{}] 批量 AI 重命名完成: 重命名 {} 个, 跳过 {} 个, 失败 {} 个",
        source_key, renamed_count, skipped_count, failed_count
    );

    Ok(())
}

/// 应用单个文件的 AI 重命名
///
/// 返回 Ok(true) 表示成功重命名，Ok(false) 表示跳过
async fn apply_ai_rename(
    video_source: &VideoSourceEnum,
    connection: &DatabaseConnection,
    source_key: &str,
    file: &crate::utils::ai_rename::FileToRename,
    new_stem: &str,
    cfg: &crate::config::Config,
) -> Result<bool> {
    use crate::utils::ai_rename;
    use bili_sync_entity::page;
    use sea_orm::EntityTrait;

    // 新文件名为空或相同则跳过
    if new_stem.is_empty() || new_stem == file.current_stem {
        debug!("[{}] 跳过(文件名相同或为空): {}", source_key, file.current_stem);
        return Ok(false);
    }

    // 重新从数据库查询最新的 page.path（可能在之前批次中被更新）
    let latest_page = page::Entity::find_by_id(file.page_id)
        .one(connection)
        .await?
        .ok_or_else(|| anyhow::anyhow!("页面记录不存在: {}", file.page_id))?;

    let page_path = match &latest_page.path {
        Some(p) if !p.is_empty() => std::path::PathBuf::from(p),
        _ => {
            debug!("[{}] 跳过(数据库路径为空): {}", source_key, file.current_stem);
            return Ok(false);
        }
    };

    // 重新检查文件是否存在（可能在批量处理期间被移动）
    if !page_path.exists() {
        debug!(
            "[{}] 跳过(文件已不存在): {} -> {:?}",
            source_key, file.current_stem, page_path
        );
        return Ok(false);
    }

    // 重新获取当前文件名（路径可能已更新）
    let current_stem = page_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(&file.current_stem);

    // 如果新文件名与当前文件名相同则跳过
    if new_stem == current_stem {
        debug!("[{}] 跳过(文件名已更新为相同): {}", source_key, new_stem);
        return Ok(false);
    }

    // 检查目标文件是否已存在，若冲突则追加 bvid
    let mut final_stem = new_stem.to_string();
    let mut new_path = page_path.with_file_name(format!("{}.{}", final_stem, file.ext));
    if new_path.exists() && new_path != page_path {
        final_stem = format!("{}-{}", new_stem, file.bvid);
        new_path = page_path.with_file_name(format!("{}.{}", final_stem, file.ext));
        info!(
            "[{}] AI 重命名检测到文件冲突，追加BV号: {} -> {}",
            source_key, new_stem, final_stem
        );
    }

    // 执行文件重命名
    if let Err(e) = std::fs::rename(&page_path, &new_path) {
        return Err(anyhow::anyhow!("重命名文件失败: {}", e));
    }

    // 重命名侧车文件
    if let Err(e) = ai_rename::rename_sidecars(&page_path, &final_stem, &file.ext) {
        warn!("[{}] AI 重命名侧车文件失败: {}", source_key, e);
    }

    // 更新 NFO 文件内容
    let new_nfo_path = new_path.with_extension("nfo");
    if let Err(e) = ai_rename::update_nfo_content(&new_nfo_path, &final_stem) {
        warn!("[{}] AI 更新NFO内容失败: {}", source_key, e);
    }

    // 重命名子文件夹（单P视频）
    let should_rename_folder = cfg.ai_rename.rename_parent_dir
        && video_source.ai_rename_rename_parent_dir()
        && !video_source.flat_folder()
        && file.single_page
        && !matches!(video_source, VideoSourceEnum::Collection(_));

    let final_path = if should_rename_folder {
        if let Some(old_dir) = new_path.parent() {
            if let Some(parent_dir) = old_dir.parent() {
                let mut target_dir = parent_dir.join(&final_stem);
                if target_dir.exists() && target_dir != old_dir {
                    target_dir = parent_dir.join(format!("{}-{}", &final_stem, file.bvid));
                }

                if target_dir != old_dir {
                    match std::fs::rename(old_dir, &target_dir) {
                        Ok(_) => {
                            let moved_path =
                                target_dir.join(new_path.file_name().expect("new_path should have file name"));
                            info!(
                                "[{}] AI 重命名子文件夹成功: {} -> {}",
                                source_key,
                                old_dir.display(),
                                target_dir.display()
                            );

                            // 更新当前 video.path
                            let new_video_path = target_dir.to_string_lossy().to_string();
                            if let Ok(Some(current_video)) =
                                video::Entity::find_by_id(file.video_id).one(connection).await
                            {
                                let mut active_video: video::ActiveModel = current_video.into();
                                active_video.path = Set(new_video_path.clone());
                                if let Err(e) = active_video.update(connection).await {
                                    warn!("[{}] 更新 video.path 失败: {}", source_key, e);
                                }
                            }

                            // 更新同一文件夹中其他视频的路径
                            let old_dir_str = old_dir.to_string_lossy().to_string();
                            let old_dir_str_alt = old_dir_str.replace('/', "\\");

                            if let Ok(other_videos) = video::Entity::find()
                                .filter(video_source.filter_expr())
                                .filter(video::Column::Id.ne(file.video_id))
                                .filter(
                                    video::Column::Path
                                        .eq(&old_dir_str)
                                        .or(video::Column::Path.eq(&old_dir_str_alt)),
                                )
                                .all(connection)
                                .await
                            {
                                for other_video in other_videos {
                                    let mut active_other: video::ActiveModel = other_video.clone().into();
                                    active_other.path = Set(new_video_path.clone());
                                    if let Err(e) = active_other.update(connection).await {
                                        warn!("[{}] 更新同文件夹其他视频 video.path 失败: {}", source_key, e);
                                    }

                                    if let Ok(other_pages) = page::Entity::find()
                                        .filter(page::Column::VideoId.eq(other_video.id))
                                        .all(connection)
                                        .await
                                    {
                                        for other_page in other_pages {
                                            if let Some(page_path_str) = other_page.path.clone() {
                                                if page_path_str.starts_with(&old_dir_str)
                                                    || page_path_str.starts_with(&old_dir_str_alt)
                                                {
                                                    let new_page_path = if page_path_str.starts_with(&old_dir_str) {
                                                        page_path_str.replacen(&old_dir_str, &new_video_path, 1)
                                                    } else {
                                                        page_path_str.replacen(&old_dir_str_alt, &new_video_path, 1)
                                                    };
                                                    let mut active_page: page::ActiveModel = other_page.into();
                                                    active_page.path = Set(Some(new_page_path.clone()));
                                                    if let Err(e) = active_page.update(connection).await {
                                                        warn!(
                                                            "[{}] 更新同文件夹其他视频 page.path 失败: {}",
                                                            source_key, e
                                                        );
                                                    } else {
                                                        info!(
                                                            "[{}] 同步更新同文件夹页面路径: {} -> {}",
                                                            source_key, page_path_str, new_page_path
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            moved_path
                        }
                        Err(e) => {
                            warn!("[{}] AI 重命名子文件夹失败: {}", source_key, e);
                            new_path.clone()
                        }
                    }
                } else {
                    new_path.clone()
                }
            } else {
                new_path.clone()
            }
        } else {
            new_path.clone()
        }
    } else {
        new_path.clone()
    };

    // 更新 page.path 和 ai_renamed 标记
    let new_path_str = final_path.to_string_lossy().to_string();
    if let Ok(Some(current_page)) = page::Entity::find_by_id(file.page_id).one(connection).await {
        let mut active_page: page::ActiveModel = current_page.into();
        active_page.path = Set(Some(new_path_str.clone()));
        active_page.ai_renamed = Set(Some(1)); // 标记为已 AI 重命名
        if let Err(e) = active_page.update(connection).await {
            warn!("[{}] 更新 page.path 失败: {}", source_key, e);
        }
    }

    info!(
        "[{}] AI 重命名成功: {} -> {}",
        source_key,
        file.current_stem,
        final_path.display()
    );

    Ok(true)
}

/// 对当前循环中失败的视频进行一次重试
pub async fn retry_failed_videos_once(
    bili_client: &BiliClient,
    video_source: &VideoSourceEnum,
    connection: &DatabaseConnection,
    downloader: &UnifiedDownloader,
    token: CancellationToken,
) -> Result<()> {
    // Early exit when paused/cancelled
    if crate::task::TASK_CONTROLLER.is_paused() || token.is_cancelled() {
        info!("任务已暂停/取消，跳过失败视频重试阶段");
        return Ok(());
    }
    let failed_videos_pages = get_failed_videos_in_current_cycle(video_source.filter_expr(), connection).await?;

    if failed_videos_pages.is_empty() {
        debug!("当前循环中没有失败的视频需要重试");
        return Ok(());
    }

    info!("开始重试当前循环中的 {} 个失败视频", failed_videos_pages.len());

    let current_config = crate::config::reload_config();
    let semaphore = Semaphore::new(current_config.concurrent_limit.video);
    let mut assigned_upper = HashSet::new();
    let mut assigned_bangumi_seasons = HashSet::new();

    let tasks = failed_videos_pages
        .into_iter()
        .map(|(video_model, pages_model)| {
            let should_download_upper = if let Some(season_id) = &video_model.season_id {
                // 番剧：基于season_id判断是否需要生成series级别文件
                let season_key = format!("season_{}", season_id);
                let should_download = !assigned_bangumi_seasons.contains(&season_key);
                assigned_bangumi_seasons.insert(season_key);
                debug!(
                    "重试番剧视频「{}」season_id={}, should_download_upper={}",
                    video_model.name, season_id, should_download
                );
                should_download
            } else {
                // 普通视频：基于upper_id判断
                let should_download = !assigned_upper.contains(&video_model.upper_id);
                assigned_upper.insert(video_model.upper_id);
                should_download
            };
            debug!("重试视频: {}", video_model.name);
            download_video_pages(
                bili_client,
                video_source,
                video_model,
                pages_model,
                connection,
                &semaphore,
                downloader,
                should_download_upper,
                token.clone(),
            )
        })
        .collect::<FuturesUnordered<_>>();

    let mut download_aborted = false;
    let mut stream = tasks;
    let mut retry_success_count = 0;

    while let Some(res) = stream.next().await {
        match res {
            Ok(model) => {
                if download_aborted {
                    continue;
                }
                retry_success_count += 1;
                if let Err(db_err) = update_videos_model(vec![model.clone()], connection).await {
                    error!("重试后更新数据库失败: {:#}", db_err);

                    // 发送数据库错误通知（异步执行，不阻塞主流程）
                    use sea_orm::ActiveValue;
                    let video_name = match &model.name {
                        ActiveValue::Set(v) | ActiveValue::Unchanged(v) => v.clone(),
                        _ => "未知".to_string(),
                    };
                    let video_bvid = match &model.bvid {
                        ActiveValue::Set(v) | ActiveValue::Unchanged(v) => v.clone(),
                        _ => "未知".to_string(),
                    };
                    let error_msg = format!("{:#}", db_err);
                    tokio::spawn(async move {
                        use crate::utils::notification::send_error_notification;
                        if let Err(e) = send_error_notification(
                            "数据库错误",
                            &error_msg,
                            Some(&format!(
                                "视频: {}\nBVID: {}\n（重试后更新失败）",
                                video_name, video_bvid
                            )),
                        )
                        .await
                        {
                            tracing::warn!("发送数据库错误通知失败: {}", e);
                        }
                    });
                }
            }
            Err(e) => {
                let error_msg = e.to_string();

                // 检查是否是暂停导致的失败，只有在任务暂停时才将 Download cancelled 视为暂停错误
                if error_msg.contains("任务已暂停")
                    || error_msg.contains("停止下载")
                    || error_msg.contains("用户主动暂停任务")
                    || (error_msg.contains("Download cancelled") && crate::task::TASK_CONTROLLER.is_paused())
                {
                    info!("重试任务因用户暂停而终止: {}", error_msg);
                    continue; // 跳过暂停相关的错误，不触发风控
                }

                if e.downcast_ref::<DownloadAbortError>().is_some() || error_msg.contains("Download cancelled") {
                    if !download_aborted {
                        debug!("重试过程中检测到风控或取消信号，停止重试");
                        token.cancel();
                        download_aborted = true;
                    }
                } else {
                    // 重试失败，但不中断其他重试任务
                    debug!("视频重试失败: {:#}", e);
                }
            }
        }
    }

    if download_aborted {
        warn!("重试过程中触发风控，已停止重试");
        // 不返回错误，避免影响主流程
    } else if retry_success_count > 0 {
        info!("循环内重试完成，成功重试 {} 个视频", retry_success_count);
    } else {
        debug!("循环内重试完成，但没有视频重试成功");
    }

    Ok(())
}

/// 分页下载任务的参数结构体
pub struct DownloadPageArgs<'a> {
    pub should_run: bool,
    pub bili_client: &'a BiliClient,
    pub video_source: &'a VideoSourceEnum,
    pub video_model: &'a video::Model,
    pub pages: Vec<page::Model>,
    pub connection: &'a DatabaseConnection,
    pub downloader: &'a UnifiedDownloader,
    pub base_path: &'a Path,
}

#[allow(clippy::too_many_arguments)]
pub async fn download_video_pages(
    bili_client: &BiliClient,
    video_source: &VideoSourceEnum,
    video_model: video::Model,
    pages: Vec<page::Model>,
    connection: &DatabaseConnection,
    semaphore: &Semaphore,
    downloader: &UnifiedDownloader,
    should_download_upper: bool,
    token: CancellationToken,
) -> Result<video::ActiveModel> {
    let _permit = tokio::select! {
        biased;
        _ = token.cancelled() => return Err(anyhow!("Download cancelled")),
        permit = semaphore.acquire() => permit.context("acquire semaphore failed")?,
    };
    let mut status = VideoStatus::from(video_model.download_status);
    let separate_status = status.should_run();
    // “重置后恢复”判断：如果数据库里已存在 page.path，通常代表曾经下载过；此时遇到 B站-404 应恢复为未重置
    let has_existing_page_paths = pages.iter().any(|p| p.path.as_ref().is_some());

    // 检查是否为番剧
    let is_bangumi = matches!(video_source, VideoSourceEnum::BangumiSource(_));

    // 检查是否为合集源
    let is_collection_source = matches!(video_source, VideoSourceEnum::Collection(_));

    // 定义最终使用的视频模型
    let final_video_model = if is_bangumi {
        video_model.clone()
    } else {
        // 对于非番剧，重新从数据库加载视频信息，以获取可能在fetch_video_details中更新的upper信息
        if let Ok(Some(updated)) = video::Entity::find_by_id(video_model.id).one(connection).await {
            debug!(
                "重新加载视频信息: upper_name={}, upper_id={}",
                updated.upper_name, updated.upper_id
            );
            updated
        } else {
            debug!("无法重新加载视频信息，使用原始模型");
            video_model.clone()
        }
    };

    // 对于已经获取过详情但可能需要合作视频重新归类的普通视频，进行检测
    let final_video_model = if !is_bangumi {
        // 检查是否需要进行合作视频检测（只对有staff信息的视频）
        if let Some(staff_info) = &final_video_model.staff_info {
            if let Ok(staff_list) = serde_json::from_value::<Vec<crate::bilibili::StaffInfo>>(staff_info.clone()) {
                debug!(
                    "视频 {} 有staff信息，成员数量: {} (下载阶段检测)",
                    final_video_model.bvid,
                    staff_list.len()
                );

                if staff_list.len() > 1 {
                    // 获取所有启用的订阅
                    let submissions = submission::Entity::find()
                        .filter(submission::Column::Enabled.eq(true))
                        .all(connection)
                        .await
                        .context("get submissions failed")?;

                    let mut matched_submission = None;
                    // 检查staff中是否有已订阅的UP主
                    for submission in &submissions {
                        for staff_member in &staff_list {
                            if staff_member.mid == submission.upper_id {
                                debug!(
                                    "在staff中找到已订阅的UP主：{} ({})",
                                    staff_member.name, staff_member.mid
                                );
                                matched_submission = Some(submission);
                                break;
                            }
                        }
                    }

                    // 如果找到了匹配的订阅UP主，进行归类
                    if let Some(submission) = matched_submission {
                        // 从staff信息中找到匹配UP主的头像
                        let matched_staff_face = staff_list
                            .iter()
                            .find(|staff| staff.mid == submission.upper_id)
                            .map(|staff| staff.face.clone())
                            .unwrap_or_default();

                        debug!(
                            "为合作视频匹配UP主头像 (下载阶段): {} -> {}",
                            submission.upper_name, matched_staff_face
                        );

                        // 创建更新后的视频模型
                        let mut updated_model = final_video_model.clone();
                        updated_model.upper_id = submission.upper_id;
                        updated_model.upper_name = submission.upper_name.clone();
                        updated_model.upper_face = matched_staff_face.clone();

                        // 立即保存到数据库
                        let mut active_model: video::ActiveModel = updated_model.clone().into();
                        active_model.upper_id = Set(submission.upper_id);
                        active_model.upper_name = Set(submission.upper_name.clone());
                        active_model.upper_face = Set(matched_staff_face);

                        if let Err(e) = active_model.update(connection).await {
                            warn!("更新合作视频信息失败: {}", e);
                        } else {
                            // 触发异步同步到内存DB
                            info!(
                                "合作视频 {} 归类到订阅UP主「{}」(下载阶段处理)",
                                updated_model.bvid, submission.upper_name
                            );
                        }

                        updated_model
                    } else {
                        debug!("staff列表中没有找到已订阅的UP主 (下载阶段)");
                        final_video_model
                    }
                } else {
                    debug!("staff列表只有{}个成员，不是合作视频 (下载阶段)", staff_list.len());
                    final_video_model
                }
            } else {
                debug!("解析staff信息失败 (下载阶段)");
                final_video_model
            }
        } else {
            debug!("视频 {} 没有staff信息 (下载阶段)", final_video_model.bvid);
            final_video_model
        }
    } else {
        final_video_model
    };

    let mut final_video_model = final_video_model;

    // 投稿源中的UGC合集视频如果缺少episode_number，按同合集发布时间顺序兜底计算。
    if is_submission_ugc_collection_video(video_source, &final_video_model)
        && final_video_model.episode_number.is_none()
    {
        match get_submission_collection_video_episode_number(connection, &final_video_model).await {
            Ok(episode_number) => {
                final_video_model.episode_number = Some(episode_number);
                if let Err(e) = video::Entity::update(video::ActiveModel {
                    id: Set(final_video_model.id),
                    episode_number: Set(Some(episode_number)),
                    ..Default::default()
                })
                .exec(connection)
                .await
                {
                    warn!(
                        "回填投稿UGC合集集序失败: video_id={}, bvid={}, err={}",
                        final_video_model.id, final_video_model.bvid, e
                    );
                } else {
                    debug!(
                        "回填投稿UGC合集集序成功: video_id={}, bvid={}, episode_number={}",
                        final_video_model.id, final_video_model.bvid, episode_number
                    );
                }
            }
            Err(e) => {
                debug!(
                    "计算投稿UGC合集集序失败，将继续使用默认值: video_id={}, bvid={}, err={}",
                    final_video_model.id, final_video_model.bvid, e
                );
            }
        }
    }

    // 投稿源中的UGC合集视频（有season_id）也按合集逻辑处理（仅路径/命名相关）
    let is_submission_collection_video = is_submission_ugc_collection_video(video_source, &final_video_model);

    // 统一“合集类视频”判定：真实合集源 + 投稿源中的UGC合集视频
    let is_collection = is_collection_source || is_submission_collection_video;

    // 为番剧获取API数据用于NFO生成
    let season_info = if is_bangumi && video_model.season_id.is_some() {
        let season_id = video_model.season_id.as_ref().unwrap();
        match get_season_info_from_api(bili_client, season_id, token.clone()).await {
            Ok(info) => {
                debug!("成功获取番剧 {} 的API信息用于NFO生成", info.title);
                Some(info)
            }
            Err(e) => {
                warn!(
                    "获取番剧 {} (season_id: {}) 的API信息失败: {}",
                    video_model.name, season_id, e
                );
                None
            }
        }
    } else {
        None
    };

    // 平铺目录模式（不为单个视频/季度创建子文件夹）
    let flat_folder = video_source.flat_folder();

    // 获取番剧源和季度信息
    let (base_path, season_folder, bangumi_folder_path) = if is_bangumi {
        let bangumi_source = match video_source {
            VideoSourceEnum::BangumiSource(source) => source,
            _ => unreachable!(),
        };

        // 为番剧创建独立的文件夹：配置路径 -> 番剧文件夹 -> Season文件夹
        let bangumi_root_path = bangumi_source.path();

        // 平铺目录模式：直接使用视频源根目录，不创建番剧文件夹/Season结构
        if flat_folder {
            debug!("平铺目录模式：番剧所有文件直接放在视频源根目录，跳过Season目录结构");
            (
                bangumi_root_path.to_path_buf(),
                None,
                Some(bangumi_root_path.to_path_buf()),
            )
        } else {
            // 创建临时的page模型来获取格式化参数（只创建一次，避免重复）
            let temp_page = bili_sync_entity::page::Model {
                id: 0,
                video_id: video_model.id,
                cid: 0,
                pid: 1,
                name: "temp".to_string(),
                width: None,
                height: None,
                duration: 0,
                path: None,
                image: None,
                download_status: 0,
                created_at: now_standard_string(),
                ai_renamed: None,
            };

            // 获取真实的番剧标题（从缓存或API）
            let api_title = if let Some(ref season_id) = video_model.season_id {
                get_cached_season_title(bili_client, season_id, token.clone()).await
            } else {
                None
            };

            // 使用番剧格式化参数，优先使用API提供的真实标题
            let format_args =
                crate::utils::format_arg::bangumi_page_format_args(&video_model, &temp_page, api_title.as_deref());

            // 检查是否有有效的series_title，如果没有则跳过番剧处理
            let series_title = format_args["series_title"].as_str().unwrap_or("");
            if series_title.is_empty() {
                return Err(anyhow::anyhow!(
                    "番剧 {} (BVID: {}) 缺少API标题数据，无法创建番剧文件夹",
                    video_model.name,
                    video_model.bvid
                ));
            }

            // 生成番剧文件夹名称
            let bangumi_folder_name =
                crate::config::with_config(|bundle| bundle.render_bangumi_folder_template(&format_args))
                    .map_err(|e| anyhow::anyhow!("渲染番剧文件夹模板失败: {}", e))?;

            // 番剧文件夹路径
            let bangumi_folder_path = bangumi_root_path.join(&bangumi_folder_name);

            // 延迟创建番剧文件夹，只在实际需要时创建

            // 检查是否启用番剧Season结构
            let use_bangumi_season_structure =
                crate::config::with_config(|bundle| bundle.config.bangumi_use_season_structure);

            if use_bangumi_season_structure {
                // 启用番剧Season结构：创建统一的系列根目录，在其下创建Season子目录

                // 提取基础系列名称和季度信息
                let series_title = api_title.as_deref().unwrap_or(&video_model.name);
                let season_title = format_args.get("season_title").and_then(|v| v.as_str());

                let (base_series_name_raw, season_number) =
                    crate::utils::bangumi_name_extractor::BangumiNameExtractor::extract_series_name_and_season(
                        series_title,
                        season_title,
                    );

                // 系列根目录路径，直接使用提取后的基础系列名称（不应用标准化）
                // 这样确保同一系列的不同季度使用相同的根目录
                let series_root_path = bangumi_root_path.join(&base_series_name_raw);

                // 生成标准的Season文件夹名称，根据实际季度编号生成
                let season_folder_name =
                    crate::utils::bangumi_name_extractor::BangumiNameExtractor::generate_season_folder_name(
                        season_number,
                    );
                let season_path = series_root_path.join(&season_folder_name);

                (season_path, Some(season_folder_name), Some(series_root_path))
            } else {
                // 原有逻辑：根据配置决定是否创建季度子目录
                let should_create_season_folder = bangumi_source.download_all_seasons
                    || (bangumi_source
                        .selected_seasons
                        .as_ref()
                        .map(|s| !s.is_empty())
                        .unwrap_or(false))
                    || video_model.season_id.is_some(); // 单季度番剧：如果有season_id就创建目录

                if should_create_season_folder && video_model.season_id.is_some() {
                    // 使用配置的folder_structure模板生成季度文件夹名称（复用已有的format_args）
                    let season_folder_name =
                        crate::config::with_config(|bundle| bundle.render_folder_structure_template(&format_args))
                            .map_err(|e| anyhow::anyhow!("渲染季度文件夹模板失败: {}", e))?;

                    (
                        bangumi_folder_path.join(&season_folder_name),
                        Some(season_folder_name),
                        Some(bangumi_folder_path),
                    )
                } else {
                    // 不启用下载所有季度且没有选中特定季度时，直接使用番剧文件夹路径
                    (bangumi_folder_path.clone(), None, Some(bangumi_folder_path))
                }
            }
        }
    } else {
        // 非番剧使用原来的逻辑，但对合集进行特殊处理
        // 【重要】：始终从视频源的原始路径开始计算，避免使用已保存的视频路径
        let video_source_base_path = video_source.path();

        debug!("=== 路径计算开始 ===");
        debug!("视频源基础路径: {:?}", video_source_base_path);
        debug!("视频BVID: {}", final_video_model.bvid);
        debug!(
            "视频UP主: {} ({})",
            final_video_model.upper_name, final_video_model.upper_id
        );
        debug!("数据库中保存的路径: {:?}", final_video_model.path);
        debug!("注意：将忽略数据库中的路径，从视频源基础路径重新计算");
        if matches!(video_source, VideoSourceEnum::Submission(_)) {
            debug!(
                "投稿合集判定: is_submission_collection_video={}, source_submission_id={:?}, season_id={:?}",
                is_submission_collection_video, final_video_model.source_submission_id, final_video_model.season_id
            );
        }

        if flat_folder {
            debug!("平铺目录模式：所有文件直接放在视频源根目录");
        }

        let path = if flat_folder {
            // 平铺目录模式：直接使用视频源根目录，不创建子文件夹
            video_source_base_path.to_path_buf()
        } else if let VideoSourceEnum::Collection(collection_source) = video_source {
            // 合集的特殊处理
            let config = crate::config::reload_config();
            match config.collection_folder_mode.as_ref() {
                "unified" => {
                    // 统一模式：所有视频放在以合集名称命名的同一个文件夹下
                    let safe_collection_name = crate::utils::filenamify::filenamify(&collection_source.name);
                    debug!(
                        "合集统一模式 - 原名称: '{}', 安全化后: '{}'",
                        collection_source.name, safe_collection_name
                    );
                    video_source_base_path.join(&safe_collection_name)
                }
                "up_seasonal" => {
                    // 同UP合集分季模式：同一个UP主的合集/列表归并到“源路径/UP主名/UP主名合集”根目录
                    let safe_upper_name = crate::utils::filenamify::filenamify(final_video_model.upper_name.trim());
                    let up_dir_name = if safe_upper_name.is_empty() {
                        format!("UP_{}", collection_source.m_id)
                    } else {
                        safe_upper_name.clone()
                    };
                    let up_collection_root_name = if safe_upper_name.is_empty() {
                        format!("UP_{}合集", collection_source.m_id)
                    } else if safe_upper_name.ends_with("合集") {
                        safe_upper_name
                    } else {
                        format!("{}合集", safe_upper_name)
                    };
                    debug!(
                        "合集同UP分季模式 - UP主: '{}' ({}), 路径: '{}/{}/{}'",
                        final_video_model.upper_name,
                        collection_source.m_id,
                        video_source_base_path.display(),
                        up_dir_name,
                        up_collection_root_name
                    );
                    video_source_base_path.join(&up_dir_name).join(&up_collection_root_name)
                }
                _ => {
                    // 分离模式（默认）：每个视频有自己的文件夹
                    let base_folder_name = crate::config::with_config(|bundle| {
                        bundle.render_video_template(&video_format_args(&final_video_model))
                    })
                    .map_err(|e| anyhow::anyhow!("模板渲染失败: {}", e))?;

                    debug!("合集分离模式 - 渲染的文件夹名: '{}'", base_folder_name);
                    debug!("合集分离模式 - 基础路径: {:?}", video_source_base_path);

                    // **智能判断：根据模板内容决定是否需要去重**
                    let video_template =
                        crate::config::with_config(|bundle| bundle.config.video_name.as_ref().to_string());
                    let needs_deduplication = video_template.contains("title")
                        || (video_template.contains("name") && !video_template.contains("upper_name"));

                    if needs_deduplication {
                        // 智能去重：检查文件夹名是否已存在，如果存在则追加唯一标识符
                        let unique_folder_name = generate_unique_folder_name(
                            video_source_base_path,
                            &base_folder_name,
                            &video_model,
                            &video_model.pubtime.format("%Y-%m-%d").to_string(),
                        );
                        video_source_base_path.join(&unique_folder_name)
                    } else {
                        // 不使用去重，允许多个视频共享同一文件夹
                        video_source_base_path.join(&base_folder_name)
                    }
                }
            }
        } else if is_submission_collection_video
            && (crate::config::reload_config().collection_folder_mode.as_ref() == "up_seasonal"
                || crate::config::reload_config().collection_use_season_structure)
        {
            // 投稿源中的UGC合集视频：在“同UP分季”模式下归并到“源路径/UP主名/UP主名合集”根目录
            let safe_upper_name = crate::utils::filenamify::filenamify(final_video_model.upper_name.trim());
            let up_dir_name = if safe_upper_name.is_empty() {
                format!("UP_{}", final_video_model.upper_id)
            } else {
                safe_upper_name.clone()
            };
            let up_collection_root_name = if safe_upper_name.is_empty() {
                format!("UP_{}合集", final_video_model.upper_id)
            } else if safe_upper_name.ends_with("合集") {
                safe_upper_name
            } else {
                format!("{}合集", safe_upper_name)
            };
            debug!(
                "投稿UGC合集同UP分季模式 - UP主: '{}' ({}), 路径: '{}/{}/{}'",
                final_video_model.upper_name,
                final_video_model.upper_id,
                video_source_base_path.display(),
                up_dir_name,
                up_collection_root_name
            );
            video_source_base_path.join(&up_dir_name).join(&up_collection_root_name)
        } else if matches!(video_source, VideoSourceEnum::Submission(_))
            && !final_video_model.single_page.unwrap_or(true)
            && crate::config::reload_config().collection_folder_mode.as_ref() == "up_seasonal"
        {
            // 多P投稿也并入“同UP合集根目录”，保持目录风格一致：
            // 源路径/UP主名/UP主名合集/<多P视频目录>/Season 01/...
            let safe_upper_name = crate::utils::filenamify::filenamify(final_video_model.upper_name.trim());
            let up_dir_name = if safe_upper_name.is_empty() {
                format!("UP_{}", final_video_model.upper_id)
            } else {
                safe_upper_name.clone()
            };
            let up_collection_root_name = if safe_upper_name.is_empty() {
                format!("UP_{}合集", final_video_model.upper_id)
            } else if safe_upper_name.ends_with("合集") {
                safe_upper_name
            } else {
                format!("{}合集", safe_upper_name)
            };
            let up_collection_root = video_source_base_path.join(&up_dir_name).join(&up_collection_root_name);

            // 渲染多P视频目录名；若模板包含路径分隔符，仅取最后一级，避免重复嵌套UP目录。
            let rendered_folder_name = crate::config::with_config(|bundle| {
                bundle.render_video_template(&video_format_args(&final_video_model))
            })
            .map_err(|e| anyhow::anyhow!("模板渲染失败: {}", e))?;
            let base_folder_name = rendered_folder_name
                .replace('\\', "/")
                .split('/')
                .filter(|seg| !seg.trim().is_empty())
                .next_back()
                .map(|s| s.to_string())
                .unwrap_or_else(|| rendered_folder_name.clone());

            let video_template = crate::config::with_config(|bundle| bundle.config.video_name.as_ref().to_string());
            let needs_deduplication = video_template.contains("title")
                || (video_template.contains("name") && !video_template.contains("upper_name"));

            let multi_page_root = if needs_deduplication {
                let unique_folder_name = generate_unique_folder_name(
                    &up_collection_root,
                    &base_folder_name,
                    &final_video_model,
                    &final_video_model.pubtime.format("%Y-%m-%d").to_string(),
                );
                up_collection_root.join(unique_folder_name)
            } else {
                up_collection_root.join(base_folder_name)
            };

            debug!(
                "多P投稿并入同UP合集目录 - UP主: '{}' ({}), 根目录: '{}', 多P目录: '{}'",
                final_video_model.upper_name,
                final_video_model.upper_id,
                up_collection_root.display(),
                multi_page_root.display()
            );
            multi_page_root
        } else {
            // 其他类型的视频源使用原来的逻辑
            let base_folder_name = crate::config::with_config(|bundle| {
                bundle.render_video_template(&video_format_args(&final_video_model))
            })
            .map_err(|e| anyhow::anyhow!("模板渲染失败: {}", e))?;

            debug!("普通视频源 - 渲染的文件夹名: '{}'", base_folder_name);
            debug!("普通视频源 - 基础路径: {:?}", video_source_base_path);

            // **智能判断：根据模板内容决定是否需要去重**
            let video_template = crate::config::with_config(|bundle| bundle.config.video_name.as_ref().to_string());
            let needs_deduplication = video_template.contains("title")
                || (video_template.contains("name") && !video_template.contains("upper_name"));

            if needs_deduplication {
                // 智能去重：检查文件夹名是否已存在，如果存在则追加唯一标识符
                let unique_folder_name = generate_unique_folder_name(
                    video_source_base_path,
                    &base_folder_name,
                    &final_video_model,
                    &final_video_model.pubtime.format("%Y-%m-%d").to_string(),
                );
                debug!("使用去重文件夹名: '{}'", unique_folder_name);
                let final_path = video_source_base_path.join(&unique_folder_name);
                debug!("最终计算路径: {:?}", final_path);
                final_path
            } else {
                // 不使用去重，允许多个视频共享同一文件夹
                debug!("不使用去重，直接使用基础文件夹名: '{}'", base_folder_name);
                let final_path = video_source_base_path.join(&base_folder_name);
                debug!("最终计算路径: {:?}", final_path);
                final_path
            }
        };

        // 检查是否为多P视频且启用了Season结构
        let config = crate::config::reload_config();
        let is_single_page = final_video_model.single_page.unwrap_or(true);

        let collection_use_season_structure =
            config.collection_use_season_structure || config.collection_folder_mode.as_ref() == "up_seasonal";
        let submission_collection_use_season = is_submission_collection_video
            && (config.collection_folder_mode.as_ref() == "up_seasonal" || config.collection_use_season_structure);

        if !flat_folder
            && ((!is_single_page && config.multi_page_use_season_structure)
                || (is_collection_source && collection_use_season_structure)
                || submission_collection_use_season)
        {
            // 为多P视频或合集创建Season文件夹结构
            let season_folder_name = if is_collection && config.collection_folder_mode.as_ref() == "up_seasonal" {
                match video_source {
                    VideoSourceEnum::Collection(collection_source) => {
                        match get_collection_source_season_number(connection, collection_source, &final_video_model)
                            .await
                        {
                            Ok(season_number) => format!("Season {:02}", season_number.max(1)),
                            Err(err) => {
                                warn!("计算同UP合集季度失败，回退为 Season 01: {}", err);
                                "Season 01".to_string()
                            }
                        }
                    }
                    VideoSourceEnum::Submission(submission_source) if is_submission_collection_video => {
                        match get_submission_source_season_number(connection, submission_source, &final_video_model)
                            .await
                        {
                            Ok(season_number) => format!("Season {:02}", season_number.max(1)),
                            Err(err) => {
                                warn!("计算投稿UGC合集季度失败，回退为 Season 01: {}", err);
                                "Season 01".to_string()
                            }
                        }
                    }
                    _ => "Season 01".to_string(),
                }
            } else if is_submission_collection_video {
                match video_source {
                    VideoSourceEnum::Submission(submission_source) => {
                        match get_submission_source_season_number(connection, submission_source, &final_video_model)
                            .await
                        {
                            Ok(season_number) => format!("Season {:02}", season_number.max(1)),
                            Err(err) => {
                                warn!("计算投稿UGC合集季度失败，回退为 Season 01: {}", err);
                                "Season 01".to_string()
                            }
                        }
                    }
                    _ => "Season 01".to_string(),
                }
            } else {
                "Season 01".to_string()
            };
            let season_path = path.join(&season_folder_name);
            (season_path, Some(season_folder_name), Some(path))
        } else {
            (path, None, None)
        }
    };

    // 延迟创建季度文件夹，只在实际需要写入文件时创建

    let current_config = crate::config::reload_config();
    let collection_use_season_structure = current_config.collection_use_season_structure
        || current_config.collection_folder_mode.as_ref() == "up_seasonal";
    // 使用UP主昵称作为文件夹名，并使用首字进行分类
    let upper_name = crate::utils::filenamify::filenamify(&final_video_model.upper_name);
    let first_char = upper_name.chars().next().context("upper_name is empty")?.to_string();
    let base_upper_path = &current_config.upper_path.join(&first_char).join(&upper_name);
    let is_single_page = final_video_model.single_page.context("single_page is null")?;

    // 为多P视频生成基于视频名称的文件名
    let video_base_name = if !is_single_page {
        // 多P视频启用Season结构时，使用视频根目录的文件夹名作为系列级封面的文件名
        let config = crate::config::reload_config();
        if config.multi_page_use_season_structure && season_folder.is_some() {
            // 从base_path获取视频根目录的文件夹名称
            if let Some(parent) = base_path.parent() {
                if let Some(folder_name) = parent.file_name() {
                    folder_name.to_string_lossy().to_string()
                } else {
                    final_video_model.name.clone() // 回退到视频标题
                }
            } else {
                final_video_model.name.clone() // 回退到视频标题
            }
        } else {
            // 不使用Season结构时，使用模板渲染
            crate::config::with_config(|bundle| bundle.render_video_template(&video_format_args(&final_video_model)))
                .map_err(|e| anyhow::anyhow!("模板渲染失败: {}", e))?
        }
    } else if is_collection {
        // 合集中的单页视频：检查是否启用Season结构
        let config = crate::config::reload_config();
        let collection_like_use_season =
            config.collection_use_season_structure || config.collection_folder_mode.as_ref() == "up_seasonal";
        if collection_like_use_season && season_folder.is_some() {
            // 合集启用Season结构时，使用合集名称作为文件名前缀
            match video_source {
                VideoSourceEnum::Collection(collection_source) => {
                    // 对合集名称进行安全化处理，避免poster/fanart文件名包含斜杠导致创建子文件夹
                    let safe_collection_name = crate::utils::filenamify::filenamify(&collection_source.name);
                    debug!(
                        "合集poster/fanart文件名安全化 - 原名称: '{}', 安全化后: '{}'",
                        collection_source.name, safe_collection_name
                    );
                    safe_collection_name
                }
                VideoSourceEnum::Submission(_) if is_submission_collection_video => {
                    if let Some(season_folder_name) = season_folder.as_ref() {
                        // 与番剧季海报命名保持一致：Season01/Season02...
                        let season_name = if let Some(raw_no) = season_folder_name.strip_prefix("Season ") {
                            if let Ok(no) = raw_no.trim().parse::<i32>() {
                                format!("Season{:02}", no.max(1))
                            } else {
                                "Season01".to_string()
                            }
                        } else {
                            "Season01".to_string()
                        };
                        crate::utils::filenamify::filenamify(&season_name)
                    } else {
                        "Season01".to_string()
                    }
                }
                _ => String::new(),
            }
        } else {
            String::new() // 合集不使用Season结构时不需要
        }
    } else {
        String::new() // 单P视频不需要这些文件
    };

    // 为番剧生成番剧文件夹级别的文件名前缀
    let bangumi_base_name = if is_bangumi {
        if let Some(ref bangumi_folder_path) = bangumi_folder_path {
            // 使用番剧文件夹名称作为前缀
            bangumi_folder_path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| "bangumi".to_string())
        } else {
            "bangumi".to_string()
        }
    } else {
        String::new()
    };

    // 对于单页视频，page 的下载已经足够
    // 对于多页视频，page 下载仅包含了分集内容，需要额外补上视频的 poster 的 tvshow.nfo
    // 使用 tokio::join! 替代装箱的 Future，零分配并行执行

    // 首先检查是否取消
    if token.is_cancelled() {
        return Err(anyhow!("Download cancelled"));
    }

    // 平铺目录模式下（番剧/合集/多P），跳过 TVShow/Season 目录结构相关的元数据文件
    // - 不下载：tvshow.nfo、poster.jpg、folder.jpg、Season01-*.jpg、*-thumb.jpg、*-fanart.jpg
    let disable_tvshow_assets = flat_folder && (is_bangumi || is_collection || !is_single_page);
    if disable_tvshow_assets {
        debug!("平铺目录模式：已启用跳过TVShow/Season元数据下载");
    }

    // 为番剧判断是否下载元数据（依赖数据库状态，不检查文件存在性）
    // 只有第一个集（should_download_upper=true）才负责下载Series级别图片
    let (should_download_bangumi_poster, should_download_bangumi_nfo) =
        if is_bangumi && bangumi_folder_path.is_some() && should_download_upper && !disable_tvshow_assets {
            let config = crate::config::reload_config();

            // 如果启用了番剧统一Season结构，跳过带番剧名的图片下载
            // 因为已经有Season级别的图片和poster.jpg了
            if config.bangumi_use_season_structure {
                // 不下载带番剧名的图片，依赖should_run控制NFO下载
                (false, true)
            } else {
                // 未启用统一Season结构时，依赖should_run参数控制下载
                (true, true)
            }
        } else {
            (false, false)
        };

    // 为启用Season结构的合集视频判断是否下载封面（依赖数据库状态，不检查文件存在性）
    let should_download_season_poster = if !is_bangumi {
        if disable_tvshow_assets {
            false
        } else {
            let config = crate::config::reload_config();
            let uses_season_structure = (is_collection && collection_use_season_structure)
                || (!is_single_page && config.multi_page_use_season_structure);

            if uses_season_structure && season_folder.is_some() {
                // 对于合集，只有第一个视频才下载合集封面
                if is_collection {
                    match video_source {
                        VideoSourceEnum::Collection(collection_source) => {
                            let season_asset_missing = season_folder
                                .as_ref()
                                .map(|season_folder_name| {
                                    let season_name = if let Some(raw_no) = season_folder_name.strip_prefix("Season ") {
                                        if let Ok(no) = raw_no.trim().parse::<i32>() {
                                            format!("Season{:02}", no.max(1))
                                        } else {
                                            "Season01".to_string()
                                        }
                                    } else {
                                        "Season01".to_string()
                                    };
                                    let root_dir = base_path.parent().unwrap_or(&base_path);
                                    !root_dir.join(format!("{}-thumb.jpg", season_name)).exists()
                                        || !root_dir.join(format!("{}-fanart.jpg", season_name)).exists()
                                })
                                .unwrap_or(false);
                            match get_collection_video_episode_number(
                                connection,
                                collection_source.id,
                                &video_model.bvid,
                            )
                            .await
                            {
                                Ok(episode_number) => {
                                    let is_first_episode = episode_number == 1;
                                    info!(
                                        "合集「{}」视频「{}」集数检查: episode={}, is_first={}",
                                        collection_source.name, video_model.name, episode_number, is_first_episode
                                    );
                                    is_first_episode || season_asset_missing
                                }
                                Err(e) => {
                                    warn!("获取合集视频集数失败: {}, 跳过合集封面下载", e);
                                    false
                                }
                            }
                        }
                        VideoSourceEnum::Submission(_) if is_submission_collection_video => {
                            let episode_number = final_video_model.episode_number.unwrap_or(0);
                            let is_first_episode = episode_number == 1;
                            let root_dir = base_path.parent().unwrap_or(&base_path);
                            let should_force_backfill =
                                !root_dir.join("poster.jpg").exists() || !root_dir.join("folder.jpg").exists();
                            let season_asset_missing = season_folder
                                .as_ref()
                                .map(|season_folder_name| {
                                    let season_name = if let Some(raw_no) = season_folder_name.strip_prefix("Season ") {
                                        if let Ok(no) = raw_no.trim().parse::<i32>() {
                                            format!("Season{:02}", no.max(1))
                                        } else {
                                            "Season01".to_string()
                                        }
                                    } else {
                                        "Season01".to_string()
                                    };
                                    !root_dir.join(format!("{}-thumb.jpg", season_name)).exists()
                                        || !root_dir.join(format!("{}-fanart.jpg", season_name)).exists()
                                })
                                .unwrap_or(false);
                            debug!(
                                "投稿UGC合集视频「{}」集数检查: episode={}, is_first={}, force_backfill={}, season_asset_missing={}",
                                video_model.name, episode_number, is_first_episode, should_force_backfill, season_asset_missing
                            );
                            is_first_episode || should_force_backfill || season_asset_missing
                        }
                        _ => false,
                    }
                } else {
                    true // 非合集的多P视频，依赖should_run参数控制
                }
            } else {
                true // 未启用Season结构时不进行检查
            }
        }
    } else {
        true // 番剧不在此处检查
    };

    // 合集/多P Season结构下，若目录级元数据缺失，允许本轮任意分集补齐。
    let should_backfill_collection_root_assets =
        if !disable_tvshow_assets && season_folder.is_some() && is_collection && collection_use_season_structure {
            let root_dir = base_path.parent().unwrap_or(&base_path);
            !root_dir.join("tvshow.nfo").exists()
                || !root_dir.join("poster.jpg").exists()
                || !root_dir.join("folder.jpg").exists()
        } else {
            false
        };
    let should_backfill_collection_season_nfo =
        if !disable_tvshow_assets && season_folder.is_some() && is_collection && collection_use_season_structure {
            !base_path.join("season.nfo").exists()
        } else {
            false
        };

    // 先处理NFO生成（独立执行，避免tokio::join!类型问题）
    let nfo_result = if is_bangumi && season_info.is_some() {
        // 番剧且有API数据：使用API驱动的NFO生成
        // 注意：启用Season结构时，bangumi_folder_path已经指向系列根目录
        generate_bangumi_video_nfo(
            separate_status[2] && bangumi_folder_path.is_some() && should_download_bangumi_nfo,
            &video_model,
            season_info.as_ref().unwrap(),
            bangumi_folder_path.as_ref().unwrap().join("tvshow.nfo"),
        )
        .await
    } else {
        // 普通视频或番剧无API数据：使用原有逻辑
        // 对于合集，只在第一个视频时生成tvshow.nfo，避免重复生成
        let should_generate_nfo = if is_bangumi {
            // 番剧：只有在文件不存在时才生成，放在番剧文件夹根目录
            separate_status[2] && bangumi_folder_path.is_some() && should_download_bangumi_nfo
        } else if is_collection {
            // 合集：只有第一个视频时生成tvshow.nfo
            if !disable_tvshow_assets && separate_status[2] && collection_use_season_structure {
                // 检查是否为第一个视频
                match video_source {
                    VideoSourceEnum::Collection(collection_source) => {
                        match get_collection_video_episode_number(connection, collection_source.id, &video_model.bvid)
                            .await
                        {
                            Ok(episode_number) => {
                                episode_number == 1
                                    || should_backfill_collection_root_assets
                                    || should_backfill_collection_season_nfo
                            }
                            Err(_) => false,
                        }
                    }
                    VideoSourceEnum::Submission(_) if is_submission_collection_video => {
                        final_video_model.episode_number.unwrap_or(0) == 1
                            || should_backfill_collection_root_assets
                            || should_backfill_collection_season_nfo
                    }
                    _ => false,
                }
            } else {
                false
            }
        } else {
            // 普通视频：为多P视频生成nfo
            separate_status[2] && !is_single_page && !disable_tvshow_assets
        };

        if should_generate_nfo && is_collection {
            // 合集：使用带合集信息的NFO生成（第一个视频时）
            let nfo_fixed_root_name = {
                let cfg = crate::config::reload_config();
                // 在“同UP分季”或“投稿UGC合集+Season结构”场景下，tvshow 标题固定使用根目录名，
                // 避免回退成某个官方合集标题（如第一季名）导致媒体库聚合错乱。
                let lock_to_root_name = cfg.collection_folder_mode.as_ref() == "up_seasonal"
                    || (is_submission_collection_video && collection_use_season_structure);
                if lock_to_root_name {
                    base_path
                        .parent()
                        .and_then(|p| p.file_name())
                        .map(|s| s.to_string_lossy().to_string())
                        .filter(|s| !s.trim().is_empty())
                } else {
                    None
                }
            };

            let (collection_name, collection_cover, collection_description): (
                Option<String>,
                Option<String>,
                Option<String>,
            ) = match video_source {
                VideoSourceEnum::Collection(collection_source) => {
                    let collection_cover = match collection::Entity::find_by_id(collection_source.id)
                        .one(connection)
                        .await
                    {
                        Ok(Some(fresh_collection)) => fresh_collection.cover.clone(),
                        _ => None,
                    };
                    let name = nfo_fixed_root_name
                        .clone()
                        .unwrap_or_else(|| collection_source.name.clone());
                    (Some(name), collection_cover, None)
                }
                VideoSourceEnum::Submission(submission_source) if is_submission_collection_video => {
                    let mut name: Option<String> = None;
                    let mut cover: Option<String> = None;
                    let mut description: Option<String> = None;

                    if let Some(season_id) = final_video_model.season_id.as_deref().filter(|s| !s.trim().is_empty()) {
                        if let Some(meta) = get_submission_collection_meta(
                            bili_client,
                            submission_source.upper_id,
                            season_id,
                            token.clone(),
                        )
                        .await
                        {
                            name = Some(meta.name);
                            cover = meta.cover;
                            description = meta.description;
                        }
                    }

                    // 兜底：如果接口无法获取官方合集标题，至少使用根目录名，避免误写为第一集标题。
                    if name.is_none() {
                        let root_name = base_path
                            .parent()
                            .and_then(|p| p.file_name())
                            .map(|s| s.to_string_lossy().to_string())
                            .filter(|s| !s.trim().is_empty());
                        if root_name.is_some() {
                            name = root_name;
                        }
                    }

                    if let Some(ref fixed_root_name) = nfo_fixed_root_name {
                        name = Some(fixed_root_name.clone());
                    }

                    (name, cover, description)
                }
                _ => (None, None, None),
            };

            let nfo_stats = match get_collection_nfo_stats(connection, video_source, &final_video_model).await {
                Ok(stats) => stats,
                Err(err) => {
                    warn!("计算合集NFO统计信息失败，回退默认值: {}", err);
                    CollectionNfoStats {
                        total_seasons: 1,
                        total_episodes: 1,
                        season_total_episodes: 1,
                    }
                }
            };

            generate_collection_video_nfo(
                true,
                &video_model,
                collection_name.as_deref(),
                collection_cover.as_deref(),
                collection_description.as_deref(),
                if let Some(ref bangumi_path) = bangumi_folder_path {
                    // 多P视频或合集使用Season结构时，tvshow.nfo放在视频根目录
                    let config = crate::config::reload_config();
                    if ((!is_single_page && config.multi_page_use_season_structure)
                        || (is_collection && collection_use_season_structure))
                        && season_folder.is_some()
                    {
                        bangumi_path.join("tvshow.nfo")
                    } else {
                        // 不使用Season结构时，保持原有逻辑
                        base_path.join(format!("{}.nfo", video_base_name))
                    }
                } else {
                    // 多P视频或合集使用Season结构时，tvshow.nfo放在视频根目录
                    let config = crate::config::reload_config();
                    if ((!is_single_page && config.multi_page_use_season_structure)
                        || (is_collection && collection_use_season_structure))
                        && season_folder.is_some()
                    {
                        // 需要从base_path（Season文件夹）回到父目录（视频根目录）
                        base_path
                            .parent()
                            .map(|parent| parent.join("tvshow.nfo"))
                            .unwrap_or_else(|| base_path.join("tvshow.nfo"))
                    } else {
                        // 普通视频nfo放在视频文件夹
                        base_path.join(format!("{}.nfo", video_base_name))
                    }
                },
                season_folder
                    .as_deref()
                    .and_then(extract_season_number_from_path)
                    .or_else(|| extract_season_number_from_path(&base_path.to_string_lossy()))
                    .unwrap_or(1)
                    .max(1),
                Some(nfo_stats.total_seasons),
                Some(nfo_stats.total_episodes),
                Some(nfo_stats.season_total_episodes),
                if season_folder.is_some() {
                    Some(base_path.join("season.nfo"))
                } else {
                    None
                },
            )
            .await
        } else {
            // 普通视频或番剧：使用原有逻辑
            generate_video_nfo(
                should_generate_nfo,
                &video_model,
                if let Some(ref bangumi_path) = bangumi_folder_path {
                    if is_bangumi {
                        // 番剧tvshow.nfo放在番剧文件夹根目录，使用固定文件名
                        bangumi_path.join("tvshow.nfo")
                    } else {
                        // 多P视频或合集使用Season结构时，tvshow.nfo放在视频根目录
                        let config = crate::config::reload_config();
                        if ((!is_single_page && config.multi_page_use_season_structure)
                            || (is_collection && collection_use_season_structure))
                            && season_folder.is_some()
                        {
                            bangumi_path.join("tvshow.nfo")
                        } else {
                            // 不使用Season结构时，保持原有逻辑
                            base_path.join(format!("{}.nfo", video_base_name))
                        }
                    }
                } else {
                    // 多P视频或合集使用Season结构时，tvshow.nfo放在视频根目录
                    let config = crate::config::reload_config();
                    if ((!is_single_page && config.multi_page_use_season_structure)
                        || (is_collection && collection_use_season_structure))
                        && season_folder.is_some()
                    {
                        // 需要从base_path（Season文件夹）回到父目录（视频根目录）
                        base_path
                            .parent()
                            .map(|parent| parent.join("tvshow.nfo"))
                            .unwrap_or_else(|| base_path.join("tvshow.nfo"))
                    } else {
                        // 普通视频nfo放在视频文件夹
                        base_path.join(format!("{}.nfo", video_base_name))
                    }
                },
            )
            .await
        }
    };

    // 预先获取合集封面URL（如果需要）
    let collection_cover_url = if is_collection && should_download_season_poster {
        if let VideoSourceEnum::Collection(collection_source) = video_source {
            match get_collection_video_episode_number(connection, collection_source.id, &video_model.bvid).await {
                Ok(1) => {
                    // 第一个视频时从数据库重新获取最新的合集信息
                    match collection::Entity::find_by_id(collection_source.id)
                        .one(connection)
                        .await
                    {
                        Ok(Some(fresh_collection)) => match fresh_collection.cover.as_ref() {
                            Some(cover_url) if !cover_url.is_empty() => {
                                info!("合集「{}」使用数据库保存的封面: {}", fresh_collection.name, cover_url);
                                Some(cover_url.clone())
                            }
                            _ => {
                                info!("合集「{}」数据库中无封面URL，使用视频封面", fresh_collection.name);
                                None
                            }
                        },
                        Ok(None) => {
                            warn!("合集ID {} 在数据库中不存在", collection_source.id);
                            None
                        }
                        Err(e) => {
                            warn!("查询合集信息失败: {}", e);
                            None
                        }
                    }
                }
                _ => {
                    // 非第一个视频，不需要重复获取
                    None
                }
            }
        } else {
            None
        }
    } else {
        None
    };

    // 为有Season文件夹的番剧生成season.nfo（无论是否启用统一结构）
    let season_nfo_result = if is_bangumi && season_info.is_some() && season_folder.is_some() {
        let config = crate::config::reload_config();

        // 确定使用的season_number
        let season_number = if config.bangumi_use_season_structure {
            // 启用统一结构：使用从番剧名称提取的季度编号
            let series_title = season_info.as_ref().unwrap().title.as_str();
            let (_, extracted_season_number) =
                crate::utils::bangumi_name_extractor::BangumiNameExtractor::extract_series_name_and_season(
                    series_title,
                    None, // 不提供season_title，让提取器从完整标题中识别
                );
            extracted_season_number
        } else {
            // 未启用统一结构：使用video_model中的原始season_number
            video_model.season_number.unwrap_or(1) as u32
        };

        let series_title = season_info.as_ref().unwrap().title.as_str();
        info!("番剧「{}」使用季度编号: {}", series_title, season_number);

        // season.nfo生成依赖separate_status[2]状态（重置状态后会强制重新生成）
        let should_generate_season_nfo = separate_status[2];

        generate_bangumi_season_nfo(
            should_generate_season_nfo,
            &video_model,
            season_info.as_ref().unwrap(),
            base_path.clone(),
            season_number,
        )
        .await
    } else {
        Ok(ExecutionStatus::Skipped)
    };

    // 为启用Season结构的番剧下载季度级图片
    // 只有第一个集（should_download_upper=true）才负责下载Season级别图片
    let season_images_result = if is_bangumi && season_info.is_some() && should_download_upper && !disable_tvshow_assets
    {
        let config = crate::config::reload_config();
        if config.bangumi_use_season_structure {
            // 启用统一结构：使用从番剧名称提取的季度编号
            let series_title = season_info.as_ref().unwrap().title.as_str();
            let (_, extracted_season_number) =
                crate::utils::bangumi_name_extractor::BangumiNameExtractor::extract_series_name_and_season(
                    series_title,
                    None, // 不提供season_title，让提取器从完整标题中识别
                );
            let season_number = extracted_season_number;

            // 季度级图片应该放在系列根目录，使用标准命名
            let series_root = bangumi_folder_path.as_ref().unwrap();
            let poster_path = series_root.join(format!("Season{:02}-thumb.jpg", season_number));
            let fanart_path = series_root.join(format!("Season{:02}-fanart.jpg", season_number));

            // 定义所有Season级别文件路径
            let season_poster_path = series_root.join(format!("Season{:02}-poster.jpg", season_number));

            // Emby兼容性：添加folder.jpg和poster.jpg
            let folder_path = series_root.join("folder.jpg");
            let generic_poster_path = series_root.join("poster.jpg");

            // 依赖数据库状态决定是否下载季度级图片（不检查文件存在性，以支持重置状态后重新下载）
            let should_download_season_images = separate_status[0];

            info!(
                "准备下载季度级图片到: {:?}, {:?}, {:?}, {:?} 和 {:?}",
                poster_path, fanart_path, season_poster_path, folder_path, generic_poster_path
            );

            // 季度级图片：thumb使用横版封面，fanart使用竖版封面
            let season_info_ref = season_info.as_ref().unwrap();
            // thumb使用横版封面（优先级：横版封面 > 专门背景图 > 竖版封面 > 新EP封面截图）
            let season_thumb_url = season_info_ref
                .horizontal_cover_169
                .as_deref()
                .filter(|s| !s.is_empty())
                .or(season_info_ref
                    .horizontal_cover_1610
                    .as_deref()
                    .filter(|s| !s.is_empty()))
                .or(season_info_ref.bkg_cover.as_deref().filter(|s| !s.is_empty()))
                .or(season_info_ref.cover.as_deref().filter(|s| !s.is_empty()))
                .or(season_info_ref.new_ep_cover.as_deref().filter(|s| !s.is_empty()));
            // fanart使用竖版封面
            let season_fanart_url = season_info_ref.cover.as_deref().filter(|s| !s.is_empty());

            debug!("Season级别图片选择逻辑:");
            debug!("  Season{:02}-thumb.jpg URL: {:?}", season_number, season_thumb_url);
            debug!("  Season{:02}-fanart.jpg URL: {:?}", season_number, season_fanart_url);

            // 并行下载五个Season级别的图片文件
            let (thumb_result, fanart_result, poster_result, folder_result, generic_poster_result) = tokio::join!(
                // Season01-thumb.jpg (横版封面)
                fetch_video_poster(
                    should_download_season_images,
                    &video_model,
                    downloader,
                    poster_path.clone(),                   // 这里实际是thumb路径
                    std::path::PathBuf::from("/dev/null"), // 占位，因为我们只下载一个文件
                    token.clone(),
                    season_thumb_url, // 使用横版封面作为thumb
                    None,             // 不使用fanart URL
                ),
                // Season01-fanart.jpg (竖版封面)
                fetch_video_poster(
                    should_download_season_images,
                    &video_model,
                    downloader,
                    fanart_path.clone(),
                    std::path::PathBuf::from("/dev/null"), // 占位，因为我们只下载一个文件
                    token.clone(),
                    season_fanart_url, // 使用竖版封面作为fanart
                    None,              // 不使用fanart URL
                ),
                // Season01-poster.jpg (竖版封面)
                fetch_bangumi_poster(
                    should_download_season_images,
                    &video_model,
                    downloader,
                    season_poster_path,
                    token.clone(),
                    season_fanart_url, // 使用竖版封面作为主封面
                ),
                // folder.jpg (竖版封面，Emby优先识别)
                fetch_bangumi_poster(
                    should_download_season_images,
                    &video_model,
                    downloader,
                    folder_path,
                    token.clone(),
                    season_fanart_url, // 使用竖版封面
                ),
                // poster.jpg (竖版封面，通用封面)
                fetch_bangumi_poster(
                    should_download_season_images,
                    &video_model,
                    downloader,
                    generic_poster_path,
                    token.clone(),
                    season_fanart_url, // 使用竖版封面
                )
            );

            // 返回综合结果
            Some(
                match (
                    thumb_result,
                    fanart_result,
                    poster_result,
                    folder_result,
                    generic_poster_result,
                ) {
                    // 只要都是Ok且至少有一个Succeeded，就算成功
                    (Ok(ExecutionStatus::Succeeded), _, _, _, _)
                    | (_, Ok(ExecutionStatus::Succeeded), _, _, _)
                    | (_, _, Ok(ExecutionStatus::Succeeded), _, _)
                    | (_, _, _, Ok(ExecutionStatus::Succeeded), _)
                    | (_, _, _, _, Ok(ExecutionStatus::Succeeded)) => ExecutionStatus::Succeeded,
                    // 都是Ok但都是Skipped
                    (
                        Ok(ExecutionStatus::Skipped),
                        Ok(ExecutionStatus::Skipped),
                        Ok(ExecutionStatus::Skipped),
                        Ok(ExecutionStatus::Skipped),
                        Ok(ExecutionStatus::Skipped),
                    ) => ExecutionStatus::Skipped,
                    // 有任何错误才报Failed
                    _ => ExecutionStatus::Failed(anyhow::anyhow!("Season级别图片下载失败")),
                },
            )
        } else {
            Some(ExecutionStatus::Skipped)
        }
    } else {
        Some(ExecutionStatus::Skipped)
    };

    let (res_1, res_2, res_folder, res_3, res_4, res_5) = tokio::join!(
        // 下载视频封面（番剧和普通视频采用不同策略）
        fetch_video_poster(
            if is_bangumi {
                // 番剧：只有在文件不存在时才下载，放在番剧文件夹根目录
                separate_status[0] && bangumi_folder_path.is_some() && should_download_bangumi_poster
            } else {
                // 普通视频：为多P视频或启用Season结构的合集生成封面，并检查文件是否已存在
                separate_status[0]
                    && (!is_single_page || (is_collection && collection_use_season_structure))
                    && should_download_season_poster
            },
            &video_model,
            downloader,
            if is_bangumi && bangumi_folder_path.is_some() {
                // 番剧封面放在番剧文件夹根目录
                let config = crate::config::reload_config();
                if config.bangumi_use_season_structure {
                    // 启用统一Season结构时，跳过带番剧名的图片下载
                    // 使用一个不存在的路径，这样下载会被跳过
                    bangumi_folder_path.as_ref().unwrap().join("__skip_download__")
                } else {
                    // 未启用统一Season结构时，保持原有逻辑
                    bangumi_folder_path
                        .as_ref()
                        .unwrap()
                        .join(format!("{}-thumb.jpg", bangumi_base_name))
                }
            } else {
                // 多P视频或合集使用Season结构时，封面放在视频根目录
                let config = crate::config::reload_config();
                if (!is_single_page && config.multi_page_use_season_structure && season_folder.is_some())
                    || (is_collection && collection_use_season_structure && season_folder.is_some())
                {
                    // 需要从base_path（Season文件夹）回到父目录（视频根目录）
                    base_path
                        .parent()
                        .map(|parent| parent.join(format!("{}-thumb.jpg", video_base_name)))
                        .unwrap_or_else(|| base_path.join(format!("{}-thumb.jpg", video_base_name)))
                } else {
                    // 普通视频封面放在视频文件夹
                    base_path.join(format!("{}-thumb.jpg", video_base_name))
                }
            },
            if is_bangumi && bangumi_folder_path.is_some() {
                // 番剧fanart放在番剧文件夹根目录
                let config = crate::config::reload_config();
                if config.bangumi_use_season_structure {
                    // 启用统一Season结构时，跳过带番剧名的图片下载
                    // 使用一个不存在的路径，这样下载会被跳过
                    bangumi_folder_path.as_ref().unwrap().join("__skip_download__")
                } else {
                    // 未启用统一Season结构时，保持原有逻辑
                    bangumi_folder_path
                        .as_ref()
                        .unwrap()
                        .join(format!("{}-fanart.jpg", bangumi_base_name))
                }
            } else {
                // 多P视频或合集使用Season结构时，fanart放在视频根目录
                let config = crate::config::reload_config();
                if (!is_single_page && config.multi_page_use_season_structure && season_folder.is_some())
                    || (is_collection && collection_use_season_structure && season_folder.is_some())
                {
                    // 需要从base_path（Season文件夹）回到父目录（视频根目录）
                    base_path
                        .parent()
                        .map(|parent| parent.join(format!("{}-fanart.jpg", video_base_name)))
                        .unwrap_or_else(|| base_path.join(format!("{}-fanart.jpg", video_base_name)))
                } else {
                    // 普通视频fanart放在视频文件夹
                    base_path.join(format!("{}-fanart.jpg", video_base_name))
                }
            },
            token.clone(),
            // thumb URL选择逻辑：番剧使用横版封面，合集和普通视频使用默认封面
            if is_bangumi && season_info.is_some() {
                let season = season_info.as_ref().unwrap();
                let thumb_url = season
                    .horizontal_cover_169
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .or(season.horizontal_cover_1610.as_deref().filter(|s| !s.is_empty()))
                    .or(season.bkg_cover.as_deref().filter(|s| !s.is_empty()))
                    .or(season.cover.as_deref().filter(|s| !s.is_empty()))
                    .or(season.new_ep_cover.as_deref().filter(|s| !s.is_empty()));

                debug!("番剧「{}」thumb选择逻辑:", video_model.name);
                debug!(
                    "  字段值: h169={:?}, h1610={:?}, bkg={:?}, cover={:?}, new_ep_cover={:?}",
                    season.horizontal_cover_169,
                    season.horizontal_cover_1610,
                    season.bkg_cover,
                    season.cover,
                    season.new_ep_cover
                );
                debug!("  最终选择的thumb URL: {:?}", thumb_url);

                thumb_url
            } else if let Some(ref cover_url) = collection_cover_url {
                Some(cover_url.as_str())
            } else {
                None
            },
            // fanart URL选择逻辑：番剧使用竖版封面，普通视频复用poster
            if is_bangumi && season_info.is_some() {
                let season = season_info.as_ref().unwrap();
                let fanart_url = season.cover.as_deref().filter(|s| !s.is_empty());

                debug!("番剧「{}」fanart选择逻辑:", video_model.name);
                debug!("  最终选择的fanart URL: {:?}", fanart_url);

                fanart_url
            } else {
                None
            },
        ),
        // 下载番剧/多P/合集根目录的 poster.jpg（Emby兼容性）
        fetch_bangumi_poster(
            {
                let config = crate::config::reload_config();
                if is_bangumi && bangumi_folder_path.is_some() {
                    // 番剧：无论是否启用Season结构都下载
                    should_download_bangumi_poster
                } else {
                    // 多P视频或合集：启用Season结构时才下载根目录封面
                    separate_status[0]
                        && ((!is_single_page && config.multi_page_use_season_structure && season_folder.is_some())
                            || (is_collection && collection_use_season_structure && season_folder.is_some()))
                        && should_download_season_poster
                }
            },
            &video_model,
            downloader,
            if is_bangumi && bangumi_folder_path.is_some() {
                // 番剧根目录的 poster.jpg
                bangumi_folder_path.as_ref().unwrap().join("poster.jpg")
            } else {
                // 多P视频或合集根目录的 poster.jpg
                let config = crate::config::reload_config();
                if (!is_single_page && config.multi_page_use_season_structure && season_folder.is_some())
                    || (is_collection && collection_use_season_structure && season_folder.is_some())
                {
                    base_path
                        .parent()
                        .map(|parent| parent.join("poster.jpg"))
                        .unwrap_or_else(|| base_path.join("poster.jpg"))
                } else {
                    std::path::PathBuf::from("/dev/null")
                }
            },
            token.clone(),
            // 使用竖版封面作为主封面
            if is_bangumi && season_info.is_some() {
                let season = season_info.as_ref().unwrap();
                let poster_url = season
                    .series_cover
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .or(season.cover.as_deref().filter(|s| !s.is_empty()));
                debug!("番剧「{}」poster.jpg选择逻辑:", video_model.name);
                debug!("  最终选择的poster URL: {:?}", poster_url);
                poster_url
            } else if let Some(ref cover_url) = collection_cover_url {
                // 合集也使用封面URL
                Some(cover_url.as_str())
            } else {
                None
            },
        ),
        // 下载番剧/多P/合集根目录的 folder.jpg（Emby兼容性，优先级最高）
        fetch_bangumi_poster(
            {
                let config = crate::config::reload_config();
                if is_bangumi && bangumi_folder_path.is_some() {
                    // 番剧：无论是否启用Season结构都下载
                    should_download_bangumi_poster
                } else {
                    // 多P视频或合集：启用Season结构时才下载根目录封面
                    separate_status[0]
                        && ((!is_single_page && config.multi_page_use_season_structure && season_folder.is_some())
                            || (is_collection && collection_use_season_structure && season_folder.is_some()))
                        && should_download_season_poster
                }
            },
            &video_model,
            downloader,
            if is_bangumi && bangumi_folder_path.is_some() {
                // 番剧根目录的 folder.jpg
                bangumi_folder_path.as_ref().unwrap().join("folder.jpg")
            } else {
                // 多P视频或合集根目录的 folder.jpg
                let config = crate::config::reload_config();
                if (!is_single_page && config.multi_page_use_season_structure && season_folder.is_some())
                    || (is_collection && collection_use_season_structure && season_folder.is_some())
                {
                    base_path
                        .parent()
                        .map(|parent| parent.join("folder.jpg"))
                        .unwrap_or_else(|| base_path.join("folder.jpg"))
                } else {
                    std::path::PathBuf::from("/dev/null")
                }
            },
            token.clone(),
            // 使用竖版封面作为主封面
            if is_bangumi && season_info.is_some() {
                let season = season_info.as_ref().unwrap();
                let folder_url = season
                    .series_cover
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .or(season.cover.as_deref().filter(|s| !s.is_empty()));
                debug!("番剧「{}」folder.jpg选择逻辑:", video_model.name);
                debug!("  最终选择的folder URL: {:?}", folder_url);
                folder_url
            } else if let Some(ref cover_url) = collection_cover_url {
                // 合集也使用封面URL
                Some(cover_url.as_str())
            } else {
                None
            },
        ),
        // 下载 Up 主头像（番剧跳过，因为番剧没有UP主信息）
        fetch_upper_face(
            separate_status[2] && should_download_upper && !is_bangumi,
            &final_video_model,
            downloader,
            base_upper_path.join("folder.jpg"),
            token.clone(),
        ),
        // 生成 Up 主信息的 nfo（番剧跳过，因为番剧没有UP主信息）
        generate_upper_nfo(
            separate_status[3] && should_download_upper && !is_bangumi,
            &final_video_model,
            base_upper_path.join("person.nfo"),
        ),
        // 分发并执行分 P 下载的任务
        dispatch_download_page(
            DownloadPageArgs {
                should_run: separate_status[4],
                bili_client,
                video_source,
                video_model: &final_video_model,
                pages,
                connection,
                downloader,
                base_path: &base_path,
            },
            token.clone()
        )
    );

    // 主要的5个任务结果，保持与VideoStatus<5>兼容
    let mut main_results = [res_1, nfo_result, res_3, res_4, res_5]
        .into_iter()
        .map(Into::into)
        .collect::<Vec<_>>();

    // 若重置后遇到 B站-404（视频被删除/不可访问），将本次重置视为无效：
    // - 把需要执行的任务全部标记为 Skipped（等同于“恢复为未重置”）
    // - 更新数据库：video.valid=false，pages.download_status=OK（避免分页仍显示未完成）
    let hit_bili_404 = main_results.iter().any(|r| match r {
        ExecutionStatus::Ignored(e) => is_bili_request_failed_404(e),
        _ => false,
    });

    if hit_bili_404 && has_existing_page_paths {
        use sea_orm::sea_query::Expr;
        use sea_orm::{Set, Unchanged};

        warn!(
            "视频「{}」({}) 已在B站删除/不可访问，已跳过本次重置并恢复为未重置状态",
            &final_video_model.name, &final_video_model.bvid
        );

        // 把本轮“需要执行”的任务全部标记为跳过，让状态位恢复为完成
        for (idx, should_run) in separate_status.iter().enumerate() {
            if *should_run {
                main_results[idx] = ExecutionStatus::Skipped;
            }
        }

        // 立即修复数据库分页状态，避免前端仍显示“分页未完成”
        let ok_page_status: u32 = PageStatus::from([STATUS_OK; 5]).into();
        let txn = connection.begin().await?;
        video::Entity::update(video::ActiveModel {
            id: Unchanged(final_video_model.id),
            valid: Set(false),
            ..Default::default()
        })
        .exec(&txn)
        .await?;
        page::Entity::update_many()
            .col_expr(page::Column::DownloadStatus, Expr::value(ok_page_status))
            .filter(page::Column::VideoId.eq(final_video_model.id))
            .exec(&txn)
            .await?;
        txn.commit().await?;
    }

    status.update_status(&main_results);

    // 下载联合投稿中其他UP主的头像（在主UP主头像下载之后）
    let staff_faces_result = if !is_bangumi && should_download_upper {
        fetch_staff_faces(separate_status[2], &final_video_model, downloader, token.clone()).await
    } else {
        Ok(ExecutionStatus::Skipped)
    };

    // 额外的结果单独处理（季度NFO、季度图片、根目录Emby兼容封面、staff头像）
    let extra_results = [
        Ok(season_nfo_result.unwrap_or(ExecutionStatus::Skipped)),
        Ok(season_images_result.unwrap_or(ExecutionStatus::Skipped)),
        res_2,              // 番剧/多P/合集根目录 poster.jpg 的结果（Emby兼容）
        res_folder,         // 番剧/多P/合集根目录 folder.jpg 的结果（Emby优先识别）
        staff_faces_result, // staff成员头像下载结果
    ]
    .into_iter()
    .map(Into::into)
    .collect::<Vec<_>>();

    // 合并所有结果用于日志处理
    let mut all_results = main_results;
    all_results.extend(extra_results);

    // 充电视频在获取详情时已经被upower字段检测并处理，无需后期检测

    all_results
        .iter()
        .take(9)
        .zip([
            "封面",
            "详情",
            "作者头像",
            "作者详情",
            "分页下载",
            "季度NFO",
            "季度图片",
            "根目录poster",
            "根目录folder",
        ])
        .for_each(|(res, task_name)| match res {
            ExecutionStatus::Skipped => debug!("处理视频「{}」{}已成功过，跳过", &video_model.name, task_name),
            ExecutionStatus::Succeeded => debug!("处理视频「{}」{}成功", &video_model.name, task_name),
            ExecutionStatus::Cancelled => info!("处理视频「{}」{}因用户暂停而终止", &video_model.name, task_name),
            ExecutionStatus::Ignored(e) => {
                let error_msg = e.to_string();
                if !error_msg.contains("status code: 87007") {
                    info!(
                        "处理视频「{}」{}出现常见错误，已忽略: {:#}",
                        &video_model.name, task_name, e
                    );
                }
            }
            ExecutionStatus::ClassifiedFailed(classified_error) => {
                // 根据错误分类进行不同级别的日志记录
                match classified_error.error_type {
                    crate::error::ErrorType::NotFound => {
                        debug!(
                            "处理视频「{}」{}失败({}): {}",
                            &video_model.name, task_name, classified_error.error_type, classified_error.message
                        );
                    }
                    crate::error::ErrorType::Permission => {
                        // 权限错误（充电专享视频现在在获取详情时处理）
                        info!(
                            "跳过视频「{}」{}: {}",
                            &video_model.name, task_name, classified_error.message
                        );
                    }
                    crate::error::ErrorType::Network
                    | crate::error::ErrorType::Timeout
                    | crate::error::ErrorType::RateLimit => {
                        warn!(
                            "处理视频「{}」{}失败({}): {}{}",
                            &video_model.name,
                            task_name,
                            classified_error.error_type,
                            classified_error.message,
                            if classified_error.should_retry {
                                " (可重试)"
                            } else {
                                ""
                            }
                        );
                    }
                    crate::error::ErrorType::RiskControl => {
                        error!(
                            "处理视频「{}」{}触发风控: {}",
                            &video_model.name, task_name, classified_error.message
                        );
                    }
                    crate::error::ErrorType::UserCancelled => {
                        info!("处理视频「{}」{}因用户暂停而终止", &video_model.name, task_name);
                    }
                    _ => {
                        error!(
                            "处理视频「{}」{}失败({}): {}",
                            &video_model.name, task_name, classified_error.error_type, classified_error.message
                        );
                    }
                }
            }
            ExecutionStatus::Failed(e) | ExecutionStatus::FixedFailed(_, e) => {
                // 使用错误分类器进行统一处理
                #[allow(clippy::needless_borrow)]
                let classified_error = crate::error::ErrorClassifier::classify_error(&e);
                match classified_error.error_type {
                    crate::error::ErrorType::NotFound => {
                        debug!("处理视频「{}」{}失败(404): {:#}", &video_model.name, task_name, e);
                    }
                    crate::error::ErrorType::UserCancelled => {
                        info!("处理视频「{}」{}因用户暂停而终止", &video_model.name, task_name);
                    }
                    _ => {
                        // 对于分页下载任务，错误日志已经在内部处理了，这里只记录DEBUG级别
                        if task_name == "分页下载" {
                            debug!("处理视频「{}」{}失败: {:#}", &video_model.name, task_name, e);
                        } else {
                            error!("处理视频「{}」{}失败: {:#}", &video_model.name, task_name, e);
                        }
                    }
                }
            }
        });
    if let ExecutionStatus::Failed(e) = all_results
        .into_iter()
        .nth(4)
        .context("page download result not found")?
    {
        if e.downcast_ref::<DownloadAbortError>().is_some() {
            return Err(e);
        }
    }
    // 保存入库日志需要的值（因为 final_video_model 会被 .into() 消耗）
    let ingest_video_id = final_video_model.id;
    let ingest_video_name = final_video_model.name.clone();
    let ingest_upper_name = final_video_model.upper_name.clone();
    let ingest_deleted = final_video_model.deleted;
    let ingest_old_video_path = final_video_model.path.clone();
    // 从 share_copy 提取番剧系列名称（《剧名》格式）
    let ingest_series_name = final_video_model.share_copy.as_ref().and_then(|s| {
        // 匹配《》中的内容
        if let Some(start) = s.find('《') {
            if let Some(end) = s.find('》') {
                if end > start {
                    return Some(s[start + 3..end].to_string()); // UTF-8 《 is 3 bytes
                }
            }
        }
        None
    });

    let mut video_active_model: video::ActiveModel = final_video_model.into();
    video_active_model.download_status = Set(status.into());

    // 对于番剧和多P视频使用Season结构时，保存根文件夹路径而不是Season文件夹路径
    let path_to_save = if is_bangumi {
        if let Some(ref bangumi_folder_path) = bangumi_folder_path {
            bangumi_folder_path.to_string_lossy().to_string()
        } else {
            base_path.to_string_lossy().to_string()
        }
    } else {
        // 检查是否为多P视频或合集且启用了Season结构
        let config = crate::config::reload_config();
        if (!is_single_page && config.multi_page_use_season_structure && season_folder.is_some())
            || (is_collection && collection_use_season_structure && season_folder.is_some())
        {
            // 对于多P视频或合集使用Season结构时，保存根目录路径而不是Season子文件夹路径
            base_path
                .parent()
                .map(|parent| parent.to_string_lossy().to_string())
                .unwrap_or_else(|| base_path.to_string_lossy().to_string())
        } else {
            base_path.to_string_lossy().to_string()
        }
    };

    debug!("=== 路径保存 ===");
    debug!("最终保存到数据库的路径: {:?}", path_to_save);
    debug!("数据库中原始视频路径: {:?}", ingest_old_video_path);
    debug!("原始基础路径: {:?}", base_path);
    if let Some(ref bangumi_folder_path) = bangumi_folder_path {
        debug!("番剧文件夹路径: {:?}", bangumi_folder_path);
    }
    if let Some(ref season_folder) = season_folder {
        debug!("季度文件夹名: {}", season_folder);
    }
    debug!("=== 路径计算结束 ===");

    // 如果用户修改了视频源目录（或合作视频归类UP主发生变化），
    // 仅“重置视频信息/更新详情”时也可能需要把已有文件夹同步到新路径，否则会出现“合作类视频仍在旧位置”。
    // 这里做一个安全的自动迁移：仅在以下条件满足时才移动：
    // - 非番剧
    // - 非平铺目录（避免误移动整个视频源根目录）
    // - 旧路径存在且是目录
    // - 新路径不存在
    // - 旧路径 != 新路径
    if !is_bangumi && !flat_folder && !ingest_old_video_path.is_empty() && ingest_old_video_path != path_to_save {
        let old_dir = std::path::Path::new(&ingest_old_video_path);
        let new_dir = std::path::Path::new(&path_to_save);
        if old_dir.is_dir() && !new_dir.exists() {
            if let Some(parent) = new_dir.parent() {
                if let Err(e) = tokio::fs::create_dir_all(parent).await {
                    warn!(
                        "创建目标目录失败，跳过自动迁移: {:?} -> {:?}, err={}",
                        old_dir, new_dir, e
                    );
                } else {
                    match tokio::fs::rename(old_dir, new_dir).await {
                        Ok(_) => {
                            info!("检测到路径变更，已自动迁移视频文件夹: {:?} -> {:?}", old_dir, new_dir);

                            // 同步更新分页的文件路径（只替换前缀，避免路径指向旧目录导致后续操作混乱）
                            if let Ok(pages) = page::Entity::find()
                                .filter(page::Column::VideoId.eq(ingest_video_id))
                                .all(connection)
                                .await
                            {
                                for p in pages {
                                    if let Some(p_path) = &p.path {
                                        if p_path.starts_with(&ingest_old_video_path) {
                                            let new_path =
                                                format!("{}{}", path_to_save, &p_path[ingest_old_video_path.len()..]);
                                            let active = page::ActiveModel {
                                                id: sea_orm::ActiveValue::Unchanged(p.id),
                                                path: Set(Some(new_path)),
                                                ..Default::default()
                                            };
                                            if let Err(e) = active.update(connection).await {
                                                warn!("更新分页路径失败: page_id={}, err={}", p.id, e);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            warn!(
                                "自动迁移视频文件夹失败（可能跨盘/被占用），将继续使用旧位置: {:?} -> {:?}, err={}",
                                old_dir, new_dir, e
                            );
                        }
                    }
                }
            }
        }
    }

    // 只在任务完成时记录"最新入库"事件（用于首页展示）
    // get_completed() 检查最高位标记，表示所有子任务都已完成（成功或达到最大重试次数）
    if status.get_completed() {
        use crate::ingest_log::IngestStatus;
        let bits: [u32; 5] = status.into();
        let all_ok = bits.iter().all(|&x| x == crate::utils::status::STATUS_OK);
        let ingest_status = if ingest_deleted != 0 {
            IngestStatus::Deleted
        } else if all_ok {
            IngestStatus::Success
        } else {
            IngestStatus::Failed
        };
        crate::ingest_log::INGEST_LOG
            .finish_video(
                ingest_video_id,
                ingest_video_name,
                ingest_upper_name,
                path_to_save.clone(),
                ingest_status,
                ingest_series_name,
            )
            .await;
    }

    // 重新从数据库读取最新的 video.path，因为 download_page 中可能进行了 AI 重命名
    // 这样可以确保返回的 video_active_model 包含正确的路径，不会被外部的 update_videos_model 覆盖
    let final_path = match video::Entity::find_by_id(ingest_video_id).one(connection).await {
        Ok(Some(latest_video)) => {
            if !latest_video.path.is_empty() && latest_video.path != path_to_save {
                debug!(
                    "检测到 AI 重命名后的路径变化: video_id={}, 原路径='{}', 新路径='{}'",
                    ingest_video_id, path_to_save, latest_video.path
                );
                latest_video.path
            } else {
                path_to_save
            }
        }
        _ => path_to_save,
    };

    video_active_model.path = Set(final_path);
    Ok(video_active_model)
}

/// 分发并执行分页下载任务，当且仅当所有分页成功下载或达到最大重试次数时返回 Ok，否则根据失败原因返回对应的错误
pub async fn dispatch_download_page(args: DownloadPageArgs<'_>, token: CancellationToken) -> Result<ExecutionStatus> {
    if !args.should_run {
        return Ok(ExecutionStatus::Skipped);
    }

    let current_config = crate::config::reload_config();
    let child_semaphore = Arc::new(Semaphore::new(current_config.concurrent_limit.page));
    let tasks = args
        .pages
        .into_iter()
        .map(|page_model| {
            let page_pid = page_model.pid; // 保存分页ID
            let page_name = page_model.name.clone(); // 保存分页名称
            let semaphore_clone = child_semaphore.clone();
            let token_clone = token.clone();
            let bili_client = args.bili_client;
            let video_source = args.video_source;
            let video_model = args.video_model;
            let connection = args.connection;
            let downloader = args.downloader;
            let base_path = args.base_path;
            async move {
                let result = download_page(
                    bili_client,
                    video_source,
                    video_model,
                    page_model,
                    connection,
                    semaphore_clone.as_ref(),
                    downloader,
                    base_path,
                    token_clone,
                )
                .await;
                // 返回结果和分页信息
                (result, page_pid, page_name)
            }
        })
        .collect::<FuturesUnordered<_>>();
    let (mut download_aborted, mut target_status) = (false, STATUS_OK);
    let mut cancelled_by_user = false;
    let mut failed_pages: Vec<String> = Vec::new(); // 收集失败的分页信息
    let mut stream = tasks;
    while let Some((res, page_pid, page_name)) = stream.next().await {
        match res {
            Ok(model) => {
                if download_aborted {
                    continue;
                }
                // 该视频的所有分页的下载状态都会在此返回，需要根据这些状态确认视频层"分 P 下载"子任务的状态
                // 在过去的实现中，此处仅仅根据 page_download_status 的最高标志位来判断，如果最高标志位是 true 则认为完成
                // 这样会导致即使分页中有失败到 MAX_RETRY 的情况，视频层的分 P 下载状态也会被认为是 Succeeded，不够准确
                // 新版本实现会将此处取值为所有子任务状态的最小值，这样只有所有分页的子任务全部成功时才会认为视频层的分 P 下载状态是 Succeeded
                let page_download_status = model.download_status.try_as_ref().expect("download_status must be set");
                let separate_status: [u32; 5] = PageStatus::from(*page_download_status).into();
                for status in separate_status {
                    target_status = target_status.min(status);
                }
                update_pages_model(vec![model], args.connection).await?;
            }
            Err(e) => {
                let error_msg = e.to_string();
                debug!("分页下载错误原始信息 - 第{}页 {}: {}", page_pid, page_name, error_msg);

                // 1. 首先检查是否是用户暂停导致的错误
                if error_msg.contains("任务已暂停")
                    || error_msg.contains("停止下载")
                    || error_msg.contains("用户主动暂停任务")
                    || (error_msg.contains("Download cancelled") && crate::task::TASK_CONTROLLER.is_paused())
                {
                    info!(
                        "分页下载任务因用户暂停而终止 - 第{}页 {}: {}",
                        page_pid, page_name, error_msg
                    );
                    cancelled_by_user = true;
                    continue; // 跳过暂停相关的错误，不触发风控或其他处理
                }

                // 2. 检查是否是真正的风控错误（DownloadAbortError）
                if e.downcast_ref::<DownloadAbortError>().is_some() {
                    warn!(
                        "检测到真正的风控错误，中止所有下载任务 - 第{}页 {}",
                        page_pid, page_name
                    );
                    if !download_aborted {
                        token.cancel();
                        download_aborted = true;
                    }
                    continue;
                }

                // 充电视频在获取详情时已经被upower字段检测并处理，这里不应该再出现充电视频错误

                // 4. 处理其他类型的错误（包括普通的Download cancelled）
                // 记录更详细的错误信息，包括错误链
                error!("下载分页子任务失败 - 第{}页 {}: {:#}", page_pid, page_name, e);

                // 输出错误链中的所有错误信息
                let mut error_chain = String::new();
                let mut current_error: &dyn std::error::Error = &*e;
                error_chain.push_str(&format!("错误: {}", current_error));

                while let Some(source) = current_error.source() {
                    error_chain.push_str(&format!("\n  原因: {}", source));
                    current_error = source;
                }

                error!("完整错误链: {}", error_chain);

                // 收集失败信息，包含分页标识
                failed_pages.push(format!("第{}页 {}: {}", page_pid, page_name, e));

                // 如果失败的任务没有达到 STATUS_OK，记录当前状态
                if target_status != STATUS_OK {
                    error!("当前分页下载状态: {}, 视频: {}", target_status, &args.video_model.name);
                }
            }
        }
    }

    if download_aborted {
        error!(
            "下载视频「{}」的分页时触发风控，将异常向上传递..",
            &args.video_model.name
        );
        bail!(DownloadAbortError());
    }
    if cancelled_by_user {
        info!("分页下载因用户暂停而终止，跳过失败汇总: {}", &args.video_model.name);
        return Ok(ExecutionStatus::Cancelled);
    }
    if crate::task::TASK_CONTROLLER.is_paused() {
        info!("任务已暂停，跳过分页失败汇总: {}", &args.video_model.name);
        return Ok(ExecutionStatus::Cancelled);
    }
    if target_status != STATUS_OK {
        // 充电视频在获取详情时已经被upower字段检测并处理，这里不需要特殊的充电视频逻辑

        // 提供更详细的错误信息，保留原始错误上下文
        error!(
            "视频「{}」分页下载失败，状态码: {}",
            &args.video_model.name, target_status
        );

        // 构建详细的错误信息
        let details = if !failed_pages.is_empty() {
            format!("失败的分页: {}", failed_pages.join("; "))
        } else {
            "请检查网络连接、文件系统权限或重试下载。".to_string()
        };

        // 发送错误通知（异步执行，不阻塞主流程）
        let video_name = args.video_model.name.clone();
        let bvid = args.video_model.bvid.clone();
        let details_clone = details.clone();
        tokio::spawn(async move {
            use crate::utils::notification::send_error_notification;
            if let Err(e) = send_error_notification(
                "下载失败",
                &format!("视频「{}」分页下载失败，状态码: {}", video_name, target_status),
                Some(&format!("BVID: {}\n{}", bvid, details_clone)),
            )
            .await
            {
                tracing::warn!("发送下载失败通知失败: {}", e);
            }
        });

        // 返回ProcessPageError，携带详细信息
        let process_error = ProcessPageError::new(args.video_model.name.clone(), target_status).with_details(details);
        return Err(process_error.into());
    }
    Ok(ExecutionStatus::Succeeded)
}

/// 下载某个分页，未发生风控且正常运行时返回 Ok(Page::ActiveModel)，其中 status 字段存储了新的下载状态，发生风控时返回 DownloadAbortError
#[allow(clippy::too_many_arguments)]
pub async fn download_page(
    bili_client: &BiliClient,
    video_source: &VideoSourceEnum,
    video_model: &video::Model,
    mut page_model: page::Model,
    connection: &DatabaseConnection,
    semaphore: &Semaphore,
    downloader: &UnifiedDownloader,
    base_path: &Path,
    token: CancellationToken,
) -> Result<page::ActiveModel> {
    let _permit = tokio::select! {
        biased;
        _ = token.cancelled() => return Err(anyhow!("Download cancelled")),
        permit = semaphore.acquire() => permit.context("acquire semaphore failed")?,
    };
    let mut status = PageStatus::from(page_model.download_status);
    let mut separate_status = status.should_run();
    let is_single_page = video_model.single_page.context("single_page is null")?;

    // 根据视频源设置调整弹幕和字幕下载开关
    // separate_status[3] = 弹幕, separate_status[4] = 字幕
    if !video_source.download_danmaku() {
        separate_status[3] = false;
    }
    if !video_source.download_subtitle() {
        separate_status[4] = false;
    }

    // 获取是否仅下载音频的设置
    let audio_only = video_source.audio_only();

    // 仅音频模式下，如果启用了audio_only_m4a_only，跳过所有sidecar文件
    // separate_status[0] = 封面, [2] = NFO, [3] = 弹幕, [4] = 字幕
    if audio_only && video_source.audio_only_m4a_only() {
        separate_status[0] = false; // 跳过封面
        separate_status[2] = false; // 跳过NFO
        separate_status[3] = false; // 跳过弹幕
        separate_status[4] = false; // 跳过字幕
    }

    // 检查是否为番剧
    let is_bangumi = match video_model.source_type {
        Some(1) => true, // source_type = 1 表示为番剧
        _ => false,
    };
    let is_submission_collection_video = is_submission_ugc_collection_video(video_source, video_model);
    if matches!(video_source, VideoSourceEnum::Submission(_)) {
        debug!(
            "分页下载投稿合集判定: bvid={}, is_submission_collection_video={}, source_submission_id={:?}, season_id={:?}",
            video_model.bvid,
            is_submission_collection_video,
            video_model.source_submission_id,
            video_model.season_id
        );
    }
    let collection_season_number =
        if matches!(video_source, VideoSourceEnum::Collection(_)) || is_submission_collection_video {
            extract_season_number_from_path(&base_path.to_string_lossy())
                .unwrap_or(1)
                .max(1)
        } else {
            1
        };

    // 根据视频源类型选择不同的模板渲染方式
    let base_name = if let VideoSourceEnum::Collection(collection_source) = video_source {
        // 合集视频的特殊处理
        let config = crate::config::reload_config();
        let legacy_fixed_season_template = config.collection_folder_mode.as_ref() == "up_seasonal"
            && config.collection_unified_name.as_ref().contains("S01E{{episode_pad}}");
        if matches!(config.collection_folder_mode.as_ref(), "unified" | "up_seasonal") {
            // 统一模式：使用可配置的合集统一命名模板（默认保持 S01E..）
            match get_collection_video_episode_number(connection, collection_source.id, &video_model.bvid).await {
                Ok(episode_number) => {
                    let args = collection_unified_page_format_args(
                        video_model,
                        &page_model,
                        episode_number,
                        collection_season_number,
                    );
                    match crate::config::with_config(|bundle| bundle.render_collection_unified_template(&args)) {
                        Ok(rendered) => {
                            if legacy_fixed_season_template {
                                if let Some(suffix) = rendered.strip_prefix("S01E") {
                                    format!("S{:02}E{}", collection_season_number, suffix)
                                } else {
                                    rendered
                                }
                            } else {
                                rendered
                            }
                        }
                        Err(e) => {
                            warn!("合集统一模式命名模板渲染失败，将回退到默认命名: {}", e);
                            let clean_name = crate::utils::filenamify::filenamify(&video_model.name);
                            let is_single_page = video_model.single_page.unwrap_or(true);
                            if !is_single_page {
                                format!(
                                    "S{:02}E{:02}P{:02} - {}",
                                    collection_season_number, episode_number, page_model.pid, clean_name
                                )
                            } else {
                                format!(
                                    "S{:02}E{:02} - {}",
                                    collection_season_number, episode_number, clean_name
                                )
                            }
                        }
                    }
                }
                Err(_) => {
                    // 如果获取序号失败，使用默认命名
                    crate::config::with_config(|bundle| {
                        bundle.render_page_template(&page_format_args(video_model, &page_model))
                    })
                    .map_err(|e| anyhow::anyhow!("模板渲染失败: {}", e))?
                }
            }
        } else {
            // 分离模式：检查是否为多P视频
            let is_single_page = video_model.single_page.unwrap_or(true);
            if !is_single_page {
                // 多P视频：使用multi_page_name模板
                let page_args = page_format_args(video_model, &page_model);
                match crate::config::with_config(|bundle| bundle.render_multi_page_template(&page_args)) {
                    Ok(rendered) => rendered,
                    Err(_) => {
                        // 如果渲染失败，使用默认格式
                        let season_number = 1;
                        let episode_number = page_model.pid;
                        format!("S{:02}E{:02}-{:02}", season_number, episode_number, episode_number)
                    }
                }
            } else {
                // 单P视频：使用page_name模板
                crate::config::with_config(|bundle| {
                    bundle.render_page_template(&page_format_args(video_model, &page_model))
                })
                .map_err(|e| anyhow::anyhow!("模板渲染失败: {}", e))?
            }
        }
    } else if is_submission_collection_video {
        let config = crate::config::reload_config();
        let legacy_fixed_season_template = config.collection_folder_mode.as_ref() == "up_seasonal"
            && config.collection_unified_name.as_ref().contains("S01E{{episode_pad}}");

        if matches!(config.collection_folder_mode.as_ref(), "unified" | "up_seasonal") {
            // 投稿源中的UGC合集视频也复用合集统一命名模板
            let episode_number = video_model.episode_number.unwrap_or(page_model.pid.max(1)).max(1);
            let args =
                collection_unified_page_format_args(video_model, &page_model, episode_number, collection_season_number);
            match crate::config::with_config(|bundle| bundle.render_collection_unified_template(&args)) {
                Ok(rendered) => {
                    if legacy_fixed_season_template {
                        if let Some(suffix) = rendered.strip_prefix("S01E") {
                            format!("S{:02}E{}", collection_season_number, suffix)
                        } else {
                            rendered
                        }
                    } else {
                        rendered
                    }
                }
                Err(e) => {
                    warn!("投稿UGC合集统一命名模板渲染失败，将回退到默认命名: {}", e);
                    let clean_name = crate::utils::filenamify::filenamify(&video_model.name);
                    let is_single_page = video_model.single_page.unwrap_or(true);
                    if !is_single_page {
                        format!(
                            "S{:02}E{:02}P{:02} - {}",
                            collection_season_number, episode_number, page_model.pid, clean_name
                        )
                    } else {
                        format!(
                            "S{:02}E{:02} - {}",
                            collection_season_number, episode_number, clean_name
                        )
                    }
                }
            }
        } else if !is_single_page {
            let page_args = page_format_args(video_model, &page_model);
            match crate::config::with_config(|bundle| bundle.render_multi_page_template(&page_args)) {
                Ok(rendered) => rendered,
                Err(_) => {
                    let season_number = 1;
                    let episode_number = page_model.pid;
                    format!("S{:02}E{:02}-{:02}", season_number, episode_number, episode_number)
                }
            }
        } else {
            crate::config::with_config(|bundle| {
                bundle.render_page_template(&page_format_args(video_model, &page_model))
            })
            .map_err(|e| anyhow::anyhow!("模板渲染失败: {}", e))?
        }
    } else if is_bangumi {
        // 番剧使用专用的模板方法
        if let VideoSourceEnum::BangumiSource(bangumi_source) = video_source {
            // 获取API标题（如果有season_id）
            let api_title = if let Some(ref season_id) = video_model.season_id {
                get_cached_season_title(bili_client, season_id, token.clone()).await
            } else {
                None
            };

            bangumi_source
                .render_page_name(video_model, &page_model, connection, api_title.as_deref())
                .await?
        } else {
            // 如果类型不匹配，使用最新配置手动渲染
            crate::config::with_config(|bundle| {
                bundle.render_page_template(&page_format_args(video_model, &page_model))
            })
            .map_err(|e| anyhow::anyhow!("模板渲染失败: {}", e))?
        }
    } else if !is_single_page {
        // 对于多P视频（非番剧），使用最新配置中的multi_page_name模板
        let page_args = page_format_args(video_model, &page_model);
        match crate::config::with_config(|bundle| bundle.render_multi_page_template(&page_args)) {
            Ok(rendered) => rendered,
            Err(_) => {
                // 如果渲染失败，使用默认格式
                let season_number = 1;
                let episode_number = page_model.pid;
                format!("S{:02}E{:02}-{:02}", season_number, episode_number, episode_number)
            }
        }
    } else {
        // 单P视频使用最新配置的page_name模板
        crate::config::with_config(|bundle| bundle.render_page_template(&page_format_args(video_model, &page_model)))
            .map_err(|e| anyhow::anyhow!("模板渲染失败: {}", e))?
    };

    // 根据audio_only设置选择文件扩展名
    let media_ext = if audio_only { "m4a" } else { "mp4" };

    let (poster_path, video_path, nfo_path, danmaku_path, fanart_path, subtitle_path) = if is_single_page {
        (
            base_path.join(format!("{}-thumb.jpg", &base_name)),
            base_path.join(format!("{}.{}", &base_name, media_ext)),
            base_path.join(format!("{}.nfo", &base_name)),
            base_path.join(format!("{}.zh-CN.default.ass", &base_name)),
            Some(base_path.join(format!("{}-fanart.jpg", &base_name))),
            base_path.join(format!("{}.srt", &base_name)),
        )
    } else if is_bangumi {
        // 番剧直接使用基础路径，不创建子文件夹结构
        (
            base_path.join(format!("{}-thumb.jpg", &base_name)),
            base_path.join(format!("{}.{}", &base_name, media_ext)),
            base_path.join(format!("{}.nfo", &base_name)),
            base_path.join(format!("{}.zh-CN.default.ass", &base_name)),
            None,
            base_path.join(format!("{}.srt", &base_name)),
        )
    } else {
        // 非番剧的多P视频直接使用基础路径，不创建子文件夹
        (
            base_path.join(format!("{}-thumb.jpg", &base_name)),
            base_path.join(format!("{}.{}", &base_name, media_ext)),
            base_path.join(format!("{}.nfo", &base_name)),
            base_path.join(format!("{}.zh-CN.default.ass", &base_name)),
            // 多P视频的每个分页都应该有自己的fanart
            Some(base_path.join(format!("{}-fanart.jpg", &base_name))),
            base_path.join(format!("{}.srt", &base_name)),
        )
    };
    let dimension = match (page_model.width, page_model.height) {
        (Some(width), Some(height)) => Some(Dimension {
            width,
            height,
            rotate: 0,
        }),
        _ => None,
    };

    // 某些异常情况下（如番剧季信息未拉到该EP的CID），page.cid 可能会被写入占位值（<=0），导致后续弹幕/字幕/视频流请求失败。
    // 这里做一次“就地自愈”：优先用 video.cid 修复（番剧单集/单P视频应一致），必要时再从API补全。
    let needs_valid_cid = separate_status[1] || separate_status[3] || separate_status[4];
    if needs_valid_cid && page_model.cid <= 0 {
        let mut repaired = false;

        if let Some(video_cid) = video_model.cid.filter(|cid| *cid > 0) {
            info!(
                "检测到分页CID无效，使用视频缓存CID修复: 视频「{}」 page_id={} cid={} -> {}",
                &video_model.name, page_model.id, page_model.cid, video_cid
            );
            page_model.cid = video_cid;
            let update_page = page::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(page_model.id),
                cid: Set(video_cid),
                ..Default::default()
            };
            update_page.update(connection).await?;
            repaired = true;
        } else if is_bangumi {
            if let Some(ep_id) = video_model.ep_id.as_deref() {
                if let Some((cid, duration)) = get_bangumi_info_from_api(bili_client, ep_id, token.clone()).await {
                    if cid > 0 {
                        info!(
                            "检测到番剧分页CID无效，已从API重新获取: 视频「{}」 EP{} -> CID={}, Duration={}s",
                            &video_model.name, ep_id, cid, duration
                        );
                        page_model.cid = cid;
                        if duration > 0 {
                            page_model.duration = duration;
                        }
                        let update_page = page::ActiveModel {
                            id: sea_orm::ActiveValue::Unchanged(page_model.id),
                            cid: Set(page_model.cid),
                            duration: Set(page_model.duration),
                            ..Default::default()
                        };
                        update_page.update(connection).await?;
                        repaired = true;
                    }
                }
            }
        }

        if !repaired {
            warn!(
                "分页CID无效且无法自动修复，后续依赖CID的任务可能失败: 视频「{}」 page_id={} pid={} cid={}",
                &video_model.name, page_model.id, page_model.pid, page_model.cid
            );
        }
    }
    let page_info = PageInfo {
        cid: page_model.cid,
        duration: page_model.duration,
        dimension,
        ..Default::default()
    };
    // 使用 tokio::join! 替代装箱的 Future，零分配并行执行
    let (res_1, res_2, res_3, res_4, res_5) = tokio::join!(
        fetch_page_poster(
            separate_status[0],
            video_model,
            &page_model,
            downloader,
            poster_path,
            fanart_path,
            token.clone(),
        ),
        fetch_page_video(
            separate_status[1],
            bili_client,
            video_model,
            downloader,
            &page_info,
            &video_path,
            audio_only,
            token.clone(),
        ),
        generate_page_nfo(
            separate_status[2],
            video_model,
            &page_model,
            nfo_path,
            connection,
            if matches!(video_source, VideoSourceEnum::Collection(_)) {
                Some(collection_season_number)
            } else {
                None
            },
        ),
        fetch_page_danmaku(
            separate_status[3],
            bili_client,
            video_model,
            &page_info,
            danmaku_path,
            token.clone(),
        ),
        fetch_page_subtitle(
            separate_status[4],
            bili_client,
            video_model,
            &page_info,
            &subtitle_path,
            token.clone(),
        )
    );

    let results = [res_1, res_2, res_3, res_4, res_5]
        .into_iter()
        .map(Into::into)
        .collect::<Vec<_>>();
    status.update_status(&results);

    // 充电视频在获取详情时已经被upower字段检测并处理，无需分页级别的后期检测

    results
        .iter()
        .zip(["封面", "视频", "详情", "弹幕", "字幕"])
        .for_each(|(res, task_name)| match res {
            ExecutionStatus::Skipped => debug!(
                "处理视频「{}」第 {} 页{}已成功过，跳过",
                &video_model.name, page_model.pid, task_name
            ),
            ExecutionStatus::Succeeded => debug!(
                "处理视频「{}」第 {} 页{}成功",
                &video_model.name, page_model.pid, task_name
            ),
            ExecutionStatus::Cancelled => info!(
                "处理视频「{}」第 {} 页{}因用户暂停而终止",
                &video_model.name, page_model.pid, task_name
            ),
            ExecutionStatus::Ignored(e) => {
                let error_msg = e.to_string();
                if !error_msg.contains("status code: 87007") {
                    info!(
                        "处理视频「{}」第 {} 页{}出现常见错误，已忽略: {:#}",
                        &video_model.name, page_model.pid, task_name, e
                    );
                }
            }
            ExecutionStatus::ClassifiedFailed(classified_error) => {
                // 根据错误分类进行不同级别的日志记录
                match classified_error.error_type {
                    crate::error::ErrorType::NotFound => {
                        debug!(
                            "处理视频「{}」第 {} 页{}失败({}): {}",
                            &video_model.name,
                            page_model.pid,
                            task_name,
                            classified_error.error_type,
                            classified_error.message
                        );
                    }
                    crate::error::ErrorType::Permission => {
                        // 权限错误（充电专享视频现在在获取详情时处理）
                        info!(
                            "跳过视频「{}」第 {} 页{}: {}",
                            &video_model.name, page_model.pid, task_name, classified_error.message
                        );
                    }
                    crate::error::ErrorType::Network
                    | crate::error::ErrorType::Timeout
                    | crate::error::ErrorType::RateLimit => {
                        warn!(
                            "处理视频「{}」第 {} 页{}失败({}): {}{}",
                            &video_model.name,
                            page_model.pid,
                            task_name,
                            classified_error.error_type,
                            classified_error.message,
                            if classified_error.should_retry {
                                " (可重试)"
                            } else {
                                ""
                            }
                        );
                    }
                    crate::error::ErrorType::RiskControl => {
                        error!(
                            "处理视频「{}」第 {} 页{}触发风控: {}",
                            &video_model.name, page_model.pid, task_name, classified_error.message
                        );
                    }
                    crate::error::ErrorType::UserCancelled => {
                        info!(
                            "处理视频「{}」第 {} 页{}因用户暂停而终止",
                            &video_model.name, page_model.pid, task_name
                        );
                    }
                    _ => {
                        error!(
                            "处理视频「{}」第 {} 页{}失败({}): {}",
                            &video_model.name,
                            page_model.pid,
                            task_name,
                            classified_error.error_type,
                            classified_error.message
                        );
                    }
                }
            }
            ExecutionStatus::Failed(e) | ExecutionStatus::FixedFailed(_, e) => {
                // 使用错误分类器进行统一处理
                #[allow(clippy::needless_borrow)]
                let classified_error = crate::error::ErrorClassifier::classify_error(&e);
                match classified_error.error_type {
                    crate::error::ErrorType::NotFound => {
                        debug!(
                            "处理视频「{}」第 {} 页{}失败(404): {:#}",
                            &video_model.name, page_model.pid, task_name, e
                        );
                    }
                    crate::error::ErrorType::UserCancelled => {
                        info!(
                            "处理视频「{}」第 {} 页{}因用户暂停而终止",
                            &video_model.name, page_model.pid, task_name
                        );
                    }
                    _ => {
                        error!(
                            "处理视频「{}」第 {} 页{}失败: {:#}",
                            &video_model.name, page_model.pid, task_name, e
                        );
                    }
                }
            }
        });
    // 检查下载视频时是否触发风控
    match results.into_iter().nth(1).context("video download result not found")? {
        ExecutionStatus::Failed(e) => {
            if let Ok(BiliError::RiskControlOccurred) = e.downcast::<BiliError>() {
                bail!(DownloadAbortError());
            }
        }
        ExecutionStatus::ClassifiedFailed(ref classified_error) => {
            if classified_error.error_type == crate::error::ErrorType::RiskControl {
                bail!(DownloadAbortError());
            }
        }
        _ => {}
    }

    // AI 自动重命名（仅非番剧 + 单源开关 + 全局开关）
    // 检查 video_path 是否存在，如果不存在可能是同视频的其他分P已经重命名了文件夹
    let video_path = if !video_path.exists() {
        // 查询数据库中同一个 video 的其他已完成 page 的路径
        use bili_sync_entity::page;
        let other_pages = page::Entity::find()
            .filter(page::Column::VideoId.eq(video_model.id))
            .filter(page::Column::Path.is_not_null())
            .filter(page::Column::Id.ne(page_model.id))
            .all(connection)
            .await
            .unwrap_or_default();

        // 尝试从其他 page 的路径推断新的文件夹
        let mut new_video_path = video_path.clone();
        for other_page in other_pages {
            if let Some(other_path_str) = &other_page.path {
                let other_path = std::path::Path::new(other_path_str);
                if let Some(parent) = other_path.parent() {
                    if parent.exists() {
                        // 找到了有效的新文件夹，用它重新构建 video_path
                        let file_name = video_path.file_name().unwrap_or_default();
                        new_video_path = parent.join(file_name);
                        info!(
                            "检测到文件夹已被重命名，使用新路径: {} -> {}",
                            video_path.display(),
                            new_video_path.display()
                        );
                        break;
                    }
                }
            }
        }
        new_video_path
    } else {
        video_path.clone()
    };

    // AI 重命名已移至视频源下载完成后批量执行（batch_ai_rename_for_source）
    // 此处仅保存原始文件路径，批量重命名时会更新
    let final_video_path = video_path.clone();

    let mut page_active_model: page::ActiveModel = page_model.into();
    page_active_model.download_status = Set(status.into());
    page_active_model.path = Set(Some(final_video_path.to_string_lossy().to_string()));
    Ok(page_active_model)
}

pub async fn fetch_page_poster(
    should_run: bool,
    video_model: &video::Model,
    page_model: &page::Model,
    downloader: &UnifiedDownloader,
    poster_path: PathBuf,
    fanart_path: Option<PathBuf>,
    token: CancellationToken,
) -> Result<ExecutionStatus> {
    // 兜底修复：旧版本可能只生成了 *-thumb.jpg（或用户曾中断/清理文件），
    // 但状态位仍显示封面已完成，导致 fanart 缺失也不会补全。
    // 这里在“无需下载”时也做一次本地自愈：如果 thumb 存在但 fanart 不存在，则复制生成 fanart。
    if !should_run {
        if let Some(fanart_path) = fanart_path {
            if !fanart_path.exists() && poster_path.exists() {
                ensure_parent_dir_for_file(&fanart_path).await?;
                fs::copy(&poster_path, &fanart_path).await?;
                return Ok(ExecutionStatus::Succeeded);
            }
        }
        return Ok(ExecutionStatus::Skipped);
    }
    let single_page = video_model.single_page.context("single_page is null")?;
    let url = if single_page {
        // 单页视频直接用视频的封面
        video_model.cover.as_str()
    } else {
        // 多页视频，如果单页没有封面，就使用视频的封面
        match &page_model.image {
            Some(url) => url.as_str(),
            None => video_model.cover.as_str(),
        }
    };
    let urls = vec![url];
    tokio::select! {
        biased;
        _ = token.cancelled() => return Ok(ExecutionStatus::Cancelled),
        res = downloader.fetch_with_fallback(&urls, &poster_path) => res,
    }?;
    if let Some(fanart_path) = fanart_path {
        ensure_parent_dir_for_file(&fanart_path).await?;
        fs::copy(&poster_path, &fanart_path).await?;
    }
    Ok(ExecutionStatus::Succeeded)
}

/// 下载单个流文件并返回文件大小（使用UnifiedDownloader智能选择下载方式）
///
/// 同时会把本次下载的 bytes 与耗时写入内存「入库事件」统计，用于首页展示平均下载速度。
async fn download_stream(downloader: &UnifiedDownloader, video_id: i32, urls: &[&str], path: &Path) -> Result<u64> {
    // 直接使用UnifiedDownloader，它会智能选择aria2或原生下载器
    // aria2本身就支持多线程，原生下载器作为备选方案使用单线程
    let start = std::time::Instant::now();
    let download_result = downloader.fetch_with_fallback(urls, path).await;

    match download_result {
        Ok(_) => {
            // 获取文件大小
            let size = tokio::fs::metadata(path)
                .await
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            let elapsed = start.elapsed();
            crate::ingest_log::INGEST_LOG
                .add_download_sample(video_id, size, elapsed)
                .await;
            Ok(size)
        }
        Err(e) => {
            let error_msg = e.to_string();
            // 检查是否为暂停相关错误
            if error_msg.contains("用户主动暂停任务") || error_msg.contains("任务已暂停") {
                info!("下载因用户暂停而终止");
            } else if let Some(BiliError::RequestFailed(-404, _)) = e.downcast_ref::<BiliError>() {
                // 对于404错误，降级为debug日志
                debug!("下载失败(404): {:#}", e);
            } else {
                // 使用错误分类器进行统一处理
                #[allow(clippy::needless_borrow)]
                let classified_error = crate::error::ErrorClassifier::classify_error(&e);
                match classified_error.error_type {
                    crate::error::ErrorType::UserCancelled => {
                        info!("下载因用户暂停而终止: {:#}", e);
                    }
                    _ => {
                        error!("下载失败: {:#}", e);
                    }
                }
            }
            Err(e)
        }
    }
}

pub async fn fetch_page_video(
    should_run: bool,
    bili_client: &BiliClient,
    video_model: &video::Model,
    downloader: &UnifiedDownloader,
    page_info: &PageInfo,
    page_path: &Path,
    audio_only: bool,
    token: CancellationToken,
) -> Result<ExecutionStatus> {
    if !should_run {
        return Ok(ExecutionStatus::Skipped);
    }

    let bili_video = Video::new(bili_client, video_model.bvid.clone());

    // 获取用户配置的筛选选项（用于按画质范围请求播放地址，避免拿到高画质单流后被过滤导致无视频流）
    let config = crate::config::reload_config();
    let filter_option = &config.filter_option;
    let max_qn = filter_option.video_max_quality as u32;
    let min_qn = filter_option.video_min_quality as u32;

    // 获取视频流信息 - 使用带API降级机制的调用
    let mut streams = tokio::select! {
        biased;
        _ = token.cancelled() => return Err(anyhow!("Download cancelled")),
        res = async {
            // 检查是否为番剧视频
            if video_model.source_type == Some(1) && video_model.ep_id.is_some() {
                // 番剧视频使用番剧专用API的回退机制
                let ep_id = video_model.ep_id.as_ref().unwrap();
                debug!("使用带质量回退的番剧API获取播放地址: ep_id={}", ep_id);
                bili_video
                    .get_bangumi_page_analyzer_with_fallback_in_range(page_info, ep_id, max_qn, min_qn)
                    .await
            } else {
                // 普通视频使用API降级机制（普通视频API -> 番剧API）
                debug!("使用API降级机制获取播放地址（普通视频API -> 番剧API）");
                // 传递ep_id以便在需要时降级到番剧API，如果没有ep_id则会自动从视频详情API获取
                let ep_id = video_model.ep_id.as_deref();
                if ep_id.is_some() {
                    debug!("视频已有ep_id: {:?}，可直接用于API降级", ep_id);
                } else {
                    debug!("视频缺少ep_id，如遇-404错误将尝试从视频详情API获取epid");
                }
                bili_video
                    .get_page_analyzer_with_api_fallback_in_range(page_info, ep_id, max_qn, min_qn)
                    .await
            }
        } => res
    }?;

    // 按需创建保存目录（只在实际下载时创建）
    ensure_parent_dir_for_file(page_path).await?;

    // UnifiedDownloader会自动选择最佳下载方式

    // 简化的配置调试日志
    debug!("=== 视频下载配置 ===");
    debug!("视频: {} ({})", video_model.name, video_model.bvid);
    debug!("分页: {} (cid: {})", page_info.name, page_info.cid);
    debug!(
        "质量配置: {} - {} (最高-最低)",
        format!(
            "{:?}({})",
            filter_option.video_max_quality, filter_option.video_max_quality as u32
        ),
        format!(
            "{:?}({})",
            filter_option.video_min_quality, filter_option.video_min_quality as u32
        )
    );
    debug!(
        "音频配置: {} - {} (最高-最低)",
        format!(
            "{:?}({})",
            filter_option.audio_max_quality, filter_option.audio_max_quality as u32
        ),
        format!(
            "{:?}({})",
            filter_option.audio_min_quality, filter_option.audio_min_quality as u32
        )
    );
    debug!("编码偏好: {:?}", filter_option.codecs);

    // 会员状态检查
    let credential = config.credential.load();
    match credential.as_deref() {
        Some(cred) => {
            debug!("用户认证: 已登录 (DedeUserID: {})", cred.dedeuserid);
        }
        None => {
            debug!("用户认证: 未登录 - 高质量视频流可能不可用");
        }
    }

    // 高质量需求提醒
    if filter_option.video_max_quality as u32 >= 120 {
        // 4K及以上
        debug!("⚠️  请求高质量视频(4K+)，需要大会员权限");
    }

    debug!("=== 配置调试结束 ===");

    // 记录开始时间
    let start_time = std::time::Instant::now();

    // 根据流类型进行不同处理
    let best_stream_result = streams.best_stream(filter_option)?;

    // 添加流选择结果日志和质量分析
    debug!("=== 流选择结果 ===");
    match &best_stream_result {
        BestStream::Mixed(stream) => {
            debug!("选择了混合流: {:?}", stream);
        }
        BestStream::VideoAudio { video, audio } => {
            if let VideoStream::DashVideo { quality, codecs, .. } = video {
                let quality_value = *quality as u32;
                let requested_quality = filter_option.video_max_quality as u32;

                debug!("✓ 选择视频流: {} {:?}", quality, codecs);

                // 质量对比分析
                if quality_value < requested_quality {
                    let quality_gap = requested_quality - quality_value;
                    if requested_quality >= 120 && quality_value < 120 {
                        debug!(
                            "⚠️  未获得4K+质量(请求{}，实际{})",
                            filter_option.video_max_quality, quality
                        );
                    } else if quality_gap >= 40 {
                        warn!(
                            "⚠️  视频质量显著低于预期(请求{}，实际{}) - 视频源可能不支持更高质量",
                            filter_option.video_max_quality, quality
                        );
                    } else {
                        info!(
                            "ℹ️  视频质量略低于预期(请求{}，实际{}) - 已选择可用的最高质量",
                            filter_option.video_max_quality, quality
                        );
                    }
                } else {
                    debug!("✓ 获得预期质量或更高");
                }
            }
            if let Some(VideoStream::DashAudio { quality, .. }) = audio {
                debug!("✓ 选择音频流: {:?}({})", quality, *quality as u32);
            } else {
                debug!("ℹ️  无独立音频流(可能为混合流)");
            }
        }
    }
    debug!("=== 流选择结束 ===");

    // 音频模式：只下载音频流
    let total_bytes = if audio_only {
        debug!("音频模式：仅下载音频流，输出 M4A 格式");
        match best_stream_result {
            BestStream::Mixed(mix_stream) => {
                // 混合流无法提取纯音频，警告并使用混合流
                warn!("混合流不支持纯音频提取，将下载完整内容");
                let urls = mix_stream.urls();
                download_stream(downloader, video_model.id, &urls, page_path).await?
            }
            BestStream::VideoAudio {
                audio: Some(audio_stream),
                ..
            } => {
                // 直接下载音频流
                let audio_urls = audio_stream.urls();
                download_stream(downloader, video_model.id, &audio_urls, page_path).await?
            }
            BestStream::VideoAudio {
                audio: None,
                video: video_stream,
            } => {
                // 没有独立音频流，警告并使用视频流（可能包含音频）
                warn!("未找到独立音频流，将下载视频流");
                let urls = video_stream.urls();
                download_stream(downloader, video_model.id, &urls, page_path).await?
            }
        }
    } else {
        // 正常模式：下载视频+音频
        match best_stream_result {
            BestStream::Mixed(mix_stream) => {
                match mix_stream {
                    // 老视频可能只返回 FLV 混合流；直接保存为 .mp4 会导致网页端无法播放
                    crate::bilibili::Stream::Flv(_) => {
                        let tmp_mix_path = page_path.with_extension("tmp_flv");
                        let urls = mix_stream.urls();
                        let downloaded_size = download_stream(downloader, video_model.id, &urls, &tmp_mix_path).await?;

                        match crate::downloader::remux_with_ffmpeg(&tmp_mix_path, page_path).await {
                            Ok(()) => {
                                let _ = fs::remove_file(&tmp_mix_path).await;
                                tokio::fs::metadata(page_path)
                                    .await
                                    .map(|metadata| metadata.len())
                                    .unwrap_or(downloaded_size)
                            }
                            Err(e) => {
                                warn!(
                                    "FLV 转封装失败，将保留原始文件（网页端可能无法播放，请检查 ffmpeg 是否可用）: {:#}",
                                    e
                                );
                                let _ = fs::remove_file(page_path).await;
                                fs::rename(&tmp_mix_path, page_path).await?;
                                downloaded_size
                            }
                        }
                    }
                    _ => {
                        let urls = mix_stream.urls();
                        download_stream(downloader, video_model.id, &urls, page_path).await?
                    }
                }
            }
            BestStream::VideoAudio {
                video: video_stream,
                audio: None,
            } => {
                let urls = video_stream.urls();
                download_stream(downloader, video_model.id, &urls, page_path).await?
            }
            BestStream::VideoAudio {
                video: video_stream,
                audio: Some(audio_stream),
            } => {
                let (tmp_video_path, tmp_audio_path) = (
                    page_path.with_extension("tmp_video"),
                    page_path.with_extension("tmp_audio"),
                );

                let video_urls = video_stream.urls();
                let video_size = download_stream(downloader, video_model.id, &video_urls, &tmp_video_path)
                    .await
                    .map_err(|e| {
                        // 使用错误分类器进行统一处理
                        let classified_error = crate::error::ErrorClassifier::classify_error(&e);
                        match classified_error.error_type {
                            crate::error::ErrorType::UserCancelled => {
                                info!("视频流下载因用户暂停而终止");
                            }
                            _ => {
                                error!("视频流下载失败: {:#}", e);
                            }
                        }
                        e
                    })?;

                let audio_urls = audio_stream.urls();
                let audio_size = download_stream(downloader, video_model.id, &audio_urls, &tmp_audio_path)
                    .await
                    .map_err(|e| {
                        // 使用错误分类器进行统一处理
                        let classified_error = crate::error::ErrorClassifier::classify_error(&e);
                        match classified_error.error_type {
                            crate::error::ErrorType::UserCancelled => {
                                info!("音频流下载因用户暂停而终止");
                            }
                            _ => {
                                error!("音频流下载失败: {:#}", e);
                            }
                        }
                        // 异步删除临时视频文件
                        let video_path_clone = tmp_video_path.clone();
                        tokio::spawn(async move {
                            let _ = fs::remove_file(&video_path_clone).await;
                        });
                        e
                    })?;

                // 增强的音视频合并，带损坏文件检测和重试机制
                let res = downloader.merge(&tmp_video_path, &tmp_audio_path, page_path).await;

                // 合并失败时的智能处理
                if let Err(e) = res {
                    error!("音视频合并失败: {:#}", e);

                    // 检查是否是文件损坏导致的失败
                    let error_msg = e.to_string();
                    if error_msg.contains("Invalid data found when processing input")
                        || error_msg.contains("ffmpeg error")
                        || error_msg.contains("文件损坏")
                    {
                        warn!("检测到文件损坏，清理临时文件并标记为重试: {}", error_msg);

                        // 立即清理损坏的临时文件
                        let _ = fs::remove_file(&tmp_video_path).await;
                        let _ = fs::remove_file(&tmp_audio_path).await;

                        // 返回特殊错误，让上层重试下载
                        return Err(anyhow::anyhow!(
                            "视频文件损坏，已清理临时文件，请重试下载: {}",
                            error_msg
                        ));
                    } else {
                        // 其他类型的合并错误，清理临时文件后直接返回
                        let _ = fs::remove_file(&tmp_video_path).await;
                        let _ = fs::remove_file(&tmp_audio_path).await;
                        return Err(e);
                    }
                }

                // 合并成功，清理临时文件
                let _ = fs::remove_file(tmp_video_path).await;
                let _ = fs::remove_file(tmp_audio_path).await;

                // 获取合并后文件大小，如果失败则使用视频和音频大小之和
                tokio::fs::metadata(page_path)
                    .await
                    .map(|metadata| metadata.len())
                    .unwrap_or(video_size + audio_size)
            }
        }
    };

    // 计算并记录下载速度
    let elapsed = start_time.elapsed();
    let elapsed_secs = elapsed.as_secs_f64();

    if elapsed_secs > 0.0 && total_bytes > 0 {
        let speed_bps = total_bytes as f64 / elapsed_secs;
        let (speed, unit) = if speed_bps >= 1_048_576.0 {
            (speed_bps / 1_048_576.0, "MB/s")
        } else if speed_bps >= 1_024.0 {
            (speed_bps / 1_024.0, "KB/s")
        } else {
            (speed_bps, "B/s")
        };

        let download_type = if audio_only { "音频" } else { "视频" };
        info!(
            "{}下载完成，总大小: {:.2} MB，耗时: {:.2} 秒，平均速度: {:.2} {}",
            download_type,
            total_bytes as f64 / 1_048_576.0,
            elapsed_secs,
            speed,
            unit
        );
    }

    Ok(ExecutionStatus::Succeeded)
}

pub async fn fetch_page_danmaku(
    should_run: bool,
    bili_client: &BiliClient,
    video_model: &video::Model,
    page_info: &PageInfo,
    danmaku_path: PathBuf,
    token: CancellationToken,
) -> Result<ExecutionStatus> {
    if !should_run {
        return Ok(ExecutionStatus::Skipped);
    }

    // 检查 CID 是否有效（-1 表示信息获取失败）
    if page_info.cid < 0 {
        warn!(
            "视频 {} 的 CID 无效（{}），跳过弹幕下载",
            &video_model.name, page_info.cid
        );
        return Ok(ExecutionStatus::Ignored(anyhow::anyhow!("CID 无效，无法下载弹幕")));
    }

    // 检查是否为番剧，如果是番剧则需要从API获取正确的 aid
    let bili_video = if video_model.source_type == Some(1) {
        // 番剧：需要从API获取aid
        if let Some(ep_id) = &video_model.ep_id {
            match tokio::select! {
                biased;
                _ = token.cancelled() => None,
                res = get_bangumi_aid_from_api(bili_client, ep_id, token.clone()) => res,
            } {
                Some(aid) => {
                    debug!("使用番剧API获取到的aid: {}", aid);
                    Video::new_with_aid(bili_client, video_model.bvid.clone(), aid)
                }
                None => {
                    warn!("无法获取番剧 {} (EP{}) 的AID，使用bvid转换", &video_model.name, ep_id);
                    Video::new(bili_client, video_model.bvid.clone())
                }
            }
        } else {
            warn!("番剧 {} 缺少EP ID，使用bvid转换aid", &video_model.name);
            Video::new(bili_client, video_model.bvid.clone())
        }
    } else {
        // 普通视频：使用 bvid 转换的 aid
        Video::new(bili_client, video_model.bvid.clone())
    };

    let danmaku_writer = tokio::select! {
        biased;
        _ = token.cancelled() => return Err(anyhow!("Download cancelled")),
        res = bili_video.get_danmaku_writer(page_info, token.clone()) => res?,
    };

    danmaku_writer.write(danmaku_path).await?;
    Ok(ExecutionStatus::Succeeded)
}

pub async fn fetch_page_subtitle(
    should_run: bool,
    bili_client: &BiliClient,
    video_model: &video::Model,
    page_info: &PageInfo,
    subtitle_path: &Path,
    token: CancellationToken,
) -> Result<ExecutionStatus> {
    if !should_run {
        return Ok(ExecutionStatus::Skipped);
    }
    let bili_video = Video::new(bili_client, video_model.bvid.clone());
    let subtitles = tokio::select! {
        biased;
        _ = token.cancelled() => return Err(anyhow!("Download cancelled")),
        res = bili_video.get_subtitles(page_info) => res?,
    };
    let tasks = subtitles
        .into_iter()
        .map(|subtitle| async move {
            let path = subtitle_path.with_extension(format!("{}.srt", subtitle.lan));
            ensure_parent_dir_for_file(&path).await.map_err(std::io::Error::other)?;
            tokio::fs::write(path, subtitle.body.to_string()).await
        })
        .collect::<FuturesUnordered<_>>();
    tasks.try_collect::<Vec<()>>().await?;
    Ok(ExecutionStatus::Succeeded)
}

pub async fn generate_page_nfo(
    should_run: bool,
    video_model: &video::Model,
    page_model: &page::Model,
    nfo_path: PathBuf,
    _connection: &DatabaseConnection,
    season_number_override: Option<i32>,
) -> Result<ExecutionStatus> {
    if !should_run {
        return Ok(ExecutionStatus::Skipped);
    }
    // 检查是否为番剧
    let is_bangumi = video_model.category == 1;

    let nfo = match video_model.single_page {
        Some(single_page) => {
            if single_page {
                if is_bangumi || video_model.collection_id.is_some() {
                    // 番剧单页或合集视频应使用Episode格式，符合Emby标准
                    use crate::utils::nfo::Episode;
                    let mut episode = Episode::from_video_and_page(video_model, page_model);
                    if let Some(season_number) = season_number_override {
                        episode.season = season_number.max(1);
                    }
                    // 对于合集视频，如果数据库中尚未带有 episode_number，按合集顺序编号
                    if video_model.collection_id.is_some() && video_model.episode_number.is_none() {
                        if let Some(col_id) = video_model.collection_id {
                            if let Ok(ep_no) =
                                get_collection_video_episode_number(_connection, col_id, &video_model.bvid).await
                            {
                                episode.episode_number = ep_no;
                            }
                        }
                    }
                    NFO::Episode(episode)
                } else {
                    // 普通单页视频生成Movie
                    use crate::utils::nfo::Movie;
                    NFO::Movie(Movie::from_video_with_pages(video_model, &[page_model.clone()]))
                }
            } else {
                use crate::utils::nfo::Episode;
                let mut episode = Episode::from_video_and_page(video_model, page_model);
                if let Some(season_number) = season_number_override {
                    episode.season = season_number.max(1);
                }
                NFO::Episode(episode)
            }
        }
        None => {
            use crate::utils::nfo::Episode;
            let mut episode = Episode::from_video_and_page(video_model, page_model);
            if let Some(season_number) = season_number_override {
                episode.season = season_number.max(1);
            }
            // 非番剧但属于合集的视频：按合集顺序编号，避免固定为1
            if video_model.category != 1 {
                if let Some(col_id) = video_model.collection_id {
                    if video_model.episode_number.is_none() {
                        if let Ok(ep_no) =
                            get_collection_video_episode_number(_connection, col_id, &video_model.bvid).await
                        {
                            episode.episode_number = ep_no;
                        }
                    }
                }
            }
            NFO::Episode(episode)
        }
    };
    generate_nfo(nfo, nfo_path).await?;
    Ok(ExecutionStatus::Succeeded)
}

#[allow(clippy::too_many_arguments)]
pub async fn fetch_video_poster(
    should_run: bool,
    video_model: &video::Model,
    downloader: &UnifiedDownloader,
    poster_path: PathBuf,
    fanart_path: PathBuf,
    token: CancellationToken,
    custom_cover_url: Option<&str>,
    custom_fanart_url: Option<&str>,
) -> Result<ExecutionStatus> {
    // 兜底修复：如果状态显示封面已完成但 fanart 文件缺失，则用已存在的 thumb 生成 fanart。
    if !should_run {
        if !fanart_path.exists() && poster_path.exists() {
            ensure_parent_dir_for_file(&fanart_path).await?;
            fs::copy(&poster_path, &fanart_path).await?;
            return Ok(ExecutionStatus::Succeeded);
        }
        return Ok(ExecutionStatus::Skipped);
    }

    debug!("开始处理视频「{}」的封面和背景图", video_model.name);
    debug!("  thumb路径: {:?}", poster_path);
    debug!("  fanart路径: {:?}", fanart_path);
    debug!("  custom_thumb_url: {:?}", custom_cover_url);
    debug!("  custom_fanart_url: {:?}", custom_fanart_url);

    // 下载thumb封面（依赖should_run参数，重置状态后会强制重新下载）
    let thumb_url = custom_cover_url.unwrap_or(video_model.cover.as_str());
    let urls = vec![thumb_url];
    tokio::select! {
        biased;
        _ = token.cancelled() => return Ok(ExecutionStatus::Cancelled),
        res = downloader.fetch_with_fallback(&urls, &poster_path) => res,
    }?;

    // 下载fanart背景图
    ensure_parent_dir_for_file(&fanart_path).await?;
    if let Some(fanart_url) = custom_fanart_url {
        // 如果有专门的fanart URL，独立下载
        let fanart_urls = vec![fanart_url];
        tokio::select! {
            biased;
            _ = token.cancelled() => return Ok(ExecutionStatus::Cancelled),
            res = downloader.fetch_with_fallback(&fanart_urls, &fanart_path) => {
                match res {
                    Ok(_) => {
                        info!("✓ 成功下载fanart背景图: {}", fanart_url);
                        return Ok(ExecutionStatus::Succeeded);
                    },
                    Err(e) => {
                        warn!("✗ fanart背景图下载失败，URL: {}, 错误: {:#}", fanart_url, e);
                        warn!("回退策略：复制thumb作为fanart");
                        // fanart下载失败，回退到复制thumb
                        if poster_path.exists() {
                            fs::copy(&poster_path, &fanart_path).await?;
                        } else {
                            warn!("thumb文件不存在，无法复制作为fanart");
                        }
                    }
                }
            },
        }
    } else {
        // 没有专门的fanart URL，直接复制thumb
        if poster_path.exists() {
            fs::copy(&poster_path, &fanart_path).await?;
        } else {
            warn!("thumb文件不存在，无法复制作为fanart");
        }
    }

    Ok(ExecutionStatus::Succeeded)
}

pub async fn fetch_upper_face(
    should_run: bool,
    video_model: &video::Model,
    downloader: &UnifiedDownloader,
    upper_face_path: PathBuf,
    token: CancellationToken,
) -> Result<ExecutionStatus> {
    if !should_run {
        return Ok(ExecutionStatus::Skipped);
    }

    // 检查URL是否有效，避免相对路径或空URL
    let upper_face_url = &video_model.upper_face;
    if upper_face_url.is_empty() || !upper_face_url.starts_with("http") {
        debug!("跳过无效的作者头像URL: {}", upper_face_url);
        return Ok(ExecutionStatus::Ignored(anyhow::anyhow!("无效的作者头像URL")));
    }

    let urls = vec![upper_face_url.as_str()];
    tokio::select! {
        biased;
        _ = token.cancelled() => return Ok(ExecutionStatus::Cancelled),
        res = downloader.fetch_with_fallback(&urls, &upper_face_path) => res,
    }?;
    Ok(ExecutionStatus::Succeeded)
}

/// 下载联合投稿中所有staff成员的头像
pub async fn fetch_staff_faces(
    should_run: bool,
    video_model: &video::Model,
    downloader: &UnifiedDownloader,
    token: CancellationToken,
) -> Result<ExecutionStatus> {
    if !should_run {
        return Ok(ExecutionStatus::Skipped);
    }

    // 检查是否有staff信息
    let staff_info = match &video_model.staff_info {
        Some(info) => info,
        None => {
            debug!("视频 {} 没有staff信息，跳过下载staff头像", video_model.bvid);
            return Ok(ExecutionStatus::Skipped);
        }
    };

    // 解析staff信息
    let staff_list: Vec<crate::bilibili::StaffInfo> = match serde_json::from_value(staff_info.clone()) {
        Ok(list) => list,
        Err(e) => {
            warn!("解析staff信息失败: {}", e);
            return Ok(ExecutionStatus::Ignored(anyhow::anyhow!("解析staff信息失败")));
        }
    };

    // 如果只有一个成员，不需要下载（主UP主的头像已经通过fetch_upper_face下载了）
    if staff_list.len() <= 1 {
        debug!(
            "视频 {} staff列表只有{}个成员，跳过下载staff头像",
            video_model.bvid,
            staff_list.len()
        );
        return Ok(ExecutionStatus::Skipped);
    }

    debug!(
        "开始下载视频 {} 的{}个staff成员头像",
        video_model.bvid,
        staff_list.len()
    );

    // 获取配置
    let current_config = crate::config::reload_config();
    let mut success_count = 0;
    let mut failed_count = 0;

    // 为每个staff成员下载头像
    for staff in &staff_list {
        // 跳过主UP主（已经通过fetch_upper_face下载了）
        if staff.mid == video_model.upper_id {
            continue;
        }

        // 检查头像URL是否有效
        if staff.face.is_empty() || !staff.face.starts_with("http") {
            debug!("跳过无效的staff头像URL: {} ({})", staff.name, staff.face);
            continue;
        }

        // 构建staff成员的头像路径（类似主UP主的路径结构）
        let staff_name = crate::utils::filenamify::filenamify(&staff.name);
        let first_char = staff_name.chars().next().unwrap_or('_').to_string();
        let staff_upper_path = current_config.upper_path.join(&first_char).join(&staff_name);
        let staff_face_path = staff_upper_path.join("folder.jpg");

        // 下载头像
        let urls = vec![staff.face.as_str()];
        match tokio::select! {
            biased;
            _ = token.cancelled() => {
                debug!("取消下载staff头像: {}", staff.name);
                Ok(ExecutionStatus::Skipped)
            },
            res = downloader.fetch_with_fallback(&urls, &staff_face_path) => {
                match res {
                    Ok(_) => {
                        debug!("成功下载staff头像: {} -> {:?}", staff.name, staff_face_path);
                        success_count += 1;
                        Ok(ExecutionStatus::Succeeded)
                    },
                    Err(e) => {
                        warn!("下载staff头像失败: {} - {}", staff.name, e);
                        failed_count += 1;
                        Err(e)
                    }
                }
            }
        } {
            Ok(_) => {}
            Err(e) => {
                // 继续处理其他staff成员，不中断整个流程
                debug!("下载staff头像时出错（继续处理其他成员）: {}", e);
            }
        }
    }

    if success_count > 0 {
        info!("成功下载{}个staff成员头像，失败{}个", success_count, failed_count);
        Ok(ExecutionStatus::Succeeded)
    } else if failed_count > 0 {
        Ok(ExecutionStatus::Ignored(anyhow::anyhow!("所有staff头像下载失败")))
    } else {
        Ok(ExecutionStatus::Skipped)
    }
}

pub async fn generate_upper_nfo(
    should_run: bool,
    video_model: &video::Model,
    nfo_path: PathBuf,
) -> Result<ExecutionStatus> {
    if !should_run {
        return Ok(ExecutionStatus::Skipped);
    }
    generate_nfo(NFO::Upper(video_model.into()), nfo_path).await?;
    Ok(ExecutionStatus::Succeeded)
}

pub async fn fetch_bangumi_poster(
    should_run: bool,
    video_model: &video::Model,
    downloader: &UnifiedDownloader,
    poster_path: PathBuf,
    token: CancellationToken,
    custom_poster_url: Option<&str>,
) -> Result<ExecutionStatus> {
    if !should_run {
        return Ok(ExecutionStatus::Skipped);
    }

    // 如果目标文件已存在且非空，则跳过重复下载：
    // - 仅对番剧/合集/多P的根目录 poster.jpg / folder.jpg 生效（避免每季/每集都重复拉取）
    // - SeasonXX-poster.jpg 等“带前缀”的文件不跳过（重置封面时需要可重新下载）
    let skip_if_exists = matches!(
        poster_path.file_name().and_then(|n| n.to_str()),
        Some("poster.jpg") | Some("folder.jpg")
    );
    if skip_if_exists && poster_path.exists() {
        match std::fs::metadata(&poster_path) {
            Ok(meta) if meta.is_file() && meta.len() > 0 => {
                debug!("番剧封面已存在，跳过下载: {:?}", poster_path);
                return Ok(ExecutionStatus::Succeeded);
            }
            Ok(_) => {}
            Err(e) => debug!("读取番剧封面文件信息失败（继续尝试下载）: {:?} - {}", poster_path, e),
        }
    }

    debug!("开始处理番剧「{}」的主封面 poster.jpg", video_model.name);
    debug!("  poster路径: {:?}", poster_path);
    debug!("  custom_poster_url: {:?}", custom_poster_url);

    // 下载 poster.jpg 文件（依赖should_run参数，重置状态后会强制重新下载）
    ensure_parent_dir_for_file(&poster_path).await?;

    let poster_url = custom_poster_url.unwrap_or(video_model.cover.as_str());
    let urls = vec![poster_url];

    tokio::select! {
        biased;
        _ = token.cancelled() => return Ok(ExecutionStatus::Cancelled),
        res = downloader.fetch_with_fallback(&urls, &poster_path) => res,
    }?;

    debug!("✓ 成功下载番剧主封面 poster.jpg: {}", poster_url);
    Ok(ExecutionStatus::Succeeded)
}

pub async fn generate_video_nfo(
    should_run: bool,
    video_model: &video::Model,
    nfo_path: PathBuf,
) -> Result<ExecutionStatus> {
    if !should_run {
        return Ok(ExecutionStatus::Skipped);
    }
    generate_nfo(NFO::TVShow(video_model.into()), nfo_path).await?;
    Ok(ExecutionStatus::Succeeded)
}

/// 为合集生成带有合集信息的TVShow NFO
pub async fn generate_collection_video_nfo(
    should_run: bool,
    video_model: &video::Model,
    collection_name: Option<&str>,
    collection_cover: Option<&str>,
    collection_description: Option<&str>,
    nfo_path: PathBuf,
    season_number: i32,
    total_seasons: Option<i32>,
    total_episodes: Option<i32>,
    season_total_episodes: Option<i32>,
    season_nfo_path: Option<PathBuf>,
) -> Result<ExecutionStatus> {
    if !should_run {
        return Ok(ExecutionStatus::Skipped);
    }
    use crate::utils::nfo::{Season, TVShow};
    let tvshow = TVShow::from_video_with_collection(
        video_model,
        collection_name,
        collection_cover,
        collection_description,
        season_number,
        total_seasons,
        total_episodes,
    );
    generate_nfo(NFO::TVShow(tvshow), nfo_path).await?;

    if let Some(season_nfo_path) = season_nfo_path {
        let season = Season::from_video_with_collection(
            video_model,
            collection_name,
            collection_cover,
            season_number,
            season_total_episodes,
        );
        generate_nfo(NFO::Season(season), season_nfo_path).await?;
    }

    Ok(ExecutionStatus::Succeeded)
}

/// 为番剧生成带有API数据的TVShow NFO
pub async fn generate_bangumi_video_nfo(
    should_run: bool,
    video_model: &video::Model,
    season_info: &SeasonInfo,
    nfo_path: PathBuf,
) -> Result<ExecutionStatus> {
    if !should_run {
        return Ok(ExecutionStatus::Skipped);
    }
    use crate::utils::nfo::TVShow;
    let tvshow = TVShow::from_season_info(video_model, season_info);
    generate_nfo(NFO::TVShow(tvshow), nfo_path).await?;
    Ok(ExecutionStatus::Succeeded)
}

/// 为番剧季度生成season.nfo文件
pub async fn generate_bangumi_season_nfo(
    should_run: bool,
    video_model: &video::Model,
    season_info: &SeasonInfo,
    season_path: PathBuf,
    _season_number: u32,
) -> Result<ExecutionStatus> {
    if !should_run {
        return Ok(ExecutionStatus::Skipped);
    }

    let nfo_path = season_path.join("season.nfo");

    // 检查文件是否已存在（但仍然继续生成，确保内容更新）
    if nfo_path.exists() {
        debug!("Season NFO文件已存在，将覆盖更新: {:?}", nfo_path);
    }

    use crate::utils::nfo::Season;
    let mut season = Season::from_season_info(video_model, season_info);
    season.season_number = _season_number as i32; // 设置正确的季度编号

    generate_nfo(NFO::Season(season), nfo_path.clone()).await?;
    info!("成功生成season.nfo: {:?} (季度{})", nfo_path, _season_number);
    Ok(ExecutionStatus::Succeeded)
}

/// 按需创建目录的辅助函数，只在实际需要写入文件时创建
async fn ensure_parent_dir_for_file(file_path: &std::path::Path) -> Result<()> {
    if let Some(parent) = file_path.parent() {
        if !parent.exists() {
            fs::create_dir_all(parent).await?;
            debug!("按需创建目录: {}", parent.display());
        }
    }
    Ok(())
}

async fn generate_nfo(nfo: NFO<'_>, nfo_path: PathBuf) -> Result<()> {
    // 只在实际写入NFO文件时才创建父目录
    ensure_parent_dir_for_file(&nfo_path).await?;
    fs::write(nfo_path, nfo.generate_nfo().await?.as_bytes()).await?;
    Ok(())
}

/// 获取番剧季标题，优先从缓存获取，缓存未命中时从API获取
async fn get_cached_season_title(
    bili_client: &BiliClient,
    season_id: &str,
    token: CancellationToken,
) -> Option<String> {
    // 先检查缓存
    if let Ok(cache) = SEASON_TITLE_CACHE.lock() {
        if let Some(title) = cache.get(season_id) {
            return Some(title.clone());
        }
    }

    // 缓存未命中，从API获取
    get_season_title_from_api(bili_client, season_id, token).await
}

async fn get_season_title_from_api(
    bili_client: &BiliClient,
    season_id: &str,
    token: CancellationToken,
) -> Option<String> {
    let url = format!("https://api.bilibili.com/pgc/view/web/season?season_id={}", season_id);

    // 重试配置：最大重试3次，每次重试间隔递增
    let max_retries = 3;
    let mut retry_count = 0;

    while retry_count <= max_retries {
        // 检查是否被取消
        if token.is_cancelled() {
            debug!("请求被取消，停止重试");
            return None;
        }

        let retry_delay = std::time::Duration::from_millis(500 * (retry_count as u64 + 1));
        if retry_count > 0 {
            debug!("第{}次重试获取季度信息，延迟{}ms", retry_count, retry_delay.as_millis());
            tokio::time::sleep(retry_delay).await;
        }

        match tokio::select! {
            biased;
            _ = token.cancelled() => return None,
            res = bili_client.get(&url, token.clone()) => res,
        } {
            Ok(res) => {
                if res.status().is_success() {
                    match res.json::<serde_json::Value>().await {
                        Ok(json) => {
                            // 检查API返回是否成功
                            if json["code"].as_i64().unwrap_or(-1) == 0 {
                                // 获取季度标题并标准化空格
                                if let Some(title) = json["result"]["title"].as_str() {
                                    // 标准化空格：将多个连续空格合并为单个空格，去除括号前的空格
                                    let normalized_title = title
                                        .split_whitespace()  // 分割字符串，自动去除首尾空格并处理连续空格
                                        .collect::<Vec<_>>()
                                        .join(" ")           // 用单个空格重新连接
                                        .replace(" （", "（") // 去除全角括号前的空格
                                        .replace(" (", "("); // 去除半角括号前的空格

                                    debug!("获取到季度标题: {} (尝试次数: {})", normalized_title, retry_count + 1);

                                    // 缓存清理后的番剧标题
                                    if let Ok(mut cache) = SEASON_TITLE_CACHE.lock() {
                                        cache.insert(season_id.to_string(), normalized_title.clone());
                                    }

                                    return Some(normalized_title);
                                }
                            } else {
                                let error_msg = json["message"].as_str().unwrap_or("未知错误");
                                // API返回错误码通常不是临时性问题，记录一次警告后直接返回
                                warn!("获取季度信息失败，API返回错误: {} (season_id={})", error_msg, season_id);
                                return None;
                            }
                        }
                        Err(e) => {
                            // JSON解析失败通常不是临时性问题，记录一次警告后直接返回
                            warn!("解析季度信息JSON失败: {} (season_id={})", e, season_id);
                            return None;
                        }
                    }
                } else {
                    // 重试过程中的错误使用 debug，避免日志过多
                    debug!(
                        "获取季度信息HTTP请求失败，状态码: {} (尝试次数: {}/{})",
                        res.status(),
                        retry_count + 1,
                        max_retries + 1
                    );
                }
            }
            Err(e) => {
                // 重试过程中的网络错误使用 debug
                debug!(
                    "发送季度信息请求失败: {} (尝试次数: {}/{})",
                    e,
                    retry_count + 1,
                    max_retries + 1
                );
            }
        }

        retry_count += 1;
    }

    warn!(
        "获取season_id={}的季度信息失败，已重试{}次，网络可能存在问题",
        season_id, max_retries
    );
    None
}

/// 从番剧API获取指定EP的AID
async fn get_bangumi_aid_from_api(bili_client: &BiliClient, ep_id: &str, token: CancellationToken) -> Option<String> {
    let url = format!("https://api.bilibili.com/pgc/view/web/season?ep_id={}", ep_id);

    // 重试配置：最大重试3次，每次重试间隔递增
    let max_retries = 3;
    let mut retry_count = 0;

    while retry_count <= max_retries {
        // 检查是否被取消
        if token.is_cancelled() {
            debug!("请求被取消，停止重试");
            return None;
        }

        let retry_delay = std::time::Duration::from_millis(500 * (retry_count as u64 + 1));
        if retry_count > 0 {
            debug!("第{}次重试获取EP信息，延时{}ms", retry_count, retry_delay.as_millis());
            tokio::time::sleep(retry_delay).await;
        }

        match tokio::select! {
            biased;
            _ = token.cancelled() => return None,
            res = bili_client.get(&url, token.clone()) => res,
        } {
            Ok(res) => {
                if res.status().is_success() {
                    match res.json::<serde_json::Value>().await {
                        Ok(json) => {
                            // 检查API返回是否成功
                            if json["code"].as_i64().unwrap_or(-1) == 0 {
                                // 在episodes数组中查找对应EP的AID
                                if let Some(episodes) = json["result"]["episodes"].as_array() {
                                    for episode in episodes {
                                        if let Some(episode_id) = episode["id"].as_i64() {
                                            if episode_id.to_string() == ep_id {
                                                debug!("获取到EP {} 的AID (尝试次数: {})", ep_id, retry_count + 1);
                                                return episode["aid"].as_i64().map(|aid| aid.to_string());
                                            }
                                        }
                                    }
                                }
                                // 找不到对应的EP，不是网络问题，直接返回
                                warn!("在episodes数组中找不到EP {}", ep_id);
                                return None;
                            } else {
                                warn!(
                                    "获取EP信息失败，API返回错误: {} (尝试次数: {})",
                                    json["message"].as_str().unwrap_or("未知错误"),
                                    retry_count + 1
                                );
                                // API返回错误码通常不是临时性问题，直接返回
                                return None;
                            }
                        }
                        Err(e) => {
                            warn!("解析番剧API响应失败: {} (尝试次数: {})", e, retry_count + 1);
                            // JSON解析失败通常不是临时性问题，直接返回
                            return None;
                        }
                    }
                } else {
                    warn!(
                        "请求EP信息HTTP失败，状态码: {} (尝试次数: {})",
                        res.status(),
                        retry_count + 1
                    );
                }
            }
            Err(e) => {
                warn!("请求番剧API失败: {} (尝试次数: {})", e, retry_count + 1);
            }
        }

        retry_count += 1;
    }

    error!("获取ep_id={}的AID失败，已重试{}次", ep_id, max_retries);
    None
}

/// 从番剧API获取指定EP的CID和duration
async fn get_bangumi_info_from_api(
    bili_client: &BiliClient,
    ep_id: &str,
    token: CancellationToken,
) -> Option<(i64, u32)> {
    let url = format!("https://api.bilibili.com/pgc/view/web/season?ep_id={}", ep_id);

    // 重试配置：最大重试3次，每次重试间隔递增
    let max_retries = 3;
    let mut retry_count = 0;

    while retry_count <= max_retries {
        // 检查是否被取消
        if token.is_cancelled() {
            debug!("请求被取消，停止重试");
            return None;
        }

        let retry_delay = std::time::Duration::from_millis(500 * (retry_count as u64 + 1));
        if retry_count > 0 {
            debug!(
                "第{}次重试获取EP详细信息，延时{}ms",
                retry_count,
                retry_delay.as_millis()
            );
            tokio::time::sleep(retry_delay).await;
        }

        match tokio::select! {
            biased;
            _ = token.cancelled() => return None,
            res = bili_client.get(&url, token.clone()) => res,
        } {
            Ok(res) => {
                if res.status().is_success() {
                    match res.json::<serde_json::Value>().await {
                        Ok(json) => {
                            // 检查API返回是否成功
                            if json["code"].as_i64().unwrap_or(-1) == 0 {
                                // 在episodes数组中查找对应EP的信息
                                if let Some(episodes) = json["result"]["episodes"].as_array() {
                                    for episode in episodes {
                                        if let Some(episode_id) = episode["id"].as_i64() {
                                            if episode_id.to_string() == ep_id {
                                                let cid = episode["cid"].as_i64().unwrap_or(0);
                                                // duration在API中是毫秒，需要转换为秒
                                                let duration_ms = episode["duration"].as_i64().unwrap_or(0);
                                                let duration_sec = (duration_ms / 1000) as u32;
                                                debug!(
                                                    "获取到番剧EP {} 的CID: {}, 时长: {}秒 (尝试次数: {})",
                                                    ep_id,
                                                    cid,
                                                    duration_sec,
                                                    retry_count + 1
                                                );
                                                return Some((cid, duration_sec));
                                            }
                                        }
                                    }
                                }
                                // 找不到对应的EP，不是网络问题，直接返回
                                warn!("在episodes数组中找不到EP {}", ep_id);
                                return None;
                            } else {
                                warn!(
                                    "获取EP详细信息失败，API返回错误: {} (尝试次数: {})",
                                    json["message"].as_str().unwrap_or("未知错误"),
                                    retry_count + 1
                                );
                                // API返回错误码通常不是临时性问题，直接返回
                                return None;
                            }
                        }
                        Err(e) => {
                            warn!("解析番剧API响应失败: {} (尝试次数: {})", e, retry_count + 1);
                            // JSON解析失败通常不是临时性问题，直接返回
                            return None;
                        }
                    }
                } else {
                    warn!(
                        "请求EP详细信息HTTP失败，状态码: {} (尝试次数: {})",
                        res.status(),
                        retry_count + 1
                    );
                }
            }
            Err(e) => {
                warn!("请求番剧API失败: {} (尝试次数: {})", e, retry_count + 1);
            }
        }

        retry_count += 1;
    }

    error!("获取ep_id={}的详细信息失败，已重试{}次", ep_id, max_retries);
    None
}

/// 从现有数据库中获取该季已有的分集信息
async fn get_existing_episodes_for_season(
    connection: &DatabaseConnection,
    season_id: &str,
    bili_client: &BiliClient,
    token: CancellationToken,
) -> Result<HashMap<String, (i64, u32)>> {
    use sea_orm::*;

    // 查询该season_id下所有已有page信息的视频
    let existing_data = video::Entity::find()
        .filter(video::Column::SeasonId.eq(season_id))
        .filter(video::Column::SourceType.eq(1)) // 番剧类型
        .filter(video::Column::EpId.is_not_null())
        .find_with_related(page::Entity)
        .all(connection)
        .await?;

    let mut episodes_map = HashMap::new();

    for (video, pages) in existing_data {
        if let Some(ep_id) = video.ep_id {
            // 每个番剧视频通常只有一个page（单集）
            if let Some(page) = pages.first() {
                // 只缓存有效CID，避免把历史的异常占位值（如-1）当作有效缓存，导致后续无法自愈
                if page.cid > 0 {
                    episodes_map.insert(ep_id, (page.cid, page.duration));
                } else {
                    warn!(
                        "发现无效的番剧分集缓存，将忽略: season_id={} ep_id={} page_id={} cid={}",
                        season_id, ep_id, page.id, page.cid
                    );
                }
            }
        }
    }

    if !episodes_map.is_empty() {
        // 尝试获取番剧标题用于显示
        let season_title = get_season_title_from_api(bili_client, season_id, token.clone()).await;
        let display_name = season_title.as_deref().unwrap_or(season_id);

        info!(
            "从数据库缓存中找到季 {} 「{}」的 {} 个分集信息",
            season_id,
            display_name,
            episodes_map.len()
        );
    }

    Ok(episodes_map)
}

/// 从API获取整个番剧季的信息（单次请求）
async fn get_season_info_from_api(
    bili_client: &BiliClient,
    season_id: &str,
    token: CancellationToken,
) -> Result<SeasonInfo> {
    let url = format!("https://api.bilibili.com/pgc/view/web/season?season_id={}", season_id);

    let res = tokio::select! {
        biased;
        _ = token.cancelled() => return Err(anyhow!("Request cancelled")),
        res = bili_client.get(&url, token.clone()) => res,
    }?;

    if !res.status().is_success() {
        bail!("获取番剧季信息失败，HTTP状态码: {}", res.status());
    }

    let json: serde_json::Value = res
        .json()
        .await
        .with_context(|| format!("解析番剧季 {} 响应失败", season_id))?;

    if json["code"].as_i64().unwrap_or(-1) != 0 {
        let error_code = json["code"].as_i64().unwrap_or(-1);
        let error_msg = json["message"].as_str().unwrap_or("未知错误").to_string();

        // 创建BiliError以触发风控检测
        let bili_error = crate::bilibili::BiliError::RequestFailed(error_code, error_msg.clone());
        let error = anyhow::Error::from(bili_error);

        // 使用错误分类器检测风控
        let classified_error = crate::error::ErrorClassifier::classify_error(&error);
        if classified_error.error_type == crate::error::ErrorType::RiskControl {
            // 风控错误，触发下载中止
            return Err(anyhow!(crate::error::DownloadAbortError()));
        }

        // 其他错误正常返回，使用BiliError以便被错误分类系统处理
        return Err(crate::bilibili::BiliError::RequestFailed(error_code, error_msg.to_string()).into());
    }

    let result = &json["result"];

    // 获取番剧标题
    let title = result["title"]
        .as_str()
        .unwrap_or(&format!("番剧{}", season_id))
        .to_string();

    // 缓存番剧标题
    if let Ok(mut cache) = SEASON_TITLE_CACHE.lock() {
        cache.insert(season_id.to_string(), title.clone());
    }

    // 提取API中的丰富元数据
    let alias = result["alias"].as_str().map(|s| s.to_string());
    let evaluate = result["evaluate"].as_str().map(|s| s.to_string());

    // 评分信息
    let rating = result["rating"]["score"].as_f64().map(|r| r as f32);
    let rating_count = result["rating"]["count"].as_i64();

    // 制作地区
    let areas: Vec<String> = result["areas"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|area| area["name"].as_str().map(|s| s.to_string()))
        .collect();

    // 声优演员信息（格式化为字符串）
    let actors = if let Some(actors_array) = result["actors"].as_array() {
        let actor_list: Vec<String> = actors_array
            .iter()
            .filter_map(|actor| {
                let character = actor["title"].as_str()?;
                let actor_name = actor["actor"].as_str()?;
                Some(format!("{}：{}", character, actor_name))
            })
            .collect();

        if !actor_list.is_empty() {
            Some(actor_list.join("\n"))
        } else {
            None
        }
    } else {
        None
    };

    // 类型标签
    let styles: Vec<String> = result["styles"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter_map(|style| style["name"].as_str().map(|s| s.to_string()))
        .collect();

    // 播出状态
    let status = result["publish"]["pub_time_show"].as_str().map(|s| {
        if s.contains("完结") || s.contains("全") {
            "Ended".to_string()
        } else if s.contains("更新") || s.contains("连载") {
            "Continuing".to_string()
        } else {
            "Ended".to_string() // 默认为完结
        }
    });

    // 其他元数据
    let total_episodes = result["total"].as_i64().map(|t| t as i32);
    let cover = result["cover"].as_str().map(|s| s.to_string());
    let series_cover = result["seasons"]
        .as_array()
        .and_then(|seasons| {
            let mut best: Option<(u32, String)> = None;

            for season in seasons {
                let Some(cover_url) = season["cover"].as_str().filter(|s| !s.is_empty()) else {
                    continue;
                };

                // 只以“第X季”为有效季数，避免特别篇/番外等误判为第1季
                let Some(season_title) = season["season_title"].as_str().filter(|s| s.contains('季')) else {
                    continue;
                };

                let season_full_title = season["title"].as_str().unwrap_or(title.as_str());
                let (_, season_number) =
                    crate::utils::bangumi_name_extractor::BangumiNameExtractor::extract_series_name_and_season(
                        season_full_title,
                        Some(season_title),
                    );

                match best {
                    None => best = Some((season_number, cover_url.to_string())),
                    Some((best_no, _)) if season_number < best_no => {
                        best = Some((season_number, cover_url.to_string()))
                    }
                    _ => {}
                }
            }

            best.map(|(_, cover_url)| cover_url)
        })
        .or_else(|| cover.clone());

    // 从seasons数组中查找当前season的横版封面信息
    let (new_ep_cover, horizontal_cover_1610, horizontal_cover_169, bkg_cover) = if let Some(seasons_array) =
        result["seasons"].as_array()
    {
        debug!(
            "seasons数组查找: 目标season_id={}, 数组长度={}",
            season_id,
            seasons_array.len()
        );

        // 在seasons数组中查找当前season_id对应的条目，同时记录第一个有横版封面的条目作为备选
        let mut target_season_covers = Vec::new(); // 目标season_id的所有条目
        let mut first_available_covers = None;

        for (index, season) in seasons_array.iter().enumerate() {
            // 简化调试输出
            let season_season_id = season["season_id"].as_i64().unwrap_or(-1);
            debug!("处理seasons[{}]: season_id={}", index, season_season_id);

            // 检查当前条目是否有有效的横版封面（作为备选）
            let current_h1610 = season["horizontal_cover_1610"].as_str().filter(|s| !s.is_empty());
            let current_h169 = season["horizontal_cover_169"].as_str().filter(|s| !s.is_empty());
            let current_bkg = season["bkg_cover"].as_str().filter(|s| !s.is_empty());
            let current_new_ep_cover = season["new_ep"]["cover"].as_str().filter(|s| !s.is_empty());

            // 如果还没有备选条目，且当前条目有有效的横版封面，就记录它
            if first_available_covers.is_none()
                && (current_new_ep_cover.is_some()
                    || current_h1610.is_some()
                    || current_h169.is_some()
                    || current_bkg.is_some())
            {
                let covers = (
                    current_new_ep_cover.map(|s| s.to_string()),
                    current_h1610.map(|s| s.to_string()),
                    current_h169.map(|s| s.to_string()),
                    current_bkg.map(|s| s.to_string()),
                );
                first_available_covers = Some(covers);
                debug!("💾 记录为第一个可用的横版封面备选：season_id={}", season_season_id);
            }

            // 检查是否匹配当前season_id
            if season_season_id.to_string() == season_id {
                debug!(
                    "✓ 找到匹配的season_id: {} (第{}个条目)",
                    season_season_id,
                    target_season_covers.len() + 1
                );
                // 找到了当前season，提取横版封面信息
                let new_ep = season["new_ep"]["cover"].as_str().map(|s| s.to_string());
                let h1610 = season["horizontal_cover_1610"].as_str().map(|s| s.to_string());
                let h169 = season["horizontal_cover_169"].as_str().map(|s| s.to_string());
                let bkg = season["bkg_cover"].as_str().map(|s| s.to_string());
                debug!(
                    "  字段提取: new_ep={:?}, h1610={:?}, h169={:?}, bkg={:?}",
                    new_ep, h1610, h169, bkg
                );
                target_season_covers.push((new_ep, h1610, h169, bkg));
                // 不要break，继续查找是否还有其他相同season_id的条目
            }
        }

        // 从目标season的所有条目中选择第一个有有效横版封面的
        let found_season_covers = if !target_season_covers.is_empty() {
            debug!(
                "共找到 {} 个 season_id {} 的条目",
                target_season_covers.len(),
                season_id
            );

            // 先寻找有有效横版封面的条目
            let valid_cover = target_season_covers.iter().find(|(new_ep, h1610, h169, bkg)| {
                new_ep.as_ref().map(|s| !s.is_empty()).unwrap_or(false)
                    || h1610.as_ref().map(|s| !s.is_empty()).unwrap_or(false)
                    || h169.as_ref().map(|s| !s.is_empty()).unwrap_or(false)
                    || bkg.as_ref().map(|s| !s.is_empty()).unwrap_or(false)
            });

            if let Some(covers) = valid_cover {
                debug!("✓ 找到有有效横版封面的season_id {} 条目", season_id);
                Some(covers.clone())
            } else {
                warn!("⚠️ 目标season {} 的所有条目都没有有效的横版封面", season_id);
                target_season_covers.first().cloned()
            }
        } else {
            None
        };

        // 智能fallback逻辑
        match found_season_covers {
            Some((new_ep, h1610, h169, bkg)) => {
                // 检查找到的season是否有有效的横版封面
                let has_valid_covers = new_ep.as_ref().map(|s| !s.is_empty()).unwrap_or(false)
                    || h1610.as_ref().map(|s| !s.is_empty()).unwrap_or(false)
                    || h169.as_ref().map(|s| !s.is_empty()).unwrap_or(false)
                    || bkg.as_ref().map(|s| !s.is_empty()).unwrap_or(false);

                if has_valid_covers {
                    debug!("✓ 目标season {} 有有效的横版封面，直接使用", season_id);
                    (new_ep, h1610, h169, bkg)
                } else if let Some((fallback_new_ep, fallback_h1610, fallback_h169, fallback_bkg)) =
                    first_available_covers
                {
                    warn!("⚠️ 目标season {} 没有有效的横版封面，使用第一个可用的备选", season_id);
                    debug!(
                        "  备选横版封面: new_ep={:?}, h1610={:?}, h169={:?}, bkg={:?}",
                        fallback_new_ep, fallback_h1610, fallback_h169, fallback_bkg
                    );
                    (fallback_new_ep, fallback_h1610, fallback_h169, fallback_bkg)
                } else {
                    warn!(
                        "⚠️ 目标season {} 和所有备选都没有有效的横版封面，使用顶层字段",
                        season_id
                    );
                    (
                        None, // 顶层没有new_ep字段
                        result["horizontal_cover_1610"].as_str().map(|s| s.to_string()),
                        result["horizontal_cover_169"].as_str().map(|s| s.to_string()),
                        result["bkg_cover"].as_str().map(|s| s.to_string()),
                    )
                }
            }
            None => {
                // 完全没找到目标season，使用备选或顶层
                if let Some((fallback_new_ep, fallback_h1610, fallback_h169, fallback_bkg)) = first_available_covers {
                    warn!("⚠️ 未找到目标season {}，使用第一个可用的备选", season_id);
                    info!(
                        "  备选横版封面: new_ep={:?}, h1610={:?}, h169={:?}, bkg={:?}",
                        fallback_new_ep, fallback_h1610, fallback_h169, fallback_bkg
                    );
                    (fallback_new_ep, fallback_h1610, fallback_h169, fallback_bkg)
                } else {
                    warn!("⚠️ 未找到目标season {} 且无备选，使用顶层字段", season_id);
                    (
                        None, // 顶层没有new_ep字段
                        result["horizontal_cover_1610"].as_str().map(|s| s.to_string()),
                        result["horizontal_cover_169"].as_str().map(|s| s.to_string()),
                        result["bkg_cover"].as_str().map(|s| s.to_string()),
                    )
                }
            }
        }
    } else {
        // 没有seasons数组，使用顶层字段
        warn!("API响应中没有seasons数组，使用顶层字段");
        (
            None, // 顶层没有new_ep字段
            result["horizontal_cover_1610"].as_str().map(|s| s.to_string()),
            result["horizontal_cover_169"].as_str().map(|s| s.to_string()),
            result["bkg_cover"].as_str().map(|s| s.to_string()),
        )
    };
    let media_id = result["media_id"].as_i64();
    let publish_time = result["publish"]["pub_time_show"].as_str().map(|s| s.to_string());
    let total_views = result["stat"]["views"].as_i64();
    let total_favorites = result["stat"]["favorites"].as_i64();

    let episodes: Vec<EpisodeInfo> = result["episodes"]
        .as_array()
        .context("找不到分集列表")?
        .iter()
        .filter_map(|ep| {
            let ep_id = ep["id"].as_i64()?.to_string();
            let cid = ep["cid"].as_i64()?;
            let duration_ms = ep["duration"].as_i64()?;
            let duration = (duration_ms / 1000) as u32;

            Some(EpisodeInfo { ep_id, cid, duration })
        })
        .collect();

    info!(
        "成功获取番剧季 {} 「{}」完整信息：{} 集，评分 {:?}，制作地区 {:?}，类型 {:?}",
        season_id,
        title,
        episodes.len(),
        rating,
        areas,
        styles
    );

    // 获取show_season_type
    let show_season_type = result["type"].as_i64().map(|v| v as i32);

    Ok(SeasonInfo {
        title,
        episodes,
        alias,
        evaluate,
        rating,
        rating_count,
        areas,
        actors,
        styles,
        total_episodes,
        status,
        cover,
        series_cover,
        new_ep_cover,
        horizontal_cover_1610,
        horizontal_cover_169,
        bkg_cover,
        media_id,
        season_id: season_id.to_string(),
        publish_time,
        total_views,
        total_favorites,
        show_season_type,
    })
}

/// 处理单个番剧视频
async fn process_bangumi_video(
    bili_client: &BiliClient,
    video_model: video::Model,
    episodes_map: &HashMap<String, (i64, u32)>,
    connection: &DatabaseConnection,
    video_source: &VideoSourceEnum,
    token: CancellationToken,
) -> Result<()> {
    let Some(ep_id) = video_model.ep_id.as_deref() else {
        warn!(
            "番剧「{}」缺少EP ID，跳过详情填充（保留未填充状态便于下次重试）",
            video_model.name
        );
        return Ok(());
    };

    let info = match episodes_map.get(ep_id).copied() {
        Some((cid, duration)) if cid > 0 => {
            debug!("使用缓存信息: EP{} -> CID={}, Duration={}s", ep_id, cid, duration);
            Some((cid, duration))
        }
        _ => {
            warn!("找不到分集 {} 的有效信息，尝试从番剧API补全", ep_id);
            get_bangumi_info_from_api(bili_client, ep_id, token.clone())
                .await
                .filter(|(cid, _)| *cid > 0)
        }
    };

    let Some((actual_cid, duration)) = info else {
        warn!(
            "番剧「{}」(EP{}) 无法获取CID，跳过详情填充（保留未填充状态便于下次重试）",
            video_model.name, ep_id
        );
        return Ok(());
    };

    let should_update_video_cid = video_model.cid.is_none();

    let txn = connection.begin().await?;

    let page_info = PageInfo {
        cid: actual_cid,
        page: 1,
        name: video_model.name.clone(),
        duration,
        first_frame: None,
        dimension: None,
    };

    // 创建page记录（这里会自动缓存cid和duration到数据库）
    create_pages(vec![page_info], &video_model, &txn).await?;

    // 更新视频状态
    let mut video_active_model: bili_sync_entity::video::ActiveModel = video_model.into();
    video_source.set_relation_id(&mut video_active_model);
    if should_update_video_cid {
        video_active_model.cid = Set(Some(actual_cid));
    }
    video_active_model.single_page = Set(Some(true)); // 番剧的每一集都是单页
    video_active_model.tags = Set(Some(serde_json::Value::Array(vec![]))); // 空标签数组
    video_active_model.save(&txn).await?;

    txn.commit().await?;

    Ok(())
}

/// 获取特定视频源的视频数量
async fn get_video_count_for_source(video_source: &VideoSourceEnum, connection: &DatabaseConnection) -> Result<usize> {
    let count = video::Entity::find()
        .filter(video_source.filter_expr())
        .count(connection)
        .await?;
    Ok(count as usize)
}

/// 自动重置风控导致的失败任务
/// 当检测到风控时，将所有失败状态(值为3)、正在进行状态(值为2)以及未完成的任务重置为未开始状态(值为0)
pub async fn auto_reset_risk_control_failures(connection: &DatabaseConnection) -> Result<()> {
    use crate::utils::status::{PageStatus, VideoStatus};
    use bili_sync_entity::{page, video};
    use sea_orm::*;

    info!("检测到风控，开始自动重置失败、进行中和未完成的下载任务...");

    // 查询所有视频和页面数据
    let (all_videos, all_pages) = tokio::try_join!(
        video::Entity::find()
            .select_only()
            .columns([video::Column::Id, video::Column::Name, video::Column::DownloadStatus,])
            .into_tuple::<(i32, String, u32)>()
            .all(connection),
        page::Entity::find()
            .select_only()
            .columns([page::Column::Id, page::Column::Name, page::Column::DownloadStatus,])
            .into_tuple::<(i32, String, u32)>()
            .all(connection)
    )?;

    let mut resetted_videos = 0;
    let mut resetted_pages = 0;

    let txn = connection.begin().await?;

    // 重置视频失败、进行中和未完成状态
    for (id, name, download_status) in all_videos {
        let mut video_status = VideoStatus::from(download_status);
        let mut video_resetted = false;

        // 检查是否为完全成功的状态（所有任务都是1）
        let is_fully_completed = (0..5).all(|task_index| video_status.get(task_index) == 1);

        if !is_fully_completed {
            // 如果不是完全成功，检查所有任务索引，将失败状态(3)、正在进行状态(2)和未开始状态(0)重置为未开始(0)
            for task_index in 0..5 {
                let status_value = video_status.get(task_index);
                if status_value == 3 || status_value == 2 || status_value == 0 {
                    video_status.set(task_index, 0); // 重置为未开始
                    video_resetted = true;
                }
            }
        }

        if video_resetted {
            video::Entity::update(video::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(id),
                download_status: sea_orm::Set(video_status.into()),
                ..Default::default()
            })
            .exec(&txn)
            .await?;

            resetted_videos += 1;
            debug!("重置视频「{}」的未完成任务状态", name);
        }
    }

    // 重置页面失败、进行中和未完成状态
    for (id, name, download_status) in all_pages {
        let mut page_status = PageStatus::from(download_status);
        let mut page_resetted = false;

        // 检查是否为完全成功的状态（所有任务都是1）
        let is_fully_completed = (0..5).all(|task_index| page_status.get(task_index) == 1);

        if !is_fully_completed {
            // 如果不是完全成功，检查所有任务索引，将失败状态(3)、正在进行状态(2)和未开始状态(0)重置为未开始(0)
            for task_index in 0..5 {
                let status_value = page_status.get(task_index);
                if status_value == 3 || status_value == 2 || status_value == 0 {
                    page_status.set(task_index, 0); // 重置为未开始
                    page_resetted = true;
                }
            }
        }

        if page_resetted {
            page::Entity::update(page::ActiveModel {
                id: sea_orm::ActiveValue::Unchanged(id),
                download_status: sea_orm::Set(page_status.into()),
                ..Default::default()
            })
            .exec(&txn)
            .await?;

            resetted_pages += 1;
            debug!("重置页面「{}」的未完成任务状态", name);
        }
    }

    txn.commit().await?;

    if resetted_videos > 0 || resetted_pages > 0 {
        info!(
            "风控自动重置完成：重置了 {} 个视频和 {} 个页面的未完成任务状态",
            resetted_videos, resetted_pages
        );
    } else {
        info!("风控自动重置完成：所有任务都已完成，无需重置");
    }

    Ok(())
}

/// 获取合集中视频的集数序号
/// 首先检查数据库中是否已有episode_number，如果没有则从API获取正确顺序并更新
fn extract_season_number_from_path(path: &str) -> Option<i32> {
    Path::new(path).components().find_map(|component| {
        let segment = component.as_os_str().to_string_lossy();
        let suffix = segment.strip_prefix("Season ")?;
        let number = suffix.trim().parse::<i32>().ok()?;
        (number > 0).then_some(number)
    })
}

async fn get_submission_collection_video_episode_number(
    connection: &DatabaseConnection,
    video_model: &video::Model,
) -> Result<i32> {
    use sea_orm::*;

    let source_submission_id = video_model
        .source_submission_id
        .context("投稿UGC合集缺少source_submission_id")?;
    let season_id = video_model
        .season_id
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .context("投稿UGC合集缺少season_id")?;

    let pubtime_text = video_model.pubtime.format("%Y-%m-%d %H:%M:%S").to_string();

    let row = connection
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            r#"
            SELECT COUNT(*)
            FROM video
            WHERE source_submission_id = ?
              AND season_id = ?
              AND (
                    pubtime < ?
                    OR (pubtime = ? AND id <= ?)
                  )
            "#,
            vec![
                source_submission_id.into(),
                season_id.to_string().into(),
                pubtime_text.clone().into(),
                pubtime_text.into(),
                video_model.id.into(),
            ],
        ))
        .await?;

    let count = row
        .and_then(|r| r.try_get_by_index::<i64>(0).ok())
        .and_then(|v| i32::try_from(v).ok())
        .unwrap_or(0);

    Ok(count.max(1))
}

fn derive_collection_mapping_key(raw_collection_id: &str) -> i32 {
    if let Ok(id) = raw_collection_id.parse::<i32>() {
        if id > 0 {
            return id;
        }
    }

    // 兜底：非纯数字ID时，使用稳定哈希生成正整数键。
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    raw_collection_id.hash(&mut hasher);
    let hashed = hasher.finish();
    ((hashed % (i32::MAX as u64 - 1)) + 1) as i32
}

fn build_submission_collection_key(collection_type: &str, sid: &str) -> Option<String> {
    let sid = sid.trim();
    if sid.is_empty() {
        return None;
    }
    match collection_type {
        "season" => Some(sid.to_string()),
        "series" => Some(format!("series_{}", sid)),
        _ => None,
    }
}

async fn get_submission_collection_meta(
    bili_client: &BiliClient,
    upper_id: i64,
    season_id: &str,
    token: CancellationToken,
) -> Option<SubmissionCollectionMeta> {
    let season_id = season_id.trim();
    if season_id.is_empty() {
        return None;
    }

    if let Ok(cache) = SUBMISSION_COLLECTION_META_CACHE.lock() {
        if let Some(meta_map) = cache.get(&upper_id) {
            if let Some(meta) = meta_map.get(season_id) {
                return Some(meta.clone());
            }
        }
    }

    if token.is_cancelled() {
        return None;
    }

    let response = match bili_client.get_user_collections(upper_id, 1, 20).await {
        Ok(resp) => resp,
        Err(err) => {
            warn!(
                "加载投稿合集元数据失败（upper_id={}，season_id={}）: {}",
                upper_id, season_id, err
            );
            return None;
        }
    };

    let mut meta_map: HashMap<String, SubmissionCollectionMeta> = HashMap::new();
    for item in response.collections {
        let Some(key) = build_submission_collection_key(&item.collection_type, &item.sid) else {
            continue;
        };
        let name = item.name.trim();
        if name.is_empty() {
            continue;
        }
        let cover = item.cover.trim();
        let description = item.description.trim();
        meta_map.insert(
            key,
            SubmissionCollectionMeta {
                name: name.to_string(),
                cover: if cover.is_empty() {
                    None
                } else {
                    Some(cover.to_string())
                },
                description: if description.is_empty() {
                    None
                } else {
                    Some(description.to_string())
                },
            },
        );
    }

    if let Ok(mut cache) = SUBMISSION_COLLECTION_META_CACHE.lock() {
        cache.insert(upper_id, meta_map.clone());
    }

    meta_map.get(season_id).cloned()
}

#[derive(Debug, Clone, Copy)]
struct CollectionNfoStats {
    total_seasons: i32,
    total_episodes: i32,
    season_total_episodes: i32,
}

async fn query_count_i32(connection: &DatabaseConnection, sql: &str, params: Vec<sea_orm::Value>) -> Result<i32> {
    use sea_orm::*;

    let row = connection
        .query_one(Statement::from_sql_and_values(DatabaseBackend::Sqlite, sql, params))
        .await?;
    let value = row.and_then(|r| r.try_get_by_index::<i64>(0).ok()).unwrap_or(0);
    Ok(i32::try_from(value).unwrap_or(0))
}

async fn get_collection_nfo_stats(
    connection: &DatabaseConnection,
    video_source: &VideoSourceEnum,
    video_model: &video::Model,
) -> Result<CollectionNfoStats> {
    match video_source {
        VideoSourceEnum::Collection(collection_source) => {
            let config = crate::config::reload_config();
            let is_up_seasonal = config.collection_folder_mode.as_ref() == "up_seasonal";

            if is_up_seasonal {
                let up_dir_name = {
                    let safe_upper_name = crate::utils::filenamify::filenamify(video_model.upper_name.trim());
                    if safe_upper_name.is_empty() {
                        format!("UP_{}", collection_source.m_id)
                    } else {
                        safe_upper_name
                    }
                };
                let grouped_base_path = Path::new(&collection_source.path)
                    .join(&up_dir_name)
                    .to_string_lossy()
                    .to_string();

                let total_seasons = query_count_i32(
                    connection,
                    r#"
                    SELECT COUNT(*)
                    FROM collection_season_mapping
                    WHERE up_mid = ? AND base_path = ?
                    "#,
                    vec![collection_source.m_id.into(), grouped_base_path.clone().into()],
                )
                .await?;

                let total_episodes = query_count_i32(
                    connection,
                    r#"
                    SELECT COUNT(*)
                    FROM video
                    WHERE collection_id IN (
                        SELECT collection_id
                        FROM collection_season_mapping
                        WHERE up_mid = ? AND base_path = ?
                    )
                    "#,
                    vec![collection_source.m_id.into(), grouped_base_path.clone().into()],
                )
                .await?;

                let season_number = extract_season_number_from_path(&video_model.path).unwrap_or(1).max(1);
                let season_total_episodes = query_count_i32(
                    connection,
                    r#"
                    SELECT COUNT(*)
                    FROM video
                    WHERE collection_id IN (
                        SELECT collection_id
                        FROM collection_season_mapping
                        WHERE up_mid = ? AND base_path = ? AND season_id = ?
                    )
                    "#,
                    vec![
                        collection_source.m_id.into(),
                        grouped_base_path.into(),
                        season_number.into(),
                    ],
                )
                .await?;

                Ok(CollectionNfoStats {
                    total_seasons: total_seasons.max(1),
                    total_episodes: total_episodes.max(1),
                    season_total_episodes: season_total_episodes.max(1),
                })
            } else {
                let total_episodes = query_count_i32(
                    connection,
                    "SELECT COUNT(*) FROM video WHERE collection_id = ?",
                    vec![collection_source.id.into()],
                )
                .await?;
                Ok(CollectionNfoStats {
                    total_seasons: 1,
                    total_episodes: total_episodes.max(1),
                    season_total_episodes: total_episodes.max(1),
                })
            }
        }
        VideoSourceEnum::Submission(submission_source)
            if is_submission_ugc_collection_video(video_source, video_model) =>
        {
            let season_key = video_model
                .season_id
                .as_deref()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or("");

            let total_episodes = query_count_i32(
                connection,
                r#"
                SELECT COUNT(*)
                FROM video
                WHERE source_submission_id = ?
                  AND season_id IS NOT NULL
                  AND TRIM(season_id) != ''
                "#,
                vec![submission_source.id.into()],
            )
            .await?;

            let season_total_episodes = if season_key.is_empty() {
                0
            } else {
                query_count_i32(
                    connection,
                    r#"
                    SELECT COUNT(*)
                    FROM video
                    WHERE source_submission_id = ?
                      AND season_id = ?
                    "#,
                    vec![submission_source.id.into(), season_key.to_string().into()],
                )
                .await?
            };

            let config = crate::config::reload_config();
            let total_seasons = if config.collection_folder_mode.as_ref() == "up_seasonal" {
                let up_dir_name = {
                    let safe_upper_name = crate::utils::filenamify::filenamify(video_model.upper_name.trim());
                    if safe_upper_name.is_empty() {
                        format!("UP_{}", submission_source.upper_id)
                    } else {
                        safe_upper_name
                    }
                };
                let grouped_base_path = Path::new(&submission_source.path)
                    .join(&up_dir_name)
                    .to_string_lossy()
                    .to_string();
                query_count_i32(
                    connection,
                    r#"
                    SELECT COUNT(*)
                    FROM collection_season_mapping
                    WHERE up_mid = ? AND base_path = ?
                    "#,
                    vec![submission_source.upper_id.into(), grouped_base_path.into()],
                )
                .await?
            } else {
                query_count_i32(
                    connection,
                    r#"
                    SELECT COUNT(DISTINCT season_id)
                    FROM video
                    WHERE source_submission_id = ?
                      AND season_id IS NOT NULL
                      AND TRIM(season_id) != ''
                    "#,
                    vec![submission_source.id.into()],
                )
                .await?
            };

            Ok(CollectionNfoStats {
                total_seasons: total_seasons.max(1),
                total_episodes: total_episodes.max(1),
                season_total_episodes: season_total_episodes.max(1),
            })
        }
        _ => Ok(CollectionNfoStats {
            total_seasons: 1,
            total_episodes: 1,
            season_total_episodes: 1,
        }),
    }
}

/// 读取投稿UP主在「合集/系列」中的完整视频归属，用于回填缺失season_id。
/// 返回: bvid -> (collection_key, episode_number)
/// - season 类型使用原始 sid（如 "4523"）
/// - series 类型使用前缀 sid（如 "series_5024459"）
async fn build_submission_collection_membership_map(
    bili_client: &BiliClient,
    upper_id: i64,
    token: CancellationToken,
) -> Result<HashMap<String, (String, i32)>> {
    let mut map: HashMap<String, (String, i32)> = HashMap::new();
    let collections = bili_client.get_user_collections(upper_id, 1, 20).await?;

    for item in collections.collections {
        if token.is_cancelled() {
            return Err(anyhow!("Download cancelled"));
        }

        let collection_type = match item.collection_type.as_str() {
            "season" => CollectionType::Season,
            "series" => CollectionType::Series,
            _ => continue,
        };
        let collection_key = match collection_type {
            CollectionType::Season => item.sid.clone(),
            CollectionType::Series => format!("series_{}", item.sid),
        };

        let collection_item = CollectionItem {
            mid: upper_id.to_string(),
            sid: item.sid.clone(),
            collection_type: collection_type.clone(),
        };
        let collection = Collection::new(bili_client, &collection_item);

        let order_map = match collection.get_video_order_map().await {
            Ok(v) => v,
            Err(err) => {
                warn!(
                    "获取投稿列表归属失败（upper_id={}, type={}, sid={}）: {}",
                    upper_id, item.collection_type, item.sid, err
                );
                continue;
            }
        };

        for (bvid, episode_number) in order_map {
            if episode_number <= 0 {
                continue;
            }

            match map.get(&bvid) {
                None => {
                    map.insert(bvid, (collection_key.clone(), episode_number));
                }
                Some((existing_key, _)) => {
                    // 同时属于 season+series 时优先使用 season 归属（更稳定）
                    let existing_is_series = existing_key.starts_with("series_");
                    let new_is_series = collection_key.starts_with("series_");
                    if existing_is_series && !new_is_series {
                        map.insert(bvid, (collection_key.clone(), episode_number));
                    }
                }
            }
        }

        // 轻微限速，降低连续请求触发风控的概率
        tokio::time::sleep(Duration::from_millis(120)).await;
    }

    Ok(map)
}

async fn backfill_submission_collection_membership(
    connection: &DatabaseConnection,
    source_submission_id: i32,
    membership: &HashMap<String, (String, i32)>,
) -> Result<u64> {
    use sea_orm::*;

    if membership.is_empty() {
        return Ok(0);
    }

    let mut updated_rows = 0u64;
    let txn = connection.begin().await?;

    for (bvid, (collection_key, episode_number)) in membership {
        let result = txn
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Sqlite,
                r#"
                UPDATE video
                SET season_id = ?, episode_number = COALESCE(episode_number, ?)
                WHERE source_submission_id = ?
                  AND bvid = ?
                  AND (season_id IS NULL OR TRIM(season_id) = '')
                "#,
                vec![
                    collection_key.clone().into(),
                    (*episode_number).into(),
                    source_submission_id.into(),
                    bvid.clone().into(),
                ],
            ))
            .await?;
        updated_rows += result.rows_affected();
    }

    txn.commit().await?;
    Ok(updated_rows)
}

async fn normalize_collection_season_mapping_group(
    connection: &DatabaseConnection,
    up_mid: i64,
    base_path: &str,
) -> Result<()> {
    use sea_orm::*;

    let rows = connection
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            r#"
            SELECT id, season_id
            FROM collection_season_mapping
            WHERE up_mid = ? AND base_path = ?
            ORDER BY pub_year ASC, pub_quarter ASC, reference_pubtime ASC, id ASC
            "#,
            vec![up_mid.into(), base_path.to_string().into()],
        ))
        .await?;

    if rows.is_empty() {
        return Ok(());
    }

    let mut changed = 0usize;
    let mut expected: i32 = 1;

    for row in rows {
        let id = row.try_get_by_index::<i64>(0).ok().filter(|v| *v > 0);
        let current = row
            .try_get_by_index::<i64>(1)
            .ok()
            .and_then(|v| i32::try_from(v).ok())
            .unwrap_or(expected);

        if let Some(id) = id {
            if current != expected {
                connection
                    .execute(Statement::from_sql_and_values(
                        DatabaseBackend::Sqlite,
                        r#"
                        UPDATE collection_season_mapping
                        SET season_id = ?, updated_at = CURRENT_TIMESTAMP
                        WHERE id = ?
                        "#,
                        vec![expected.into(), id.into()],
                    ))
                    .await?;
                changed += 1;
            }
        }
        expected += 1;
    }

    if changed > 0 {
        info!(
            "已规范季度映射顺序: up_mid={}, base_path='{}', 修正 {} 条映射为从 Season 01 连续编号",
            up_mid, base_path, changed
        );
    }

    Ok(())
}

async fn get_submission_source_season_number(
    connection: &DatabaseConnection,
    submission_source: &bili_sync_entity::submission::Model,
    video_model: &video::Model,
) -> Result<i32> {
    use sea_orm::*;

    let season_id_str = video_model
        .season_id
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .context("投稿UGC合集缺少season_id")?;
    let mapping_collection_id = derive_collection_mapping_key(season_id_str);

    let up_mid = submission_source.upper_id;
    let up_dir_name = {
        let safe_upper_name = crate::utils::filenamify::filenamify(video_model.upper_name.trim());
        if safe_upper_name.is_empty() {
            format!("UP_{}", up_mid)
        } else {
            safe_upper_name
        }
    };
    let base_path = Path::new(&submission_source.path)
        .join(&up_dir_name)
        .to_string_lossy()
        .to_string();
    let reference_pubtime = video_model.pubtime;
    let pub_year = reference_pubtime.year();
    let pub_quarter = ((reference_pubtime.month0() / 3) + 1) as i32;

    normalize_collection_season_mapping_group(connection, up_mid, &base_path).await?;

    // 1) 先检查该UGC合集ID是否已有映射并与当前关键字段一致
    let mapping_row = connection
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            r#"
            SELECT season_id, up_mid, base_path, pub_year, pub_quarter
            FROM collection_season_mapping
            WHERE collection_id = ?
            LIMIT 1
            "#,
            vec![mapping_collection_id.into()],
        ))
        .await?;

    if let Some(row) = mapping_row {
        let mapped_season_id = row
            .try_get_by_index::<i64>(0)
            .ok()
            .and_then(|v| i32::try_from(v).ok())
            .filter(|v| *v > 0);
        let mapped_up_mid = row.try_get_by_index::<i64>(1).ok();
        let mapped_base_path = row.try_get_by_index::<String>(2).ok();
        let mapped_year = row.try_get_by_index::<i32>(3).ok();
        let mapped_quarter = row.try_get_by_index::<i32>(4).ok();

        if mapped_season_id.is_some()
            && mapped_up_mid == Some(up_mid)
            && mapped_base_path.as_deref() == Some(base_path.as_str())
            && mapped_year == Some(pub_year)
            && mapped_quarter == Some(pub_quarter)
        {
            return Ok(mapped_season_id.unwrap());
        }
    }

    // 2) 查询同UP+同路径+同季度是否已有已分配season_id
    let same_quarter_row = connection
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            r#"
            SELECT season_id
            FROM collection_season_mapping
            WHERE up_mid = ? AND base_path = ? AND pub_year = ? AND pub_quarter = ?
            ORDER BY id ASC
            LIMIT 1
            "#,
            vec![
                up_mid.into(),
                base_path.clone().into(),
                pub_year.into(),
                pub_quarter.into(),
            ],
        ))
        .await?;

    let season_number = if let Some(row) = same_quarter_row {
        row.try_get_by_index::<i64>(0)
            .ok()
            .and_then(|v| i32::try_from(v).ok())
            .filter(|v| *v > 0)
            .unwrap_or(1)
    } else {
        // 3) 同分组无同季度映射时，分配下一个季号
        let max_row = connection
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Sqlite,
                r#"
                SELECT COALESCE(MAX(season_id), 0)
                FROM collection_season_mapping
                WHERE up_mid = ? AND base_path = ?
                "#,
                vec![up_mid.into(), base_path.clone().into()],
            ))
            .await?;

        let max_season = max_row
            .and_then(|row| row.try_get_by_index::<i64>(0).ok())
            .and_then(|v| i32::try_from(v).ok())
            .unwrap_or(0);
        (max_season + 1).max(1)
    };

    let reference_pubtime_text = reference_pubtime.format("%Y-%m-%d %H:%M:%S").to_string();

    // 4) 写回映射，保证后续稳定
    connection
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            r#"
            INSERT INTO collection_season_mapping (
                collection_id, up_mid, base_path, pub_year, pub_quarter, season_id, reference_pubtime, created_at, updated_at
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
            ON CONFLICT(collection_id) DO UPDATE SET
                up_mid = excluded.up_mid,
                base_path = excluded.base_path,
                pub_year = excluded.pub_year,
                pub_quarter = excluded.pub_quarter,
                season_id = excluded.season_id,
                reference_pubtime = excluded.reference_pubtime,
                updated_at = CURRENT_TIMESTAMP
            "#,
            vec![
                mapping_collection_id.into(),
                up_mid.into(),
                base_path.into(),
                pub_year.into(),
                pub_quarter.into(),
                season_number.into(),
                reference_pubtime_text.into(),
            ],
        ))
        .await?;

    Ok(season_number)
}

async fn get_collection_source_season_number(
    connection: &DatabaseConnection,
    collection_source: &bili_sync_entity::collection::Model,
    video_model: &video::Model,
) -> Result<i32> {
    use bili_sync_entity::video;
    use sea_orm::*;

    // 读取该合集最早投稿时间（用于季度映射）
    let earliest_video = video::Entity::find()
        .filter(video::Column::CollectionId.eq(collection_source.id))
        .order_by_asc(video::Column::Pubtime)
        .one(connection)
        .await?;

    let up_dir_name = {
        let safe_upper_name = crate::utils::filenamify::filenamify(video_model.upper_name.trim());
        if safe_upper_name.is_empty() {
            format!("UP_{}", collection_source.m_id)
        } else {
            safe_upper_name
        }
    };
    let grouped_base_path = Path::new(&collection_source.path)
        .join(&up_dir_name)
        .to_string_lossy()
        .to_string();
    let reference_pubtime = earliest_video
        .as_ref()
        .map(|item| item.pubtime)
        .or_else(|| parse_time_string(&collection_source.created_at))
        .unwrap_or_else(now_naive);
    let pub_year = reference_pubtime.year();
    let pub_quarter = ((reference_pubtime.month0() / 3) + 1) as i32;

    normalize_collection_season_mapping_group(connection, collection_source.m_id, &grouped_base_path).await?;

    // 1) 先尝试读取当前合集已有映射（若匹配当前关键字段，直接复用）
    let mapping_row = connection
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            r#"
            SELECT season_id, up_mid, base_path, pub_year, pub_quarter
            FROM collection_season_mapping
            WHERE collection_id = ?
            LIMIT 1
            "#,
            vec![collection_source.id.into()],
        ))
        .await?;

    if let Some(row) = mapping_row {
        let mapped_season_id = row
            .try_get_by_index::<i64>(0)
            .ok()
            .and_then(|v| i32::try_from(v).ok())
            .filter(|v| *v > 0);
        let mapped_up_mid = row.try_get_by_index::<i64>(1).ok();
        let mapped_base_path = row.try_get_by_index::<String>(2).ok();
        let mapped_year = row.try_get_by_index::<i32>(3).ok();
        let mapped_quarter = row.try_get_by_index::<i32>(4).ok();

        if mapped_season_id.is_some()
            && mapped_up_mid == Some(collection_source.m_id)
            && mapped_base_path.as_deref() == Some(grouped_base_path.as_str())
            && mapped_year == Some(pub_year)
            && mapped_quarter == Some(pub_quarter)
        {
            return Ok(mapped_season_id.unwrap());
        }
    }

    // 2) 找同分组同季度已有season_id（同UP + 同路径 + 同年季度）
    let same_quarter_row = connection
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            r#"
            SELECT season_id
            FROM collection_season_mapping
            WHERE up_mid = ? AND base_path = ? AND pub_year = ? AND pub_quarter = ?
            ORDER BY id ASC
            LIMIT 1
            "#,
            vec![
                collection_source.m_id.into(),
                grouped_base_path.clone().into(),
                pub_year.into(),
                pub_quarter.into(),
            ],
        ))
        .await?;

    let season_id = if let Some(row) = same_quarter_row {
        row.try_get_by_index::<i64>(0)
            .ok()
            .and_then(|v| i32::try_from(v).ok())
            .filter(|v| *v > 0)
            .unwrap_or(1)
    } else {
        // 3) 同分组无同季度映射时，分配下一个season_id
        let max_row = connection
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Sqlite,
                r#"
                SELECT COALESCE(MAX(season_id), 0)
                FROM collection_season_mapping
                WHERE up_mid = ? AND base_path = ?
                "#,
                vec![collection_source.m_id.into(), grouped_base_path.clone().into()],
            ))
            .await?;

        let max_season = max_row
            .and_then(|row| row.try_get_by_index::<i64>(0).ok())
            .and_then(|v| i32::try_from(v).ok())
            .unwrap_or(0);
        (max_season + 1).max(1)
    };

    let reference_pubtime_text = reference_pubtime.format("%Y-%m-%d %H:%M:%S").to_string();

    // 4) 持久化映射（按collection_id幂等更新）
    connection
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            r#"
            INSERT INTO collection_season_mapping (
                collection_id, up_mid, base_path, pub_year, pub_quarter, season_id, reference_pubtime, created_at, updated_at
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)
            ON CONFLICT(collection_id) DO UPDATE SET
                up_mid = excluded.up_mid,
                base_path = excluded.base_path,
                pub_year = excluded.pub_year,
                pub_quarter = excluded.pub_quarter,
                season_id = excluded.season_id,
                reference_pubtime = excluded.reference_pubtime,
                updated_at = CURRENT_TIMESTAMP
            "#,
            vec![
                collection_source.id.into(),
                collection_source.m_id.into(),
                grouped_base_path.into(),
                pub_year.into(),
                pub_quarter.into(),
                season_id.into(),
                reference_pubtime_text.into(),
            ],
        ))
        .await?;

    Ok(season_id)
}

async fn get_collection_video_episode_number(
    connection: &DatabaseConnection,
    collection_id: i32,
    bvid: &str,
) -> Result<i32> {
    use bili_sync_entity::{collection, video};
    use sea_orm::*;

    // 1. 首先检查该视频是否已有episode_number
    let video_model = video::Entity::find()
        .filter(video::Column::CollectionId.eq(collection_id))
        .filter(video::Column::Bvid.eq(bvid))
        .one(connection)
        .await?;

    if let Some(ref model) = video_model {
        if let Some(ep_num) = model.episode_number {
            return Ok(ep_num);
        }
    }

    // 2. 如果没有episode_number，从API获取正确顺序并更新所有视频
    // 获取合集信息
    let collection_model = collection::Entity::find_by_id(collection_id)
        .one(connection)
        .await?
        .ok_or_else(|| anyhow!("合集 {} 不存在", collection_id))?;

    // 构建Collection对象
    use crate::bilibili::{BiliClient, Collection, CollectionItem, CollectionType};
    let bili_client = BiliClient::new(String::new());
    let collection_item = CollectionItem {
        mid: collection_model.m_id.to_string(),
        sid: collection_model.s_id.to_string(),
        collection_type: CollectionType::from(collection_model.r#type),
    };
    let collection = Collection::new(&bili_client, &collection_item);

    // 获取正确的视频顺序
    let order_map = collection.get_video_order_map().await?;
    debug!(
        "从API获取合集 {} 的视频顺序，共 {} 个视频",
        collection_id,
        order_map.len()
    );

    // 3. 更新数据库中所有视频的episode_number
    for (video_bvid, episode_num) in &order_map {
        video::Entity::update_many()
            .filter(video::Column::CollectionId.eq(collection_id))
            .filter(video::Column::Bvid.eq(video_bvid))
            .col_expr(video::Column::EpisodeNumber, Expr::value(Some(*episode_num)))
            .exec(connection)
            .await?;
    }
    info!("已更新合集 {} 中 {} 个视频的集数序号", collection_id, order_map.len());

    // 4. 返回当前视频的集数
    order_map
        .get(bvid)
        .copied()
        .ok_or_else(|| anyhow!("视频 {} 在合集 {} 的API响应中未找到", bvid, collection_id))
}

/// 修复page表中错误的video_id
///
/// **注意**：在写穿透模式下，此功能理论上不应该需要，因为所有写操作都直接写入主数据库，
/// 确保了ID的一致性。但为了兼容可能存在的历史数据问题，仍然保留此功能。
/// 使用两阶段策略避免唯一约束冲突
pub async fn fix_page_video_ids(connection: &DatabaseConnection) -> Result<()> {
    debug!("开始检查并修复page表的video_id和cid不匹配问题");
    warn!("注意：在写穿透模式下，此数据修复功能理论上不应该需要。如果频繁出现需要修复的数据，请检查系统配置。");

    // 使用事务确保原子性
    let txn = connection.begin().await?;

    // 1. 首先处理cid不匹配的记录 - 这些应该删除
    let cid_mismatch_count: i64 = txn
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            r#"
            SELECT COUNT(*) as count 
            FROM page p 
            JOIN video v ON p.video_id = v.id 
            WHERE p.pid = 1 
            AND v.cid IS NOT NULL 
            AND p.cid != v.cid
            "#,
            vec![],
        ))
        .await?
        .and_then(|row| row.try_get_by_index::<i64>(0).ok())
        .unwrap_or(0);

    // 创建临时表来跟踪需要设置auto_download=0的video
    let mut videos_to_disable = Vec::new();

    if cid_mismatch_count > 0 {
        warn!(
            "发现 {} 条cid不匹配的page记录，这些记录的内容已变化，将删除",
            cid_mismatch_count
        );

        // 先收集这些记录对应的video_id（用于后续设置auto_download=0）
        let mismatch_videos = txn
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Sqlite,
                r#"
                SELECT DISTINCT p.video_id
                FROM page p 
                JOIN video v ON p.video_id = v.id 
                WHERE p.pid = 1 
                AND v.cid IS NOT NULL 
                AND p.cid != v.cid
                "#,
                vec![],
            ))
            .await?;

        for row in mismatch_videos {
            if let Ok(video_id) = row.try_get_by_index::<i64>(0) {
                videos_to_disable.push(video_id);
            }
        }

        // 删除cid不匹配的page记录
        let delete_mismatch_result = txn
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Sqlite,
                r#"
                DELETE FROM page 
                WHERE id IN (
                    SELECT p.id 
                    FROM page p 
                    JOIN video v ON p.video_id = v.id 
                    WHERE p.pid = 1 
                    AND v.cid IS NOT NULL 
                    AND p.cid != v.cid
                )
                "#,
                vec![],
            ))
            .await?;

        info!(
            "已删除 {} 条cid不匹配的page记录",
            delete_mismatch_result.rows_affected()
        );
    }

    // 2. 然后统计有多少page记录需要修复video_id
    let wrong_pages_count: i64 = txn
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            r#"
            SELECT COUNT(*) as count 
            FROM page p 
            LEFT JOIN video v ON p.video_id = v.id 
            WHERE v.id IS NULL
            "#,
            vec![],
        ))
        .await?
        .and_then(|row| row.try_get_by_index::<i64>(0).ok())
        .unwrap_or(0);

    if wrong_pages_count == 0 {
        debug!("所有page记录的video_id都正确，无需修复");
        txn.commit().await?;
        return Ok(());
    }

    info!("发现 {} 条page记录需要修复video_id", wrong_pages_count);

    // 3. 第一阶段：将错误的video_id设置为临时的负数值
    info!("第一阶段：设置临时video_id避免冲突...");
    let set_temp_id_sql = r#"
        UPDATE page
        SET video_id = -id
        WHERE video_id NOT IN (SELECT id FROM video)
    "#;

    let phase1_result = txn
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            set_temp_id_sql,
            vec![],
        ))
        .await?;

    info!("已将 {} 条记录设置为临时video_id", phase1_result.rows_affected());

    // 4. 第二阶段：根据cid匹配更新为正确的video_id
    info!("第二阶段：更新为正确的video_id...");

    // 4.1 修复单P视频（pid=1）- 分批处理避免冲突
    info!("开始修复单P视频...");

    // 先修复那些不会产生冲突的记录
    let fix_no_conflict_sql = r#"
        UPDATE page
        SET video_id = (
            SELECT v.id 
            FROM video v 
            WHERE v.cid = page.cid
            LIMIT 1
        )
        WHERE video_id < 0
        AND pid = 1
        AND EXISTS (SELECT 1 FROM video v WHERE v.cid = page.cid)
        AND NOT EXISTS (
            SELECT 1 FROM page p2 
            WHERE p2.video_id = (SELECT v.id FROM video v WHERE v.cid = page.cid LIMIT 1)
            AND p2.pid = 1
        )
    "#;

    let no_conflict_result = txn
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            fix_no_conflict_sql,
            vec![],
        ))
        .await?;

    info!("修复了 {} 条不冲突的单P视频记录", no_conflict_result.rows_affected());

    // 处理会冲突的记录 - 这些需要特殊处理
    let conflicting_pages = txn
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            r#"
            SELECT p1.id, p1.cid, v.id as correct_video_id, p2.id as existing_id, p2.cid as existing_cid
            FROM page p1
            JOIN video v ON p1.cid = v.cid
            LEFT JOIN page p2 ON v.id = p2.video_id AND p2.pid = 1
            WHERE p1.video_id < 0 AND p1.pid = 1 AND p2.id IS NOT NULL
            "#,
            vec![],
        ))
        .await?;

    let mut conflict_count = 0u64;
    let mut duplicate_deleted = 0u64;

    for row in conflicting_pages {
        if let (Ok(page_id), Ok(page_cid), Ok(_correct_vid), Ok(existing_id), Ok(existing_cid)) = (
            row.try_get_by_index::<i32>(0),
            row.try_get_by_index::<i64>(1),
            row.try_get_by_index::<i32>(2),
            row.try_get_by_index::<i32>(3),
            row.try_get_by_index::<i64>(4),
        ) {
            // 只有当两个page的cid相同时，才是真正的重复记录
            if page_cid == existing_cid {
                // 真正的重复，删除临时ID的那条
                txn.execute(Statement::from_sql_and_values(
                    DatabaseBackend::Sqlite,
                    r#"DELETE FROM page WHERE id = ?"#,
                    vec![page_id.into()],
                ))
                .await?;
                duplicate_deleted += 1;
                debug!("删除重复的page记录 id={} (与id={}重复)", page_id, existing_id);
            } else {
                // 不同的cid，说明是不同的视频，记录错误但不处理
                conflict_count += 1;
                warn!(
                    "发现冲突记录：page.id={} cid={} 与 page.id={} cid={} 冲突，需要手动处理",
                    page_id, page_cid, existing_id, existing_cid
                );
            }
        }
    }

    info!(
        "修复单P视频完成：删除了 {} 条真正的重复记录，发现 {} 条需要手动处理的冲突",
        duplicate_deleted, conflict_count
    );

    // 4.2 修复多P视频（pid>1）
    info!("修复多P视频的video_id...");

    // 使用路径匹配方式修复多P视频
    // 原理：同一视频的多个分P在同一目录下，通过找到同目录的pid=1记录来获取正确的video_id
    let fix_multi_p_sql = r#"
        UPDATE page
        SET video_id = (
            SELECT v.id 
            FROM page p1
            JOIN video v ON v.cid = p1.cid
            WHERE p1.pid = 1 
            -- 使用RTRIM去除文件名，只保留目录路径进行匹配
            AND RTRIM(p1.path, REPLACE(p1.path, '/', '')) = RTRIM(page.path, REPLACE(page.path, '/', ''))
            LIMIT 1
        )
        WHERE video_id < 0 
        AND pid > 1
        AND EXISTS (
            SELECT 1 FROM page p1
            JOIN video v ON v.cid = p1.cid
            WHERE p1.pid = 1 
            AND RTRIM(p1.path, REPLACE(p1.path, '/', '')) = RTRIM(page.path, REPLACE(page.path, '/', ''))
        )
    "#;

    let multi_p_result = txn
        .execute(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            fix_multi_p_sql,
            vec![],
        ))
        .await?;

    info!("修复了 {} 条多P视频的page记录", multi_p_result.rows_affected());

    // 5. 处理无法修复的记录（video_id仍为负数的）
    let orphan_count: i64 = txn
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            r#"
            SELECT COUNT(*) as count 
            FROM page 
            WHERE video_id < 0
            "#,
            vec![],
        ))
        .await?
        .and_then(|row| row.try_get_by_index::<i64>(0).ok())
        .unwrap_or(0);

    if orphan_count > 0 {
        warn!(
            "发现 {} 条无法修复的page记录（找不到对应的video），将删除这些孤立记录",
            orphan_count
        );

        // 删除无法修复的孤立记录
        let delete_result = txn
            .execute(Statement::from_sql_and_values(
                DatabaseBackend::Sqlite,
                r#"DELETE FROM page WHERE video_id < 0"#,
                vec![],
            ))
            .await?;

        info!("已删除 {} 条孤立的page记录", delete_result.rows_affected());
    }

    // 6. 设置cid不匹配的video的deleted为1
    // 但要排除已修复的video（即在修复过程中成功更新的video）
    if !videos_to_disable.is_empty() {
        info!("标记 {} 个cid不匹配video为已删除", videos_to_disable.len());

        // 收集所有已修复的video_id（这些不应该被标记为已删除）
        let fixed_videos = txn
            .query_all(Statement::from_sql_and_values(
                DatabaseBackend::Sqlite,
                r#"
                SELECT DISTINCT video_id 
                FROM page 
                WHERE video_id > 0
                "#,
                vec![],
            ))
            .await?;

        let mut fixed_video_ids = std::collections::HashSet::new();
        for row in fixed_videos {
            if let Ok(video_id) = row.try_get_by_index::<i64>(0) {
                fixed_video_ids.insert(video_id);
            }
        }

        // 只设置那些不在fixed_video_ids中的video
        let mut disabled_count = 0;
        for video_id in &videos_to_disable {
            // 如果这个video已经被修复（有正确的page记录），则跳过
            if fixed_video_ids.contains(video_id) {
                continue;
            }

            let update_result = txn
                .execute(Statement::from_sql_and_values(
                    DatabaseBackend::Sqlite,
                    r#"UPDATE video SET deleted = 1 WHERE id = ?"#,
                    vec![(*video_id).into()],
                ))
                .await?;

            if update_result.rows_affected() > 0 {
                disabled_count += 1;
            }
        }

        info!("已标记 {} 个video为已删除（排除了已修复的记录）", disabled_count);

        // 6.5 自动为涉及的源启用 scan_deleted_videos
        if disabled_count > 0 {
            info!("检测到视频被标记为已删除，正在自动启用相关源的'扫描已删除视频'功能...");

            // 查询刚刚被标记为已删除的视频的源信息
            let deleted_videos_sources = txn
                .query_all(Statement::from_sql_and_values(
                    DatabaseBackend::Sqlite,
                    r#"
                    SELECT DISTINCT 
                        submission_id,
                        collection_id,
                        favorite_id,
                        watch_later_id,
                        source_id,
                        source_type
                    FROM video 
                    WHERE deleted = 1 
                    AND id IN (SELECT value FROM json_each(?))
                    "#,
                    vec![serde_json::to_string(&videos_to_disable)?.into()],
                ))
                .await?;

            // 收集各类型源的ID
            let mut submission_ids = std::collections::HashSet::new();
            let mut collection_ids = std::collections::HashSet::new();
            let mut favorite_ids = std::collections::HashSet::new();
            let mut watch_later_ids = std::collections::HashSet::new();
            let mut bangumi_source_ids = std::collections::HashSet::new();

            for row in deleted_videos_sources {
                if let Ok(Some(id)) = row.try_get::<Option<i32>>("", "submission_id") {
                    submission_ids.insert(id);
                }
                if let Ok(Some(id)) = row.try_get::<Option<i32>>("", "collection_id") {
                    collection_ids.insert(id);
                }
                if let Ok(Some(id)) = row.try_get::<Option<i32>>("", "favorite_id") {
                    favorite_ids.insert(id);
                }
                if let Ok(Some(id)) = row.try_get::<Option<i32>>("", "watch_later_id") {
                    watch_later_ids.insert(id);
                }
                // 番剧通过source_id和source_type=1判断
                if let (Ok(Some(source_id)), Ok(Some(source_type))) = (
                    row.try_get::<Option<i32>>("", "source_id"),
                    row.try_get::<Option<i32>>("", "source_type"),
                ) {
                    if source_type == 1 {
                        bangumi_source_ids.insert(source_id);
                    }
                }
            }

            // 批量更新各个源表的scan_deleted_videos字段
            let mut enabled_sources = vec![];

            // UP主投稿
            if !submission_ids.is_empty() {
                let placeholders = submission_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
                let result = txn
                    .execute(Statement::from_sql_and_values(
                        DatabaseBackend::Sqlite,
                        format!(
                            "UPDATE submission SET scan_deleted_videos = 1 
                             WHERE id IN ({}) AND scan_deleted_videos = 0",
                            placeholders
                        ),
                        submission_ids.iter().map(|id| (*id).into()).collect::<Vec<_>>(),
                    ))
                    .await?;
                if result.rows_affected() > 0 {
                    enabled_sources.push(format!("{}个UP主投稿", result.rows_affected()));
                }
            }

            // 合集
            if !collection_ids.is_empty() {
                let placeholders = collection_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
                let result = txn
                    .execute(Statement::from_sql_and_values(
                        DatabaseBackend::Sqlite,
                        format!(
                            "UPDATE collection SET scan_deleted_videos = 1 
                             WHERE id IN ({}) AND scan_deleted_videos = 0",
                            placeholders
                        ),
                        collection_ids.iter().map(|id| (*id).into()).collect::<Vec<_>>(),
                    ))
                    .await?;
                if result.rows_affected() > 0 {
                    enabled_sources.push(format!("{}个合集", result.rows_affected()));
                }
            }

            // 收藏夹
            if !favorite_ids.is_empty() {
                let placeholders = favorite_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
                let result = txn
                    .execute(Statement::from_sql_and_values(
                        DatabaseBackend::Sqlite,
                        format!(
                            "UPDATE favorite SET scan_deleted_videos = 1 
                             WHERE id IN ({}) AND scan_deleted_videos = 0",
                            placeholders
                        ),
                        favorite_ids.iter().map(|id| (*id).into()).collect::<Vec<_>>(),
                    ))
                    .await?;
                if result.rows_affected() > 0 {
                    enabled_sources.push(format!("{}个收藏夹", result.rows_affected()));
                }
            }

            // 稍后再看
            if !watch_later_ids.is_empty() {
                let placeholders = watch_later_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
                let result = txn
                    .execute(Statement::from_sql_and_values(
                        DatabaseBackend::Sqlite,
                        format!(
                            "UPDATE watch_later SET scan_deleted_videos = 1 
                             WHERE id IN ({}) AND scan_deleted_videos = 0",
                            placeholders
                        ),
                        watch_later_ids.iter().map(|id| (*id).into()).collect::<Vec<_>>(),
                    ))
                    .await?;
                if result.rows_affected() > 0 {
                    enabled_sources.push(format!("{}个稍后再看", result.rows_affected()));
                }
            }

            // 番剧
            if !bangumi_source_ids.is_empty() {
                let placeholders = bangumi_source_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
                let result = txn
                    .execute(Statement::from_sql_and_values(
                        DatabaseBackend::Sqlite,
                        format!(
                            "UPDATE video_source SET scan_deleted_videos = 1 
                             WHERE id IN ({}) AND scan_deleted_videos = 0",
                            placeholders
                        ),
                        bangumi_source_ids.iter().map(|id| (*id).into()).collect::<Vec<_>>(),
                    ))
                    .await?;
                if result.rows_affected() > 0 {
                    enabled_sources.push(format!("{}个番剧", result.rows_affected()));
                }
            }

            if !enabled_sources.is_empty() {
                info!(
                    "已自动启用以下视频源的'扫描已删除视频'功能: {}",
                    enabled_sources.join(", ")
                );
            }
        }
    }

    // 7. 提交事务
    txn.commit().await?;

    // 8. 最终验证
    let final_check: i64 = connection
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Sqlite,
            r#"
            SELECT COUNT(*) as count 
            FROM page p 
            LEFT JOIN video v ON p.video_id = v.id 
            WHERE v.id IS NULL
            "#,
            vec![],
        ))
        .await?
        .and_then(|row| row.try_get_by_index::<i64>(0).ok())
        .unwrap_or(0);

    if final_check == 0 {
        info!("所有page记录的video_id修复完成！");
    } else {
        error!("修复后仍有 {} 条page记录的video_id错误，请检查", final_check);
    }

    Ok(())
}

/// 填充数据库中所有缺失cid的视频
/// 这个函数在迁移完成后运行，用于批量获取并填充视频的cid
pub async fn populate_missing_video_cids(
    bili_client: &BiliClient,
    connection: &DatabaseConnection,
    token: CancellationToken,
) -> Result<()> {
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

    debug!("开始检查并填充缺失的视频cid");

    // 查询所有cid为空的视频
    let videos_without_cid = video::Entity::find()
        .filter(video::Column::Cid.is_null())
        .filter(video::Column::Valid.eq(true))
        .filter(video::Column::Deleted.eq(0))
        .all(connection)
        .await?;

    if videos_without_cid.is_empty() {
        debug!("所有视频都已有cid，无需填充");
        return Ok(());
    }

    info!("发现 {} 个视频需要填充cid", videos_without_cid.len());

    // 批量处理视频，每批10个
    let chunk_size = 10;
    let total_batches = videos_without_cid.len().div_ceil(chunk_size);

    for (batch_idx, chunk) in videos_without_cid.chunks(chunk_size).enumerate() {
        if token.is_cancelled() {
            info!("cid填充任务被取消");
            return Ok(());
        }

        info!("处理第 {}/{} 批视频", batch_idx + 1, total_batches);

        let futures = chunk.iter().map(|video_model| {
            let bili_client = bili_client.clone();
            let connection = connection.clone();
            let token = token.clone();
            let video_model = video_model.clone();

            async move {
                // 获取视频详情
                let video = Video::new(&bili_client, video_model.bvid.clone());

                let view_info = tokio::select! {
                    biased;
                    _ = token.cancelled() => return Err(anyhow!("任务被取消")),
                    res = video.get_view_info() => res,
                };

                match view_info {
                    Ok(VideoInfo::Detail { pages, .. }) => {
                        // 获取第一个page的cid
                        if let Some(first_page) = pages.first() {
                            let bvid = video_model.bvid.clone();
                            let cid = first_page.cid;
                            let mut video_active_model: video::ActiveModel = video_model.into();
                            video_active_model.cid = Set(Some(cid));
                            video_active_model.save(&connection).await?;

                            // 触发异步同步到内存DB

                            debug!("成功更新视频 {} 的cid: {}", bvid, cid);
                        }
                    }
                    Err(e) => {
                        warn!("获取视频 {} 详情失败，跳过cid填充: {}", video_model.bvid, e);
                    }
                    _ => {
                        warn!("视频 {} 返回了非预期的信息类型", video_model.bvid);
                    }
                }

                Ok::<_, anyhow::Error>(())
            }
        });

        let results: Vec<_> = futures::future::join_all(futures).await;

        for result in results {
            if let Err(e) = result {
                error!("处理视频时出错: {}", e);
            }
        }

        // 批次之间添加延迟，避免触发风控
        if batch_idx < total_batches - 1 {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
    }

    info!("cid填充任务完成");
    Ok(())
}

/// 检查文件夹是否为同一视频的文件夹
fn is_same_video_folder(folder_path: &std::path::Path, video_model: &video::Model) -> bool {
    use std::fs;

    if !folder_path.exists() {
        debug!("文件夹不存在: {:?}", folder_path);
        return false;
    }

    debug!("=== 智能冲突检测开始 ===");
    debug!("检查文件夹: {:?}", folder_path);
    debug!("数据库存储路径: {}", video_model.path);
    debug!("视频BVID: {}", video_model.bvid);
    debug!("视频标题: {}", video_model.name);

    // 方法1：增强的数据库路径匹配
    let db_path = std::path::Path::new(&video_model.path);

    // 1.1 完整路径匹配
    if folder_path == db_path {
        debug!("✓ 通过完整路径匹配确认为同一视频文件夹");
        return true;
    }

    // 1.2 规范化路径比较（处理不同的路径分隔符）
    let folder_normalized = folder_path.to_string_lossy().replace('\\', "/");
    let db_normalized = db_path.to_string_lossy().replace('\\', "/");
    if folder_normalized == db_normalized {
        debug!("✓ 通过规范化路径匹配确认为同一视频文件夹");
        return true;
    }

    // 1.3 文件夹名称匹配（原有逻辑）
    if let Some(db_folder_name) = db_path.file_name() {
        if let Some(check_folder_name) = folder_path.file_name() {
            if db_folder_name == check_folder_name {
                debug!("✓ 通过文件夹名称匹配确认为同一视频文件夹");
                return true;
            }
        }
    }

    // 1.4 相对路径后缀匹配
    if let Some(db_folder_name) = db_path.file_name() {
        if folder_path.ends_with(db_folder_name) {
            debug!("✓ 通过路径后缀匹配确认为同一视频文件夹");
            return true;
        }
    }

    debug!("⚠ 数据库路径匹配失败，尝试文件内容检测");

    // 方法2：扩展的文件内容检测
    if let Ok(entries) = fs::read_dir(folder_path) {
        let mut found_media_files = false;
        let mut found_bvid_files = false;
        let mut found_title_files = false;

        for entry in entries.flatten() {
            let file_name = entry.file_name();
            let file_name_str = file_name.to_string_lossy().to_lowercase();

            // 2.1 检查包含BVID的文件（扩展文件类型）
            if file_name_str.contains(&video_model.bvid.to_lowercase()) {
                if file_name_str.ends_with(".tmp_video")
                    || file_name_str.ends_with(".tmp_audio")
                    || file_name_str.ends_with(".mp4")
                    || file_name_str.ends_with(".mkv")
                    || file_name_str.ends_with(".flv")
                    || file_name_str.ends_with(".webm")
                    || file_name_str.ends_with(".nfo")
                    || file_name_str.ends_with(".jpg")
                    || file_name_str.ends_with(".png")
                    || file_name_str.ends_with(".ass")
                    || file_name_str.ends_with(".srt")
                {
                    debug!(
                        "✓ 通过BVID文件匹配确认为同一视频文件夹: {} (匹配文件: {})",
                        folder_path.display(),
                        file_name_str
                    );
                    return true;
                }
                found_bvid_files = true;
            }

            // 2.2 检查包含视频标题关键词的文件
            let video_title_clean = video_model
                .name
                .to_lowercase()
                .replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "");
            if !video_title_clean.is_empty() && video_title_clean.len() > 3 {
                // 提取标题的前几个字符作为关键词
                let title_keywords: Vec<&str> = video_title_clean.split_whitespace().take(3).collect();
                for keyword in title_keywords {
                    if keyword.len() > 2 && file_name_str.contains(keyword) {
                        if file_name_str.ends_with(".mp4")
                            || file_name_str.ends_with(".mkv")
                            || file_name_str.ends_with(".flv")
                            || file_name_str.ends_with(".webm")
                            || file_name_str.ends_with(".nfo")
                        {
                            debug!(
                                "✓ 通过标题关键词匹配确认为同一视频文件夹: {} (匹配关键词: {}, 文件: {})",
                                folder_path.display(),
                                keyword,
                                file_name_str
                            );
                            return true;
                        }
                        found_title_files = true;
                    }
                }
            }

            // 2.3 检查是否有媒体相关文件（降低要求）
            if file_name_str.ends_with(".mp4")
                || file_name_str.ends_with(".mkv")
                || file_name_str.ends_with(".flv")
                || file_name_str.ends_with(".webm")
                || file_name_str.ends_with(".nfo")
                || file_name_str.ends_with(".jpg")
                || file_name_str.ends_with(".png")
                || file_name_str.ends_with(".ass")
                || file_name_str.ends_with(".srt")
            {
                found_media_files = true;
            }
        }

        debug!(
            "文件检测结果: 媒体文件={}, BVID文件={}, 标题文件={}",
            found_media_files, found_bvid_files, found_title_files
        );

        // 如果找到了相关文件但没有精确匹配，记录为可疑
        if found_media_files || found_bvid_files || found_title_files {
            debug!("⚠ 文件夹包含相关文件但无法确认为同一视频文件夹: {:?}", folder_path);
        }
    }

    debug!("✗ 无法确认为同一视频文件夹: {:?}", folder_path);
    debug!("=== 智能冲突检测结束 ===");
    false
}

/// 生成唯一的文件夹名称，避免同名冲突（增强版）
pub fn generate_unique_folder_name(
    parent_dir: &std::path::Path,
    base_name: &str,
    video_model: &video::Model,
    pubtime: &str,
) -> String {
    let mut unique_name = base_name.to_string();
    let mut counter = 0;

    // 检查基础名称是否已存在
    let base_path = parent_dir.join(&unique_name);
    if !base_path.exists() {
        return unique_name;
    }

    // 如果存在，智能检查这个文件夹是否就是当前视频的文件夹
    if is_same_video_folder(&base_path, video_model) {
        debug!("文件夹 {:?} 已是当前视频的文件夹，无需生成新名称", base_path);
        return unique_name;
    }

    // 确认是真正的冲突，开始生成唯一名称
    debug!("检测到真实的文件夹名冲突，开始生成唯一名称: {}", base_name);

    // 如果存在真正冲突，先尝试追加发布时间
    unique_name = format!("{}-{}", base_name, pubtime);
    let time_path = parent_dir.join(&unique_name);
    if !time_path.exists() {
        info!("检测到下载文件夹名冲突，追加发布时间: {} -> {}", base_name, unique_name);
        return unique_name;
    }

    // 如果发布时间也冲突，追加BVID
    unique_name = format!("{}-{}", base_name, &video_model.bvid);
    let bvid_path = parent_dir.join(&unique_name);
    if !bvid_path.exists() {
        info!("检测到下载文件夹名冲突，追加BVID: {} -> {}", base_name, unique_name);
        return unique_name;
    }

    // 如果都冲突，使用数字后缀
    loop {
        counter += 1;
        unique_name = format!("{}-{}", base_name, counter);
        let numbered_path = parent_dir.join(&unique_name);
        if !numbered_path.exists() {
            warn!(
                "检测到严重下载文件夹名冲突，使用数字后缀: {} -> {}",
                base_name, unique_name
            );
            return unique_name;
        }

        // 防止无限循环，使用随机后缀
        if counter > 1000 {
            warn!("下载文件夹名冲突解决失败，使用随机后缀");
            let random_suffix: u32 = rand::random::<u32>() % 90000 + 10000;
            unique_name = format!("{}-{}", base_name, random_suffix);
            return unique_name;
        }
    }
}

#[cfg(test)]
mod tests {
    use handlebars::handlebars_helper;
    use serde_json::json;

    use crate::config::PathSafeTemplate;

    #[test]
    fn test_template_usage() {
        let mut template = handlebars::Handlebars::new();
        handlebars_helper!(truncate: |s: String, len: usize| {
            if s.chars().count() > len {
                s.chars().take(len).collect::<String>()
            } else {
                s.to_string()
            }
        });
        template.register_helper("truncate", Box::new(truncate));
        let _ = template.path_safe_register("video", "test{{bvid}}test");
        let _ = template.path_safe_register("test_truncate", "哈哈，{{ truncate title 30 }}");
        let _ = template.path_safe_register("test_path_unix", "{{ truncate title 7 }}/test/a");
        let _ = template.path_safe_register("test_path_windows", r"{{ truncate title 7 }}\\test\\a");
        #[cfg(not(windows))]
        {
            assert_eq!(
                template
                    .path_safe_render("test_path_unix", &json!({"title": "关注/永雏塔菲喵"}))
                    .unwrap(),
                "关注_永雏塔菲/test/a"
            );
            assert_eq!(
                template
                    .path_safe_render("test_path_windows", &json!({"title": "关注/永雏塔菲喵"}))
                    .unwrap(),
                "关注_永雏塔菲_test_a"
            );
        }
        #[cfg(windows)]
        {
            assert_eq!(
                template
                    .path_safe_render("test_path_unix", &json!({"title": "关注/永雏塔菲喵"}))
                    .unwrap(),
                "关注_永雏塔菲_test_a"
            );
            assert_eq!(
                template
                    .path_safe_render("test_path_windows", &json!({"title": "关注/永雏塔菲喵"}))
                    .unwrap(),
                "关注_永雏塔菲\\test\\a"
            );
        }
        assert_eq!(
            template
                .path_safe_render("video", &json!({"bvid": "BV1b5411h7g7"}))
                .unwrap(),
            "testBV1b5411h7g7test"
        );
        assert_eq!(
            template
                .path_safe_render(
                    "test_truncate",
                    &json!({"title": "你说得对，但是 Rust 是由 Mozilla 自主研发的一款全新的编译期格斗游戏。\
                    编译将发生在一个被称作「Cargo」的构建系统中。在这里，被引用的指针将被授予「生命周期」之力，导引对象安全。\
                    你将扮演一位名为「Rustacean」的神秘角色, 在与「Rustc」的搏斗中邂逅各种骨骼惊奇的傲娇报错。\
                    征服她们、通过编译同时，逐步发掘「C++」程序崩溃的真相。"})
                )
                .unwrap(),
            "哈哈，你说得对，但是 Rust 是由 Mozilla 自主研发的一"
        );
    }

    // 旧的87007/87008错误检测测试已清理，现在使用革命性的upower字段检测
}
