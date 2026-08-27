use anyhow::{anyhow, Context, Result};
use bili_sync_entity::*;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use once_cell::sync::Lazy;
use sea_orm::entity::prelude::*;
use sea_orm::sea_query::{OnConflict, SimpleExpr};
use sea_orm::DatabaseTransaction;
use sea_orm::{QueryOrder, QuerySelect, Set, Unchanged};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{oneshot, Mutex as AsyncMutex, Notify};
use tracing::{debug, info, warn};

use crate::adapter::{VideoSource, VideoSourceEnum};
use crate::bilibili::{BiliClient, Collection, CollectionItem, CollectionType, PageInfo, Video, VideoInfo};
use crate::utils::live_updates::notify_videos_changed;
use crate::utils::status::STATUS_COMPLETED;

/// 从 VideoInfo 中提取 BVID
fn extract_bvid(video_info: &VideoInfo) -> String {
    match video_info {
        VideoInfo::Submission { bvid, .. } => bvid.clone(),
        VideoInfo::Dynamic { bvid, .. } => bvid.clone(),
        VideoInfo::Detail { bvid, .. } => bvid.clone(),
        VideoInfo::Favorite { bvid, .. } => bvid.clone(),
        VideoInfo::WatchLater { bvid, .. } => bvid.clone(),
        VideoInfo::Collection { bvid, .. } => bvid.clone(),
        VideoInfo::Bangumi { bvid, .. } => bvid.clone(),
    }
}

/// 从 VideoInfo 中提取标题
fn extract_title(video_info: &VideoInfo) -> String {
    match video_info {
        VideoInfo::Submission { title, .. } => title.clone(),
        VideoInfo::Dynamic { title, .. } => title.clone(),
        VideoInfo::Detail { title, .. } => title.clone(),
        VideoInfo::Favorite { title, .. } => title.clone(),
        VideoInfo::WatchLater { title, .. } => title.clone(),
        VideoInfo::Collection { title, .. } => title.clone(),
        VideoInfo::Bangumi { title, .. } => title.clone(),
    }
}

/// 从 VideoInfo 中提取发布时间
fn extract_pubtime(video_info: &VideoInfo) -> DateTime<Utc> {
    match video_info {
        VideoInfo::Submission { ctime, .. } => *ctime,
        VideoInfo::Dynamic { pubtime, .. } => *pubtime,
        VideoInfo::Detail { pubtime, .. } => *pubtime,
        VideoInfo::Favorite { pubtime, .. } => *pubtime,
        VideoInfo::WatchLater { pubtime, .. } => *pubtime,
        VideoInfo::Collection { pubtime, .. } => *pubtime,
        VideoInfo::Bangumi { pubtime, .. } => *pubtime,
    }
}

/// 从 VideoInfo 中提取时长（秒）
fn extract_duration_seconds(video_info: &VideoInfo) -> Option<i32> {
    match video_info {
        VideoInfo::Submission { duration, .. } => *duration,
        VideoInfo::Dynamic { duration, .. } => *duration,
        VideoInfo::Detail { duration, .. } => *duration,
        VideoInfo::Favorite { duration, .. } => *duration,
        VideoInfo::WatchLater { duration, .. } => *duration,
        VideoInfo::Collection { duration, arc, .. } => duration.or_else(|| {
            arc.as_ref().and_then(|arc| {
                arc.get("duration")
                    .and_then(|value| value.as_i64().map(|v| v as i32))
                    .or_else(|| {
                        arc.get("arc")
                            .and_then(|nested| nested.get("duration"))
                            .and_then(|value| value.as_i64().map(|v| v as i32))
                    })
            })
        }),
        VideoInfo::Bangumi { duration, .. } => *duration,
    }
}

fn video_info_is_valid(video_info: &VideoInfo) -> bool {
    match video_info {
        VideoInfo::Favorite { attr, .. } => *attr == 0 || *attr == 4,
        VideoInfo::WatchLater { state, .. } | VideoInfo::Detail { state, .. } => *state == 0,
        _ => true,
    }
}

fn is_unidentified_invalid_video(video_info: &VideoInfo) -> bool {
    extract_bvid(video_info).trim().is_empty() && !video_info_is_valid(video_info)
}

fn model_marks_video_invalid(model: &video::ActiveModel) -> bool {
    matches!(model.valid, sea_orm::ActiveValue::Set(false))
}

fn metadata_updates_for_list_item(
    existing: &video::Model,
    model: &video::ActiveModel,
    new_name: sea_orm::ActiveValue<String>,
) -> (
    sea_orm::ActiveValue<String>,
    sea_orm::ActiveValue<String>,
    sea_orm::ActiveValue<String>,
) {
    if model_marks_video_invalid(model) {
        debug!(
            "B站列表返回失效占位，保留本地已有标题和封面: video_id={}, name={}, bvid={}",
            existing.id, existing.name, existing.bvid
        );
        (
            sea_orm::ActiveValue::NotSet,
            sea_orm::ActiveValue::NotSet,
            sea_orm::ActiveValue::NotSet,
        )
    } else {
        (new_name, model.intro.clone(), model.cover.clone())
    }
}

/// 根据show_season_type和其他字段重新计算番剧的智能命名
fn recalculate_bangumi_name(
    title: &str,
    share_copy: Option<&str>,
    show_title: Option<&str>,
    show_season_type: Option<i32>,
) -> String {
    // 参考convert.rs中的智能命名逻辑
    if show_season_type == Some(2) {
        // 番剧影视类型，使用简化命名
        show_title.unwrap_or(title).to_string()
    } else {
        // 常规番剧类型，使用详细命名
        share_copy
            .filter(|s| !s.is_empty() && s.len() > title.len()) // 只有当share_copy更详细时才使用
            .or(show_title)
            .unwrap_or(title)
            .to_string()
    }
}

/// 筛选未填充的视频
pub async fn filter_unfilled_videos(
    additional_expr: SimpleExpr,
    conn: &DatabaseConnection,
) -> Result<Vec<video::Model>> {
    video::Entity::find()
        .filter(
            video::Column::Valid
                .eq(true)
                .and(video::Column::DownloadStatus.eq(0))
                .and(video::Column::Category.is_in([1, 2]))
                .and(video::Column::SinglePage.is_null())
                .and(video::Column::Deleted.eq(0))
                .and(video::Column::AutoDownload.eq(true))  // 只处理设置为自动下载的视频
                .and(additional_expr),
        )
        .all(conn)
        .await
        .context("filter unfilled videos failed")
}

/// 筛选未处理完成的视频和视频页
pub async fn filter_unhandled_video_pages(
    additional_expr: SimpleExpr,
    connection: &DatabaseConnection,
) -> Result<Vec<(video::Model, Vec<page::Model>)>> {
    video::Entity::find()
        .filter(
            video::Column::Valid
                .eq(true)
                .and(video::Column::DownloadStatus.lt(STATUS_COMPLETED))
                .and(video::Column::Category.is_in([1, 2]))
                .and(video::Column::SinglePage.is_not_null())
                .and(video::Column::Deleted.eq(0))
                .and(video::Column::AutoDownload.eq(true))  // 只处理设置为自动下载的视频
                .and(additional_expr),
        )
        .find_with_related(page::Entity)
        .all(connection)
        .await
        .context("filter unhandled video pages failed")
}

/// 筛选在当前循环中失败但可重试的视频（不包括已达到最大重试次数的视频）
pub async fn get_failed_videos_in_current_cycle(
    additional_expr: SimpleExpr,
    connection: &DatabaseConnection,
) -> Result<Vec<(video::Model, Vec<page::Model>)>> {
    use crate::utils::status::STATUS_COMPLETED;

    let all_videos = video::Entity::find()
        .filter(
            video::Column::Valid
                .eq(true)
                .and(video::Column::DownloadStatus.lt(STATUS_COMPLETED))
                .and(video::Column::DownloadStatus.gt(0)) // 排除未开始的视频 (状态为0)
                .and(video::Column::Category.is_in([1, 2]))
                .and(video::Column::SinglePage.is_not_null())
                .and(video::Column::Deleted.eq(0))
                .and(video::Column::AutoDownload.eq(true))  // 只处理设置为自动下载的视频
                .and(additional_expr),
        )
        .find_with_related(page::Entity)
        .all(connection)
        .await?;

    // 获取所有待处理的删除任务中的视频ID
    use crate::task::DeleteVideoTask;
    use bili_sync_entity::task_queue::{self, TaskStatus, TaskType};

    let pending_delete_tasks = task_queue::Entity::find()
        .filter(task_queue::Column::TaskType.eq(TaskType::DeleteVideo))
        .filter(task_queue::Column::Status.eq(TaskStatus::Pending))
        .all(connection)
        .await?;

    let mut videos_in_delete_queue = std::collections::HashSet::new();
    for task_record in pending_delete_tasks {
        if let Ok(task_data) = serde_json::from_str::<DeleteVideoTask>(&task_record.task_data) {
            videos_in_delete_queue.insert(task_data.video_id);
        }
    }

    let result = all_videos
        .into_iter()
        .filter(|(video_model, pages_model)| {
            // 如果视频已经在删除队列中，跳过重试
            if videos_in_delete_queue.contains(&video_model.id) {
                return false;
            }

            // 检查视频和分页是否有可重试的失败
            let video_status = crate::utils::status::VideoStatus::from(video_model.download_status);
            let video_should_retry = video_status.should_run().iter().any(|&should_run| should_run);

            let pages_should_retry = pages_model.iter().any(|page_model| {
                let page_status = crate::utils::status::PageStatus::from(page_model.download_status);
                page_status.should_run().iter().any(|&should_run| should_run)
            });

            video_should_retry || pages_should_retry
        })
        .collect::<Vec<_>>();

    Ok(result)
}

/// 尝试创建 Video Model，如果发生冲突则忽略
/// 如果视频源启用了扫描已删除视频设置，则会恢复已删除的视频
/// 对于选择性下载模式，只存储选中的视频到数据库
pub async fn create_videos(
    videos_info: Vec<VideoInfo>,
    video_source: &VideoSourceEnum,
    connection: &DatabaseConnection,
) -> Result<()> {
    use sea_orm::{Set, Unchanged};

    // 新增：在全量模式下进行去重检查，防止重复处理已存在的视频
    let current_config = crate::config::reload_config();
    let is_full_mode = !current_config.submission_risk_control.enable_incremental_fetch;

    let final_videos_info = if is_full_mode && matches!(video_source, VideoSourceEnum::Submission(_)) {
        // 全量模式下的 UP主投稿，检查哪些视频已存在
        let all_bvids: Vec<String> = videos_info.iter().map(extract_bvid).collect();

        // 批量查询已存在的视频
        let existing_videos = video::Entity::find()
            .filter(video::Column::Bvid.is_in(all_bvids.clone()))
            .filter(video_source.filter_expr())
            .all(connection)
            .await?;

        let existing_bvids: HashSet<String> = existing_videos.into_iter().map(|v| v.bvid).collect();

        // 过滤出真正的新视频
        let new_videos: Vec<VideoInfo> = videos_info
            .into_iter()
            .filter(|info| !existing_bvids.contains(&extract_bvid(info)))
            .collect();

        let total_count = all_bvids.len();
        let existing_count = existing_bvids.len();
        let new_count = new_videos.len();

        if existing_count > 0 {
            info!(
                "全量模式去重检查完成：总视频 {} 个，已存在 {} 个，新视频 {} 个",
                total_count, existing_count, new_count
            );
        } else {
            debug!("全量模式：所有 {} 个视频都是新视频", new_count);
        }

        new_videos
    } else {
        // 增量模式或其他类型的视频源，使用原有逻辑
        videos_info
    };

    // 关键词过滤：支持双列表模式（黑白名单同时生效）
    // 白名单：如果设置了白名单，视频必须匹配其中之一才下载
    // 黑名单：匹配黑名单的视频即使通过白名单也不下载
    let blacklist_keywords = video_source.get_blacklist_keywords();
    let whitelist_keywords = video_source.get_whitelist_keywords();
    let case_sensitive = video_source.get_keyword_case_sensitive();
    let min_duration_seconds = video_source.get_min_duration_seconds();
    let max_duration_seconds = video_source.get_max_duration_seconds();
    let published_after = video_source.get_published_after();
    let published_before = video_source.get_published_before();
    // 向后兼容：旧的单列表模式
    let keyword_filters = video_source.get_keyword_filters();
    let keyword_filter_mode = video_source.get_keyword_filter_mode();

    // 判断是否有任何过滤配置
    let has_dual_list = blacklist_keywords.is_some() || whitelist_keywords.is_some();
    let has_legacy = keyword_filters.is_some();
    let has_duration_filter = min_duration_seconds.is_some() || max_duration_seconds.is_some();
    let has_published_filter = published_after.is_some() || published_before.is_some();
    let published_after_date = published_after
        .as_deref()
        .and_then(|date| chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok());
    let published_before_date = published_before
        .as_deref()
        .and_then(|date| chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok());

    let final_videos_info = if has_dual_list || has_legacy || has_duration_filter || has_published_filter {
        use crate::utils::keyword_filter::{should_filter_video_dual_list, should_filter_video_with_mode};

        let before_count = final_videos_info.len();
        let filtered_videos: Vec<VideoInfo> = final_videos_info
            .into_iter()
            .filter(|info| {
                let title = extract_title(info);
                let bvid = extract_bvid(info);

                // 优先使用新的双列表模式
                let mut should_filter = if has_dual_list {
                    should_filter_video_dual_list(&title, &blacklist_keywords, &whitelist_keywords, case_sensitive)
                } else {
                    // 向后兼容：使用旧的单列表模式
                    should_filter_video_with_mode(&title, &keyword_filters, &keyword_filter_mode)
                };

                if !should_filter && has_duration_filter {
                    let duration_seconds = extract_duration_seconds(info);
                    if let Some(duration_seconds) = duration_seconds {
                        if min_duration_seconds.map(|min| duration_seconds < min).unwrap_or(false)
                            || max_duration_seconds.map(|max| duration_seconds > max).unwrap_or(false)
                        {
                            info!(
                                "视频 '{}' 时长 {} 秒不在过滤范围内，跳过: {}",
                                title, duration_seconds, bvid
                            );
                            should_filter = true;
                        }
                    }
                }

                if !should_filter && has_published_filter {
                    let beijing_tz = crate::utils::time_format::beijing_timezone();
                    let published_at = extract_pubtime(info).with_timezone(&beijing_tz);
                    let published_date = published_at.date_naive();
                    let published_time = published_at.format("%Y%m%d%H%M%S").to_string();
                    if published_after_date
                        .map(|start| published_date < start)
                        .unwrap_or(false)
                        || published_before_date.map(|end| published_date > end).unwrap_or(false)
                    {
                        info!(
                            "视频 '{}' 发布时间 {} 不在过滤范围内，跳过: {}",
                            title, published_time, bvid
                        );
                        should_filter = true;
                    }
                }

                if should_filter && (has_dual_list || has_legacy) {
                    info!("视频 '{}' 被关键词过滤器过滤，跳过: {}", title, bvid);
                }
                !should_filter
            })
            .collect();

        let filtered_count = before_count - filtered_videos.len();
        if filtered_count > 0 {
            info!(
                "视频过滤完成：原视频 {} 个，过滤 {} 个，剩余 {} 个",
                before_count,
                filtered_count,
                filtered_videos.len()
            );
        }

        filtered_videos
    } else {
        final_videos_info
    };

    // 如果没有新视频需要处理，直接返回
    if final_videos_info.is_empty() {
        debug!("没有新视频需要创建，跳过处理");
        return Ok(());
    }

    // 检查是否启用了扫描已删除视频
    let scan_deleted = video_source.scan_deleted_videos();

    if scan_deleted {
        // 启用扫描已删除视频：需要特别处理已删除的视频
        for video_info in final_videos_info {
            if is_unidentified_invalid_video(&video_info) {
                warn!(
                    "跳过B站返回的失效占位视频：{}「{}」未提供BVID，无法可靠关联本地记录",
                    video_source.source_type_display(),
                    video_source.source_name_display()
                );
                continue;
            }

            // 选择性下载逻辑：针对 submission 类型视频源 - 需要在 into_simple_model() 之前获取信息
            let should_store_video = if let Some(selected_videos) = video_source.get_selected_videos() {
                // 获取创建时间来判断是否为新投稿
                let is_new_submission = if let Some(created_at) = video_source.get_created_at() {
                    // 如果视频发布时间晚于订阅创建时间，则为新投稿，自动下载
                    let beijing_tz = crate::utils::time_format::beijing_timezone();
                    video_info.release_datetime().with_timezone(&beijing_tz).naive_local() > created_at
                } else {
                    // 如果无法获取创建时间，保守地认为不是新投稿
                    false
                };

                // 获取视频的 BVID（从 VideoInfo 获取）
                let video_bvid = extract_bvid(&video_info);

                let should_store = if is_new_submission {
                    // 新投稿：存储到数据库并设置自动下载
                    true
                } else {
                    // 历史投稿：只有在选择列表中的才存储到数据库
                    selected_videos.contains(&video_bvid)
                };

                debug!(
                    "选择性下载检查(已删除扫描): BVID={}, 是否新投稿={}, 是否在选择列表中={}, 是否存储={}",
                    video_bvid,
                    is_new_submission,
                    selected_videos.contains(&video_bvid),
                    should_store
                );

                should_store
            } else {
                // 没有选择性下载，存储所有视频
                true
            };

            // 如果不应该存储此视频，则跳过
            if !should_store_video {
                continue;
            }

            let mut model = video_info.into_simple_model();
            video_source.set_relation_id(&mut model);

            // 对于需要存储的视频，设置 auto_download 为 true
            model.auto_download = Set(true);

            // 查找是否存在已删除的同一视频
            let existing_video = video::Entity::find()
                .filter(video::Column::Bvid.eq(model.bvid.as_ref()))
                .filter(video_source.filter_expr())
                .one(connection)
                .await?;

            if let Some(existing) = existing_video {
                if existing.deleted == 1 {
                    // 存在已删除的视频，恢复它并重置下载状态以强制重新下载
                    let update_model = video::ActiveModel {
                        id: Unchanged(existing.id),
                        deleted: Set(0),
                        download_status: Set(0),   // 重置下载状态为未开始，强制重新下载
                        path: Set("".to_string()), // 清空原有路径，因为文件可能已经不存在
                        single_page: Set(None),    // 设为NULL，让filter_unfilled_videos识别并重新获取完整信息
                        // 更新其他可能变化的字段
                        name: model.name.clone(),
                        intro: model.intro.clone(),
                        cover: model.cover.clone(),
                        tags: model.tags.clone(),
                        ..Default::default()
                    };
                    crate::database::run_traced_db_operation(
                        format!("utils.model.restore_deleted_video(video_id={})", existing.id),
                        async {
                            update_model.save(connection).await?;
                            // 恢复后确保参与自动下载流程
                            video::Entity::update(video::ActiveModel {
                                id: Unchanged(existing.id),
                                auto_download: Set(true),
                                ..Default::default()
                            })
                            .exec(connection)
                            .await?;

                            // 删除该视频的所有旧page记录（如果存在的话）
                            // 因为视频信息可能已经变化，旧的page记录可能不准确
                            page::Entity::delete_many()
                                .filter(page::Column::VideoId.eq(existing.id))
                                .exec(connection)
                                .await
                        },
                    )
                    .await?;

                    info!("恢复已删除的视频，将重新获取详细信息: {}", existing.name);
                } else {
                    // 视频存在且未删除，检查是否需要更新字段
                    let mut needs_update = false;
                    let mut should_recalculate_name = false;
                    let valid_changed = match &model.valid {
                        Set(new_valid) => existing.valid != *new_valid,
                        _ => false,
                    };

                    // 检查 share_copy 字段更新
                    let share_copy_changed = if let Some(new_share_copy) = model.share_copy.as_ref() {
                        existing.share_copy.as_ref() != Some(new_share_copy)
                    } else {
                        false
                    };

                    // 检查 show_season_type 字段更新
                    let show_season_type_changed = if let Some(new_show_season_type) = model.show_season_type.as_ref() {
                        existing.show_season_type != Some(*new_show_season_type)
                    } else {
                        false
                    };

                    // 检查 actors 字段更新
                    let actors_changed = match (&existing.actors, model.actors.as_ref()) {
                        (None, Some(new_actors)) => {
                            // 数据库为空，API有数据，需要更新
                            tracing::info!("检测到actors字段从空值更新为有值: {:?}", new_actors);
                            true
                        }
                        (Some(existing_actors), Some(new_actors)) => {
                            // 两者都有值，比较是否不同
                            let changed = existing_actors != new_actors;
                            if changed {
                                tracing::info!(
                                    "检测到actors字段值发生变化: 原值={:?}, 新值={:?}",
                                    existing_actors,
                                    new_actors
                                );
                            }
                            changed
                        }
                        (Some(_), None) => {
                            // 数据库有值，API返回空，保持原值不变
                            tracing::debug!("API未返回actors数据，保持数据库现有值");
                            false
                        }
                        (None, None) => {
                            // 两者都为空，无需更新
                            false
                        }
                    };

                    if share_copy_changed || show_season_type_changed || actors_changed || valid_changed {
                        needs_update = true;
                        should_recalculate_name = true;

                        if share_copy_changed {
                            info!(
                                "检测到需要更新share_copy: 视频={}, 原值={:?}, 新值={:?}",
                                existing.name, existing.share_copy, model.share_copy
                            );
                        }
                        if show_season_type_changed {
                            info!(
                                "检测到需要更新show_season_type: 视频={}, 原值={:?}, 新值={:?}",
                                existing.name, existing.show_season_type, model.show_season_type
                            );
                        }
                        if actors_changed {
                            info!(
                                "检测到需要更新actors: 视频={}, 原值={:?}, 新值={:?}",
                                existing.name, existing.actors, model.actors
                            );
                        }
                        if valid_changed {
                            info!(
                                "检测到需要更新valid: 视频={}, 原值={}, 新值={:?}",
                                existing.name, existing.valid, model.valid
                            );
                        }
                    }

                    if needs_update {
                        // 如果需要重新计算name，并且这是番剧类型（category=1）
                        // 但对于番剧影视类型（show_season_type=2），不重新计算name，保持原有的简洁格式
                        let new_name = if should_recalculate_name && existing.category == 1 {
                            let new_show_season_type = match &model.show_season_type {
                                Set(opt) => *opt,
                                _ => existing.show_season_type,
                            };

                            // 如果是番剧影视类型，不重新计算name，保持现有的简洁name
                            if new_show_season_type == Some(2) {
                                sea_orm::ActiveValue::NotSet // 保持现有name不变
                            } else {
                                // 对于常规番剧类型，进行重新计算
                                let title = existing.name.as_str();
                                let share_copy = match &model.share_copy {
                                    Set(Some(s)) => Some(s.as_str()),
                                    Set(None) => None,
                                    _ => existing.share_copy.as_deref(),
                                };

                                let recalculated_name =
                                    recalculate_bangumi_name(title, share_copy, None, new_show_season_type);
                                info!(
                                    "重新计算常规番剧name: 视频={}, 原name={}, 新name={}",
                                    existing.name, existing.name, recalculated_name
                                );
                                Set(recalculated_name)
                            }
                        } else {
                            model.name.clone()
                        };
                        let (name_update, intro_update, cover_update) =
                            metadata_updates_for_list_item(&existing, &model, new_name);

                        let update_model = video::ActiveModel {
                            id: Unchanged(existing.id),
                            valid: model.valid.clone(),
                            share_copy: model.share_copy.clone(),
                            show_season_type: model.show_season_type.clone(),
                            actors: model.actors.clone(),
                            name: name_update,
                            intro: intro_update,
                            cover: cover_update,
                            ..Default::default()
                        };

                        // 详细的数据库更新调试日志
                        tracing::info!(
                            "即将执行数据库更新(启用扫描删除): 视频={}, actors字段={:?}, share_copy={:?}, show_season_type={:?}",
                            existing.name, update_model.actors, update_model.share_copy, update_model.show_season_type
                        );

                        crate::database::run_traced_db_operation(
                            format!(
                                "utils.model.update_video_metadata(video_id={}, scan_deleted=true)",
                                existing.id
                            ),
                            async { update_model.save(connection).await },
                        )
                        .await?;
                        info!("更新视频 {} 的字段完成", existing.name);
                    } else {
                        tracing::debug!(
                            "字段无需更新: 视频={}, share_copy={:?}, show_season_type={:?}, actors={:?}",
                            existing.name,
                            existing.share_copy,
                            existing.show_season_type,
                            existing.actors
                        );
                    }
                    continue;
                }
            } else {
                // 视频不存在，正常插入
                crate::database::run_traced_db_operation("utils.model.insert_video(scan_deleted=true)", async {
                    video::Entity::insert(model)
                        .on_conflict(OnConflict::new().do_nothing().to_owned())
                        .do_nothing()
                        .exec(connection)
                        .await
                })
                .await?;
            }
        }
    } else {
        // 未启用扫描已删除视频：使用原有逻辑，但增加 share_copy 更新检查
        for video_info in final_videos_info {
            if is_unidentified_invalid_video(&video_info) {
                warn!(
                    "跳过B站返回的失效占位视频：{}「{}」未提供BVID，无法可靠关联本地记录",
                    video_source.source_type_display(),
                    video_source.source_name_display()
                );
                continue;
            }

            // 选择性下载逻辑：针对 submission 类型视频源 - 需要在 into_simple_model() 之前获取信息
            let should_store_video = if let Some(selected_videos) = video_source.get_selected_videos() {
                // 获取创建时间来判断是否为新投稿
                let is_new_submission = if let Some(created_at) = video_source.get_created_at() {
                    // 如果视频发布时间晚于订阅创建时间，则为新投稿，自动下载
                    let beijing_tz = crate::utils::time_format::beijing_timezone();
                    video_info.release_datetime().with_timezone(&beijing_tz).naive_local() > created_at
                } else {
                    // 如果无法获取创建时间，保守地认为不是新投稿
                    false
                };

                // 获取视频的 BVID（从 VideoInfo 获取）
                let video_bvid = extract_bvid(&video_info);

                let should_store = if is_new_submission {
                    // 新投稿：存储到数据库并设置自动下载
                    true
                } else {
                    // 历史投稿：只有在选择列表中的才存储到数据库
                    selected_videos.contains(&video_bvid)
                };

                debug!(
                    "选择性下载检查(常规模式): BVID={}, 是否新投稿={}, 是否在选择列表中={}, 是否存储={}",
                    video_bvid,
                    is_new_submission,
                    selected_videos.contains(&video_bvid),
                    should_store
                );

                should_store
            } else {
                // 没有选择性下载，存储所有视频
                true
            };

            // 如果不应该存储此视频，则跳过
            if !should_store_video {
                continue;
            }

            let mut model = video_info.into_simple_model();
            video_source.set_relation_id(&mut model);

            // 对于需要存储的视频，设置 auto_download 为 true
            model.auto_download = Set(true);

            // 检查是否是番剧类型（source_type = 1）且有 ep_id
            let is_bangumi_with_ep_id =
                matches!(model.source_type, Set(Some(1))) && matches!(model.ep_id, Set(Some(_)));

            // 对于番剧类型，先检查是否已存在相同 bvid + ep_id 的记录
            let existing_check = if is_bangumi_with_ep_id {
                debug!(
                    "番剧视频插入检查: bvid={}, ep_id={:?}",
                    model.bvid.as_ref(),
                    model.ep_id.as_ref()
                );

                let mut query = video::Entity::find()
                    .filter(video::Column::Bvid.eq(model.bvid.as_ref()))
                    .filter(video_source.filter_expr());

                if let Set(Some(ep_id)) = &model.ep_id {
                    query = query.filter(video::Column::EpId.eq(ep_id));
                    debug!("查询番剧记录: bvid={}, ep_id={}", model.bvid.as_ref(), ep_id);
                }

                let result = query.one(connection).await?;
                debug!("番剧查询结果: existing={}", result.is_some());
                result
            } else {
                None
            };

            let insert_result = if existing_check.is_some() {
                // 已存在相同记录，模拟冲突结果
                Ok(sea_orm::TryInsertResult::Conflicted)
            } else {
                // 尝试插入新记录
                crate::database::run_traced_db_operation("utils.model.insert_video(scan_deleted=false)", async {
                    video::Entity::insert(model.clone())
                        .on_conflict(OnConflict::new().do_nothing().to_owned())
                        .do_nothing()
                        .exec(connection)
                        .await
                })
                .await
            };

            // 如果插入没有影响任何行（即记录已存在），检查是否需要更新 share_copy
            if let Ok(insert_res) = insert_result {
                // 检查插入是否真的生效，如果没有生效说明记录已存在
                let insert_success = match &insert_res {
                    sea_orm::TryInsertResult::Inserted(_) => true,
                    sea_orm::TryInsertResult::Conflicted => false,
                    sea_orm::TryInsertResult::Empty => true, // 空插入视为成功
                };
                if !insert_success {
                    // 记录已存在，检查是否需要更新字段
                    let existing_video = if let Some(existing) = existing_check {
                        // 如果之前的检查中已经找到了记录，直接使用
                        Some(existing)
                    } else {
                        // 否则重新查询（适用于非番剧类型或其他情况）
                        let mut query = video::Entity::find()
                            .filter(video::Column::Bvid.eq(model.bvid.as_ref()))
                            .filter(video_source.filter_expr());

                        // 对于番剧类型，还需要通过 ep_id 来精确查找
                        if is_bangumi_with_ep_id {
                            if let Set(Some(ep_id)) = &model.ep_id {
                                query = query.filter(video::Column::EpId.eq(ep_id));
                            }
                        }

                        query.one(connection).await?
                    };

                    if let Some(existing) = existing_video {
                        let mut needs_update = false;
                        let mut should_recalculate_name = false;
                        let valid_changed = match &model.valid {
                            Set(new_valid) => existing.valid != *new_valid,
                            _ => false,
                        };

                        // 检查 share_copy 字段更新
                        let share_copy_changed = if let Some(new_share_copy) = model.share_copy.as_ref() {
                            existing.share_copy.as_ref() != Some(new_share_copy)
                        } else {
                            false
                        };

                        // 检查 show_season_type 字段更新
                        let show_season_type_changed =
                            if let Some(new_show_season_type) = model.show_season_type.as_ref() {
                                existing.show_season_type != Some(*new_show_season_type)
                            } else {
                                false
                            };

                        // 检查 actors 字段更新
                        let actors_changed = match (&existing.actors, model.actors.as_ref()) {
                            (None, Some(new_actors)) => {
                                // 数据库为空，API有数据，需要更新
                                tracing::info!("检测到actors字段从空值更新为有值(未启用扫描删除): {:?}", new_actors);
                                true
                            }
                            (Some(existing_actors), Some(new_actors)) => {
                                // 两者都有值，比较是否不同
                                let changed = existing_actors != new_actors;
                                if changed {
                                    tracing::info!(
                                        "检测到actors字段值发生变化(未启用扫描删除): 原值={:?}, 新值={:?}",
                                        existing_actors,
                                        new_actors
                                    );
                                }
                                changed
                            }
                            (Some(_), None) => {
                                // 数据库有值，API返回空，保持原值不变
                                tracing::debug!("API未返回actors数据，保持数据库现有值(未启用扫描删除)");
                                false
                            }
                            (None, None) => {
                                // 两者都为空，无需更新
                                false
                            }
                        };

                        if share_copy_changed || show_season_type_changed || actors_changed || valid_changed {
                            needs_update = true;
                            should_recalculate_name = true;

                            if share_copy_changed {
                                info!(
                                    "检测到需要更新share_copy(未启用扫描删除): 视频={}, 原值={:?}, 新值={:?}",
                                    existing.name, existing.share_copy, model.share_copy
                                );
                            }
                            if show_season_type_changed {
                                info!(
                                    "检测到需要更新show_season_type(未启用扫描删除): 视频={}, 原值={:?}, 新值={:?}",
                                    existing.name, existing.show_season_type, model.show_season_type
                                );
                            }
                            if actors_changed {
                                info!(
                                    "检测到需要更新actors(未启用扫描删除): 视频={}, 原值={:?}, 新值={:?}",
                                    existing.name, existing.actors, model.actors
                                );
                            }
                            if valid_changed {
                                info!(
                                    "检测到需要更新valid(未启用扫描删除): 视频={}, 原值={}, 新值={:?}",
                                    existing.name, existing.valid, model.valid
                                );
                            }
                        }

                        if needs_update {
                            // 如果需要重新计算name，并且这是番剧类型（category=1）
                            // 但对于番剧影视类型（show_season_type=2），不重新计算name，保持原有的简洁格式
                            let new_name = if should_recalculate_name && existing.category == 1 {
                                let new_show_season_type = match &model.show_season_type {
                                    Set(opt) => *opt,
                                    _ => existing.show_season_type,
                                };

                                // 如果是番剧影视类型，不重新计算name，保持现有的简洁name
                                if new_show_season_type == Some(2) {
                                    sea_orm::ActiveValue::NotSet // 保持现有name不变
                                } else {
                                    // 对于常规番剧类型，进行重新计算
                                    let title = existing.name.as_str();
                                    let share_copy = match &model.share_copy {
                                        Set(Some(s)) => Some(s.as_str()),
                                        Set(None) => None,
                                        _ => existing.share_copy.as_deref(),
                                    };

                                    let recalculated_name =
                                        recalculate_bangumi_name(title, share_copy, None, new_show_season_type);
                                    info!(
                                        "重新计算常规番剧name(未启用扫描删除): 视频={}, 原name={}, 新name={}",
                                        existing.name, existing.name, recalculated_name
                                    );
                                    Set(recalculated_name)
                                }
                            } else {
                                model.name.clone()
                            };
                            let (name_update, intro_update, cover_update) =
                                metadata_updates_for_list_item(&existing, &model, new_name);

                            let update_model = video::ActiveModel {
                                id: Unchanged(existing.id),
                                valid: model.valid.clone(),
                                share_copy: model.share_copy.clone(),
                                show_season_type: model.show_season_type.clone(),
                                actors: model.actors.clone(),
                                name: name_update,
                                intro: intro_update,
                                cover: cover_update,
                                ..Default::default()
                            };

                            // 详细的数据库更新调试日志
                            tracing::info!(
                                "即将执行数据库更新(未启用扫描删除): 视频={}, actors字段={:?}, share_copy={:?}, show_season_type={:?}",
                                existing.name, update_model.actors, update_model.share_copy, update_model.show_season_type
                            );

                            crate::database::run_traced_db_operation(
                                format!(
                                    "utils.model.update_video_metadata(video_id={}, scan_deleted=false)",
                                    existing.id
                                ),
                                async { update_model.save(connection).await },
                            )
                            .await?;
                            info!("更新视频 {} 的字段完成(未启用扫描删除)", existing.name);
                        } else {
                            tracing::debug!(
                                "字段无需更新(未启用扫描删除): 视频={}, share_copy={:?}, show_season_type={:?}, actors={:?}",
                                existing.name,
                                existing.share_copy,
                                existing.show_season_type,
                                existing.actors
                            );
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

/// 从收藏夹扫描得到的 VideoInfo 中收集 bvid -> B站 当前分P数 的映射。
///
/// 仅统计类型为普通视频（type=2）且 B站 返回了有效分P数的条目；
/// 合集（type=21）条目已在收藏夹流中展开为每一集，不在此处理。
pub fn collect_favorite_page_counts(videos_info: &[VideoInfo]) -> HashMap<String, i32> {
    let mut map = HashMap::new();
    for info in videos_info {
        if let VideoInfo::Favorite { bvid, page, .. } = info {
            if *page > 0 && !bvid.is_empty() {
                map.insert(bvid.clone(), *page);
            }
        }
    }
    map
}

/// 收藏夹源：对比 B站 收藏夹接口返回的当前分P数与本地已存在视频的分P数，
/// 若发现 UP主 新增了分P（例如合集类视频持续更新分P），重置该视频的
/// single_page/download_status，使其重新进入“详情填充”阶段拉取最新分P列表，
/// 从而新增并下载这些分P。
pub async fn check_favorite_videos_for_new_parts(
    api_page_map: HashMap<String, i32>,
    video_source: &VideoSourceEnum,
    connection: &DatabaseConnection,
) -> Result<()> {
    let VideoSourceEnum::Favorite(favorite_source) = video_source else {
        return Ok(());
    };
    if api_page_map.is_empty() {
        return Ok(());
    }

    let bvids: Vec<String> = api_page_map.keys().cloned().collect();
    let existing = video::Entity::find()
        .filter(video::Column::Bvid.is_in(bvids))
        .filter(video_source.filter_expr())
        .find_with_related(page::Entity)
        .all(connection)
        .await
        .context("查询收藏夹视频分P数失败")?;

    let mut updated = 0usize;
    for (video_model, pages) in existing {
        if video_model.deleted != 0 || !video_model.valid {
            continue;
        }
        let Some(&api_page) = api_page_map.get(&video_model.bvid) else {
            continue;
        };
        let local_page_count = pages.len() as i32;
        // 新入库尚未填充详情的视频（本地 0 分P）不需要重置，详情填充会自动处理
        if local_page_count == 0 {
            continue;
        }
        if reset_video_if_parts_increased(
            &video_model,
            local_page_count,
            api_page,
            &favorite_source.name,
            connection,
        )
        .await?
        {
            updated += 1;
        }
    }
    if updated > 0 {
        notify_videos_changed();
    }
    Ok(())
}

/// 对比当前分P数与本地已入库分P数，发现变多则重置视频的
/// single_page/download_status，使其重新进入详情填充阶段拉取最新分P并下载。
/// 返回是否执行了重置。
async fn reset_video_if_parts_increased(
    video_model: &video::Model,
    local_page_count: i32,
    current_page_count: i32,
    favorite_name: &str,
    connection: &DatabaseConnection,
) -> Result<bool> {
    if current_page_count <= local_page_count {
        return Ok(false);
    }
    info!(
        "收藏夹「{}」视频 {} ({}) 检测到新增分P（本地 {} -> B站 {}），重新拉取详情并下载新分P",
        favorite_name,
        video_model.name,
        video_model.bvid,
        local_page_count,
        current_page_count
    );
    let update = video::ActiveModel {
        id: Unchanged(video_model.id),
        single_page: Set(None),
        download_status: Set(0),
        valid: Set(true),
        ..Default::default()
    };
    update.save(connection).await?;
    Ok(true)
}

/// 收藏夹源：每轮限量巡检已入库的多P视频是否新增了分P。
///
/// 增量扫描只会看到「新收藏」的条目，而多P视频新增分P不会改变收藏时间
/// （fav_time），旧收藏里持续更新的合集类视频永远进不了增量窗口。
/// 这里直接查库找出该收藏夹下已填充详情的多P视频，每轮轮转抽查一部分
/// （默认每轮 10 个），用 view 接口对比当前分P数与本地记录，发现新增就
/// 重置视频以重新拉取详情并下载新分P。返回本轮重置的视频数量。
pub async fn check_favorite_multipage_videos_for_new_parts(
    bili_client: &BiliClient,
    video_source: &VideoSourceEnum,
    connection: &DatabaseConnection,
) -> Result<usize> {
    let VideoSourceEnum::Favorite(favorite_source) = video_source else {
        return Ok(0);
    };

    // 每轮最多巡检的多P视频数量，控制对 B站 的请求量
    const BATCH: usize = 10;

    let mut videos = video::Entity::find()
        .filter(video_source.filter_expr())
        .filter(video::Column::SinglePage.eq(false))
        .filter(video::Column::Valid.eq(true))
        .filter(video::Column::Deleted.eq(0))
        .filter(video::Column::AutoDownload.eq(true))
        .order_by_asc(video::Column::Id)
        .all(connection)
        .await
        .context("查询收藏夹多P视频失败")?;

    if videos.is_empty() {
        return Ok(0);
    }

    // 每个收藏夹源各自记录上次巡检位置，每轮从不同位置取一批，循环覆盖全部多P视频
    let offset = {
        let mut offsets = FAVORITE_MULTIPAGE_CHECK_OFFSETS.lock().await;
        let entry = offsets.entry(favorite_source.id).or_insert(0);
        let current = *entry;
        *entry = (current + BATCH) % videos.len();
        current
    };
    videos.rotate_left(offset);

    let mut updated = 0usize;
    for video_model in videos.into_iter().take(BATCH) {
        let local_page_count = page::Entity::find()
            .filter(page::Column::VideoId.eq(video_model.id))
            .count(connection)
            .await
            .context("查询视频分P数失败")? as i32;

        let current_page_count = match Video::new(bili_client, video_model.bvid.clone()).get_view_info().await {
            Ok(VideoInfo::Detail { pages, .. }) => pages.len() as i32,
            // 特殊状态视频（数据异常/已失效）会解析为其他变体，跳过即可
            Ok(_) => continue,
            Err(err) => {
                debug!(
                    "收藏夹「{}」多P视频 {} 分P巡检失败（跳过）: {:#}",
                    favorite_source.name, video_model.bvid, err
                );
                continue;
            }
        };

        if reset_video_if_parts_increased(
            &video_model,
            local_page_count,
            current_page_count,
            &favorite_source.name,
            connection,
        )
        .await?
        {
            updated += 1;
        }
    }

    if updated > 0 {
        notify_videos_changed();
    }
    Ok(updated)
}

/// 收藏夹源：每轮重新拉取收藏夹内已入库的视频合集（type=21）的分集列表，
/// 发现新增分集（新 bvid）时自动入库，使合集后续新增分集也能增量同步。
/// 返回本轮新增的分集数量。
pub async fn recheck_favorite_collections(
    bili_client: &BiliClient,
    video_source: &VideoSourceEnum,
    connection: &DatabaseConnection,
) -> Result<usize> {
    let VideoSourceEnum::Favorite(favorite_source) = video_source else {
        return Ok(0);
    };

    // 每轮最多巡检的合集数量，避免一次性拉取过多合集触发风控
    const COLLECTION_BATCH: usize = 3;

    // 找出该收藏夹内已入库的合集（season_id 非空），按合集去重
    let collection_videos = video::Entity::find()
        .filter(video_source.filter_expr())
        .filter(video::Column::SeasonId.is_not_null())
        .filter(video::Column::SeasonId.ne(""))
        .all(connection)
        .await
        .context("查询收藏夹内合集视频失败")?;

    let mut handled = HashSet::new();
    let mut collections: Vec<(String, i64)> = Vec::new();
    for video_model in collection_videos {
        let Some(season_id) = video_model.season_id.clone() else {
            continue;
        };
        // 同一合集（season_id + upper_id）只处理一次
        let collection_key = format!("{}:{}", season_id, video_model.upper_id);
        if handled.insert(collection_key) {
            collections.push((season_id, video_model.upper_id));
        }
    }
    if collections.is_empty() {
        return Ok(0);
    }

    // 轮转偏移：每个收藏夹源各自记录上次巡检位置，每轮取一批合集，循环覆盖
    let offset = {
        let mut offsets = FAVORITE_COLLECTION_CHECK_OFFSETS.lock().await;
        let entry = offsets.entry(favorite_source.id).or_insert(0);
        let current = *entry;
        *entry = (current + COLLECTION_BATCH) % collections.len();
        current
    };
    collections.rotate_left(offset);

    let mut new_episode_count = 0usize;
    debug!(
        "收藏夹「{}」合集分集巡检：共 {} 个合集，本轮检查 {} 个",
        favorite_source.name,
        collections.len(),
        COLLECTION_BATCH.min(collections.len())
    );
    for (season_id, upper_id) in collections.into_iter().take(COLLECTION_BATCH) {

        let collection_item = CollectionItem {
            mid: upper_id.to_string(),
            sid: season_id.clone(),
            collection_type: CollectionType::Season,
        };
        let collection = Collection::new(bili_client, &collection_item);
        let stream = collection.into_video_stream();
        futures::pin_mut!(stream);

        let mut fetched: Vec<VideoInfo> = Vec::new();
        while let Some(res) = stream.next().await {
            match res {
                Ok(mut episode) => {
                    if let VideoInfo::Collection { season_id: sid, arc, .. } = &mut episode {
                        *sid = Some(season_id.clone());
                        // 合集分集接口返回的条目不带 author 字段，这里把合集作者 mid 补进去，
                        // 保证入库时 upper_id 正确（否则详情填充会因 upper_id=0 误判归属而清空 season_id）。
                        if arc.is_none() {
                            *arc = Some(serde_json::json!({}));
                        }
                        if let Some(arc_value) = arc.as_mut() {
                            if arc_value["author"].is_null() {
                                arc_value["author"] = serde_json::json!({
                                    "mid": upper_id,
                                    "name": "",
                                    "face": "",
                                });
                            }
                        }
                    }
                    fetched.push(episode);
                }
                Err(err) => {
                    warn!(
                        "收藏夹「{}」合集 {} 分集巡检失败（本轮跳过）: {:#}",
                        favorite_source.name, season_id, err
                    );
                    break;
                }
            }
        }
        if fetched.is_empty() {
            continue;
        }

        // 找出该收藏夹内已存在的 bvid，筛出新增分集
        let fetched_bvids: Vec<String> = fetched
            .iter()
            .map(|v| match v {
                VideoInfo::Collection { bvid, .. } => bvid.clone(),
                _ => String::new(),
            })
            .filter(|b| !b.is_empty())
            .collect();
        let existing_bvids: HashSet<String> = video::Entity::find()
            .filter(video_source.filter_expr())
            .filter(video::Column::Bvid.is_in(fetched_bvids.clone()))
            .all(connection)
            .await?
            .into_iter()
            .map(|v| v.bvid)
            .collect();

        let new_episodes: Vec<VideoInfo> = fetched
            .into_iter()
            .filter(|v| {
                let bvid = match v {
                    VideoInfo::Collection { bvid, .. } => bvid.as_str(),
                    _ => return false,
                };
                !bvid.is_empty() && !existing_bvids.contains(bvid)
            })
            .collect();

        if !new_episodes.is_empty() {
            let added_here = new_episodes.len();
            info!(
                "收藏夹「{}」合集 {} 发现 {} 个新增分集，自动入库",
                favorite_source.name, season_id, added_here
            );
            create_videos(new_episodes, video_source, connection).await?;
            new_episode_count += added_here;
        }
    }

    if new_episode_count > 0 {
        notify_videos_changed();
    }
    Ok(new_episode_count)
}

/// 收藏夹多P视频分P巡检的轮转偏移（source_id -> 已巡检到的位置）
static FAVORITE_MULTIPAGE_CHECK_OFFSETS: Lazy<AsyncMutex<HashMap<i32, usize>>> =
    Lazy::new(|| AsyncMutex::new(HashMap::new()));

/// 收藏夹合集分集巡检的轮转偏移（source_id -> 已巡检到的位置）
static FAVORITE_COLLECTION_CHECK_OFFSETS: Lazy<AsyncMutex<HashMap<i32, usize>>> =
    Lazy::new(|| AsyncMutex::new(HashMap::new()));

/// 尝试创建 Page Model，基于 cid 判断是否已存在
///
/// 处理逻辑：
/// - 已存在的分P（按 cid 判断）：跳过，保留本地文件
/// - 新的分P（新 cid）：分配不冲突的 pid 后插入
/// - UP主删除/重排分P：不影响已下载的内容
/// - 单P变多P：自动更新 single_page 字段并重置下载状态
pub async fn create_pages(
    mut pages_info: Vec<PageInfo>,
    video_model: &bili_sync_entity::video::Model,
    connection: &DatabaseTransaction,
) -> Result<()> {
    use sea_orm::{Set, Unchanged};

    // 对于单P视频，统一使用视频标题作为页面名称
    if pages_info.len() == 1 && pages_info[0].page == 1 && pages_info[0].name != video_model.name {
        debug!(
            "单P视频页面名称标准化: 视频 {} ({}), 原名称='{}' -> 使用视频标题='{}'",
            video_model.bvid, video_model.id, pages_info[0].name, video_model.name
        );
        pages_info[0].name = video_model.name.clone();
    }

    // 查询该视频已存在的分P信息
    let existing_pages: Vec<page::Model> = page::Entity::find()
        .filter(page::Column::VideoId.eq(video_model.id))
        .all(connection)
        .await?;

    // 如果没有已存在的分P，直接插入所有分P（首次下载）
    if existing_pages.is_empty() {
        let page_models = pages_info
            .into_iter()
            .map(|p| p.into_active_model(video_model))
            .collect::<Vec<page::ActiveModel>>();
        for page_chunk in page_models.chunks(50) {
            page::Entity::insert_many(page_chunk.to_vec())
                .on_conflict(
                    OnConflict::columns([page::Column::VideoId, page::Column::Pid])
                        .do_nothing()
                        .to_owned(),
                )
                .do_nothing()
                .exec(connection)
                .await?;
        }
        return Ok(());
    }

    // 收集已存在的 cid 和最大 pid
    let existing_cids: HashSet<i64> = existing_pages.iter().map(|p| p.cid).collect();
    let max_existing_pid: i32 = existing_pages.iter().map(|p| p.pid).max().unwrap_or(0);

    // 过滤出新的分P（cid 不存在的）
    let mut new_pages: Vec<PageInfo> = pages_info
        .into_iter()
        .filter(|p| !existing_cids.contains(&p.cid))
        .collect();

    if new_pages.is_empty() {
        debug!("视频 {} ({}) 没有新增分P，跳过", video_model.bvid, video_model.id);
        return Ok(());
    }

    // 为新分P分配不冲突的 pid（从 max_pid + 1 开始）
    for (i, page) in new_pages.iter_mut().enumerate() {
        let old_pid = page.page;
        page.page = max_existing_pid + 1 + i as i32;
        debug!(
            "视频 {} 新增分P: cid={}, B站pid={} -> 本地pid={}",
            video_model.bvid, page.cid, old_pid, page.page
        );
    }

    // 检测单P变多P的情况：原来是单P（single_page=true 且只有1个已存在分P），现在有新增分P
    let total_pages_after = existing_pages.len() + new_pages.len();
    if video_model.single_page == Some(true) && existing_pages.len() == 1 && total_pages_after > 1 {
        info!(
            "视频 {} ({}) 从单P变为多P（{}个分P），更新 single_page 字段并重置下载状态",
            video_model.bvid, video_model.id, total_pages_after
        );

        // 更新视频的 single_page 字段为 false，并重置下载状态以触发重新处理
        let update_video = video::ActiveModel {
            id: Unchanged(video_model.id),
            single_page: Set(Some(false)),
            download_status: Set(0),   // 重置下载状态，让视频重新进入下载流程
            path: Set("".to_string()), // 清空路径，因为目录结构会变化
            total_file_size_bytes: Set(None),
            ..Default::default()
        };
        update_video.save(connection).await?;

        // 同时重置原P1的下载状态，让它重新下载到新的多P目录结构中
        let original_page = &existing_pages[0];
        let old_path = original_page.path.clone();
        info!(
            "重置原P1的下载状态: page_id={}, cid={}, 原路径={:?}",
            original_page.id, original_page.cid, old_path
        );
        let update_page = page::ActiveModel {
            id: Unchanged(original_page.id),
            download_status: Set(0), // 重置下载状态
            path: Set(None),         // 清空路径
            file_size_bytes: Set(None),
            video_stream_size_bytes: Set(None),
            audio_stream_size_bytes: Set(None),
            ..Default::default()
        };
        update_page.save(connection).await?;

        // 发送通知提醒用户清理原文件（异步执行，不阻塞主流程）
        let video_name = video_model.name.clone();
        let bvid = video_model.bvid.clone();
        tokio::spawn(async move {
            use crate::utils::notification::send_single_to_multi_page_notification;
            if let Err(e) =
                send_single_to_multi_page_notification(&video_name, &bvid, total_pages_after, old_path.as_deref()).await
            {
                tracing::warn!("发送单P变多P通知失败: {}", e);
            }
        });
    } else {
        info!(
            "视频 {} ({}) 检测到 {} 个新增分P，准备下载",
            video_model.bvid,
            video_model.id,
            new_pages.len()
        );
    }

    // 插入新分P
    let page_models = new_pages
        .into_iter()
        .map(|p| p.into_active_model(video_model))
        .collect::<Vec<page::ActiveModel>>();

    for page_chunk in page_models.chunks(50) {
        page::Entity::insert_many(page_chunk.to_vec()).exec(connection).await?;
    }

    Ok(())
}

/// 更新视频 model 的下载状态
pub async fn update_videos_model(videos: Vec<video::ActiveModel>, connection: &DatabaseConnection) -> Result<()> {
    if videos.is_empty() {
        return Ok(());
    }

    let affected_count = videos.len();
    crate::database::run_traced_db_operation(
        format!("utils.model.update_videos_model(count={affected_count})"),
        async {
            // 这些调用点都只更新已存在的视频记录，直接 UPDATE 比 UPSERT 更轻。
            for video in videos {
                video::Entity::update(video).exec(connection).await?;
            }
            Ok::<_, DbErr>(())
        },
    )
    .await?;

    notify_videos_changed();
    Ok(())
}

/// 更新视频页 model 的下载状态
pub async fn update_pages_model(pages: Vec<page::ActiveModel>, connection: &DatabaseConnection) -> Result<()> {
    if pages.is_empty() {
        return Ok(());
    }

    let (done_tx, done_rx) = oneshot::channel();
    {
        let mut queue = PAGE_MODEL_UPDATE_QUEUE.lock().await;
        queue.push(PendingPageUpdateRequest { pages, done_tx });
    }
    PAGE_MODEL_UPDATE_QUEUE_NOTIFY.notify_one();

    if PAGE_MODEL_UPDATE_WORKER_RUNNING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        let connection = connection.clone();
        tokio::spawn(async move {
            flush_batched_page_updates(connection).await;
        });
    }

    match done_rx.await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => Err(anyhow!(err)),
        Err(_) => Err(anyhow!("分页状态批量写入 worker 异常退出")),
    }
}

fn dedup_ids(ids: &[i32]) -> Vec<i32> {
    let mut unique_ids = ids.iter().copied().filter(|id| *id > 0).collect::<Vec<_>>();
    unique_ids.sort_unstable();
    unique_ids.dedup();
    unique_ids
}

fn file_size_to_i64(size: u64) -> i64 {
    i64::try_from(size).unwrap_or(i64::MAX)
}

const VIDEO_FILE_SIZE_BACKFILL_BATCH_SIZE: usize = 200;

static VIDEO_FILE_SIZE_BACKFILL_QUEUE: Lazy<Mutex<HashSet<i32>>> = Lazy::new(|| Mutex::new(HashSet::new()));
static VIDEO_FILE_SIZE_BACKFILL_RUNNING: AtomicBool = AtomicBool::new(false);
static PAGE_MODEL_UPDATE_QUEUE: Lazy<AsyncMutex<Vec<PendingPageUpdateRequest>>> =
    Lazy::new(|| AsyncMutex::new(Vec::new()));
static PAGE_MODEL_UPDATE_QUEUE_NOTIFY: Lazy<Notify> = Lazy::new(Notify::new);
static PAGE_MODEL_UPDATE_WORKER_RUNNING: AtomicBool = AtomicBool::new(false);

const PAGE_MODEL_UPDATE_BATCH_WINDOW: Duration = Duration::from_millis(50);

struct PendingPageUpdateRequest {
    pages: Vec<page::ActiveModel>,
    done_tx: oneshot::Sender<std::result::Result<(), String>>,
}

fn get_page_active_model_id(page: &page::ActiveModel) -> Option<i32> {
    match &page.id {
        sea_orm::ActiveValue::Set(id) | sea_orm::ActiveValue::Unchanged(id) => Some(*id),
        sea_orm::ActiveValue::NotSet => None,
    }
}

async fn flush_batched_page_updates(connection: DatabaseConnection) {
    loop {
        let mut requests = {
            let mut queue = PAGE_MODEL_UPDATE_QUEUE.lock().await;
            if queue.is_empty() {
                PAGE_MODEL_UPDATE_WORKER_RUNNING.store(false, Ordering::Release);
                return;
            }
            queue.drain(..).collect::<Vec<_>>()
        };

        // 等待一个短暂的安静窗口，把同一波后续页面状态写入尽量合并成一批。
        while tokio::time::timeout(
            PAGE_MODEL_UPDATE_BATCH_WINDOW,
            PAGE_MODEL_UPDATE_QUEUE_NOTIFY.notified(),
        )
        .await
        .is_ok()
        {
            let mut extra_requests = {
                let mut queue = PAGE_MODEL_UPDATE_QUEUE.lock().await;
                queue.drain(..).collect::<Vec<_>>()
            };
            if extra_requests.is_empty() {
                continue;
            }
            requests.append(&mut extra_requests);
        }

        let mut ordered_ids = Vec::new();
        let mut deduped_pages = HashMap::new();
        let mut passthrough_pages = Vec::new();
        for request in &requests {
            for page in &request.pages {
                if let Some(page_id) = get_page_active_model_id(page) {
                    if !deduped_pages.contains_key(&page_id) {
                        ordered_ids.push(page_id);
                    }
                    deduped_pages.insert(page_id, page.clone());
                } else {
                    passthrough_pages.push(page.clone());
                }
            }
        }

        let mut merged_pages = ordered_ids
            .into_iter()
            .filter_map(|page_id| deduped_pages.remove(&page_id))
            .collect::<Vec<_>>();
        merged_pages.extend(passthrough_pages);

        let affected_count = merged_pages.len();
        let result = crate::database::run_traced_db_operation(
            format!("utils.model.update_pages_model(count={affected_count})"),
            async {
                use sea_orm::TransactionTrait;

                connection
                    .transaction::<_, (), DbErr>(move |txn| {
                        Box::pin(async move {
                            for page in merged_pages {
                                page::Entity::update(page).exec(txn).await?;
                            }
                            Ok(())
                        })
                    })
                    .await
                    .map_err(|err| match err {
                        sea_orm::TransactionError::Connection(db_err)
                        | sea_orm::TransactionError::Transaction(db_err) => db_err,
                    })?;
                Ok::<_, DbErr>(())
            },
        )
        .await;

        if result.is_ok() {
            notify_videos_changed();
        }

        let error_text = result.err().map(|err| format!("{:#}", err));
        for request in requests {
            let _ = request.done_tx.send(match &error_text {
                Some(err) => Err(err.clone()),
                None => Ok(()),
            });
        }
    }
}

fn take_video_file_size_backfill_batch(limit: usize) -> Vec<i32> {
    let mut queue = VIDEO_FILE_SIZE_BACKFILL_QUEUE
        .lock()
        .expect("video file size backfill queue lock poisoned");
    let mut batch = queue.iter().copied().collect::<Vec<_>>();
    batch.sort_unstable();
    batch.truncate(limit);
    for video_id in &batch {
        queue.remove(video_id);
    }
    batch
}

pub fn is_video_file_size_backfill_pending() -> bool {
    VIDEO_FILE_SIZE_BACKFILL_RUNNING.load(Ordering::Acquire)
        || !VIDEO_FILE_SIZE_BACKFILL_QUEUE
            .lock()
            .expect("video file size backfill queue lock poisoned")
            .is_empty()
}

fn spawn_video_file_size_backfill_worker(connection: Arc<DatabaseConnection>) {
    if VIDEO_FILE_SIZE_BACKFILL_RUNNING
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }

    tokio::spawn(async move {
        loop {
            let batch = take_video_file_size_backfill_batch(VIDEO_FILE_SIZE_BACKFILL_BATCH_SIZE);
            if batch.is_empty() {
                VIDEO_FILE_SIZE_BACKFILL_RUNNING.store(false, Ordering::Release);
                if !VIDEO_FILE_SIZE_BACKFILL_QUEUE
                    .lock()
                    .expect("video file size backfill queue lock poisoned")
                    .is_empty()
                    && VIDEO_FILE_SIZE_BACKFILL_RUNNING
                        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                        .is_ok()
                {
                    continue;
                }
                break;
            }

            match backfill_video_file_sizes(&batch, connection.as_ref()).await {
                Ok(()) => {
                    debug!("后台文件大小回填完成一批视频，共 {} 条", batch.len());
                    notify_videos_changed();
                }
                Err(error) => {
                    warn!("后台文件大小回填失败，本批 {} 条视频已跳过: {error:#}", batch.len());
                }
            }
        }
    });
}

pub fn queue_video_file_size_backfill(video_ids: &[i32], connection: Arc<DatabaseConnection>) -> usize {
    let video_ids = dedup_ids(video_ids);
    if video_ids.is_empty() {
        return 0;
    }

    let queued_count = {
        let mut queue = VIDEO_FILE_SIZE_BACKFILL_QUEUE
            .lock()
            .expect("video file size backfill queue lock poisoned");
        for video_id in &video_ids {
            queue.insert(*video_id);
        }
        queue.len()
    };

    spawn_video_file_size_backfill_worker(connection);
    queued_count
}

pub async fn queue_missing_video_file_size_backfill(connection: Arc<DatabaseConnection>) -> Result<usize> {
    let missing_video_ids = video::Entity::find()
        .select_only()
        .column(video::Column::Id)
        .filter(video::Column::TotalFileSizeBytes.is_null())
        .into_tuple::<(i32,)>()
        .all(connection.as_ref())
        .await?
        .into_iter()
        .map(|(id,)| id)
        .collect::<Vec<_>>();

    Ok(queue_video_file_size_backfill(&missing_video_ids, connection))
}

pub async fn backfill_video_file_sizes(video_ids: &[i32], connection: &DatabaseConnection) -> Result<()> {
    let video_ids = dedup_ids(video_ids);
    if video_ids.is_empty() {
        return Ok(());
    }

    let page_rows: Vec<(i32, i32, Option<String>)> = page::Entity::find()
        .select_only()
        .column(page::Column::Id)
        .column(page::Column::VideoId)
        .column(page::Column::Path)
        .filter(page::Column::VideoId.is_in(video_ids.clone()))
        .into_tuple::<(i32, i32, Option<String>)>()
        .all(connection)
        .await?;

    let (page_sizes, total_sizes) = tokio::task::spawn_blocking(move || {
        let mut page_sizes = HashMap::<i32, i64>::new();
        let mut total_sizes = HashMap::<i32, i64>::new();

        for (page_id, video_id, path) in page_rows {
            let file_size = path
                .as_deref()
                .and_then(|path| std::fs::metadata(path).ok())
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            let file_size = file_size_to_i64(file_size);
            page_sizes.insert(page_id, file_size);
            total_sizes
                .entry(video_id)
                .and_modify(|total| *total = total.saturating_add(file_size))
                .or_insert(file_size);
        }

        (page_sizes, total_sizes)
    })
    .await
    .map_err(|e| anyhow::anyhow!("回填视频文件大小任务失败: {e}"))?;

    let txn = crate::database::begin_traced_transaction(connection, "utils.model.backfill_video_file_sizes").await?;
    for (page_id, file_size_bytes) in page_sizes {
        page::Entity::update(page::ActiveModel {
            id: Unchanged(page_id),
            file_size_bytes: Set(Some(file_size_bytes)),
            ..Default::default()
        })
        .exec(&txn)
        .await?;
    }

    for video_id in video_ids {
        video::Entity::update(video::ActiveModel {
            id: Unchanged(video_id),
            total_file_size_bytes: Set(Some(total_sizes.get(&video_id).copied().unwrap_or(0))),
            ..Default::default()
        })
        .exec(&txn)
        .await?;
    }
    txn.commit().await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bili_sync_migration::{Migrator, MigratorTrait};
    use sea_orm::sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
    use sea_orm::{
        ActiveModelTrait, ActiveValue::NotSet, ConnectionTrait, DbBackend, QueryOrder, SqlxSqliteConnector, Statement,
    };
    use std::fs;
    use std::path::{Path, PathBuf};

    /// collect_favorite_page_counts 只收集 type=2 普通视频的分P数，
    /// 且忽略 page<=0 或 bvid 为空的条目（合集已在收藏夹流中展开）。
    #[test]
    fn collect_favorite_page_counts_filters_valid_entries() {
        fn mk_favorite(bvid: &str, vtype: i32, page: i32) -> VideoInfo {
            serde_json::from_value(serde_json::json!({
                "id": 1,
                "type": vtype,
                "title": "测试",
                "intro": "",
                "cover": "",
                "upper": { "mid": 1, "name": "UP主", "face": "" },
                "ctime": 1620000000,
                "fav_time": 1620000000,
                "pubtime": 1620000000,
                "duration": 60,
                "attr": 0,
                "bvid": bvid,
                "bv_id": bvid,
                "page": page,
            }))
            .expect("收藏夹条目应能解析")
        }

        let videos = vec![
            mk_favorite("BV1multi", 2, 16),
            mk_favorite("BV1single", 2, 1),
            mk_favorite("", 2, 5),      // bvid 为空：忽略
            mk_favorite("BV1zero", 2, 0), // page 为 0：忽略
        ];
        let map = collect_favorite_page_counts(&videos);
        assert_eq!(map.len(), 2);
        assert_eq!(map.get("BV1multi"), Some(&16));
        assert_eq!(map.get("BV1single"), Some(&1));
    }

    /// reset_video_if_parts_increased：当前分P数大于本地时重置视频，否则不动。
    #[tokio::test]
    async fn reset_video_if_parts_increased_only_resets_when_count_grew() {
        use chrono::TimeZone;
        let db = create_test_db("favorite-reset-helper").await;
        let ts = chrono::Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap();

        let video_id = 3001;
        video::ActiveModel {
            id: Set(video_id),
            collection_id: Set(None),
            favorite_id: Set(Some(1)),
            watch_later_id: Set(None),
            submission_id: Set(None),
            source_id: Set(None),
            source_type: Set(None),
            upper_id: Set(1),
            upper_name: Set("UP主".to_string()),
            upper_face: Set(String::new()),
            staff_info: Set(None),
            source_submission_id: Set(None),
            name: Set("多P视频".to_string()),
            path: Set("/tmp/video".to_string()),
            category: Set(2),
            bvid: Set("BV1CV4y1e7qF".to_string()),
            intro: Set(String::new()),
            cover: Set(String::new()),
            ctime: Set(ts.naive_utc()),
            pubtime: Set(ts.naive_utc()),
            favtime: Set(ts.naive_utc()),
            download_status: Set(3),
            valid: Set(true),
            tags: Set(None),
            single_page: Set(Some(false)),
            created_at: Set("2026-08-01 00:00:00".to_string()),
            season_id: Set(None),
            submission_membership_state: Set(0),
            submission_membership_checked_at: Set(None),
            ep_id: Set(None),
            season_number: Set(None),
            episode_number: Set(None),
            deleted: Set(0),
            share_copy: Set(None),
            show_season_type: Set(None),
            actors: Set(None),
            auto_download: Set(true),
            cid: Set(None),
            is_charge_video: Set(false),
            charge_can_play: Set(false),
            total_file_size_bytes: Set(None),
            skip_reason: Set(None),
        }
        .insert(&db)
        .await
        .expect("应能插入视频");

        let video = video::Entity::find_by_id(video_id)
            .one(&db)
            .await
            .expect("查询应成功")
            .expect("视频应存在");

        // 未增加（6 <= 6）：不重置
        let reset = reset_video_if_parts_increased(&video, 6, 6, "测试收藏夹", &db)
            .await
            .expect("比较应成功");
        assert!(!reset, "分P数未增加时不应重置");
        let after = video::Entity::find_by_id(video_id)
            .one(&db)
            .await
            .expect("查询应成功")
            .expect("视频应存在");
        assert_eq!(after.single_page, Some(false));
        assert_eq!(after.download_status, 3);

        // 增加（4 -> 6）：重置
        let reset = reset_video_if_parts_increased(&video, 4, 6, "测试收藏夹", &db)
            .await
            .expect("比较应成功");
        assert!(reset, "分P数增加时应重置");
        let after = video::Entity::find_by_id(video_id)
            .one(&db)
            .await
            .expect("查询应成功")
            .expect("视频应存在");
        assert_eq!(after.single_page, None, "应重置 single_page 以重新拉取详情");
        assert_eq!(after.download_status, 0, "应重置下载状态");
    }

    /// Collection 变体经 into_simple_model 入库时应保留 season_id，
    /// 这是收藏夹合集分集巡检追踪合集的基础。
    #[test]
    fn collection_into_simple_model_keeps_season_id() {
        use chrono::TimeZone;
        let ts = chrono::Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap();
        let info = VideoInfo::Collection {
            bvid: "BV1H2421A7kz".to_string(),
            cover: String::new(),
            ctime: ts,
            pubtime: ts,
            title: "测试分集".to_string(),
            duration: Some(60),
            arc: Some(serde_json::json!({"author": {"mid": 7920565}})),
            season_id: Some("2337331".to_string()),
        };
        let model = info.into_simple_model();
        match model.season_id {
            sea_orm::ActiveValue::Set(Some(v)) => assert_eq!(v, "2337331"),
            other => panic!("season_id 应被保留，实际: {:?}", other),
        }
    }

    /// 分P巡检对非收藏夹源直接返回 0，不发起任何请求。
    #[tokio::test]
    async fn check_favorite_multipage_videos_skips_non_favorite_sources() {
        let db = create_test_db("favorite-multipage-guard").await;
        let source = VideoSourceEnum::WatchLater(
            watch_later::Model {
                id: 1,
                path: "/tmp/wl".to_string(),
                created_at: "2026-08-01 00:00:00".to_string(),
                latest_row_at: "1970-01-01 00:00:00".to_string(),
                enabled: true,
                scan_deleted_videos: false,
                scan_deleted_videos_once: false,
                filter_option: None,
                keyword_filters: None,
                keyword_filter_mode: None,
                blacklist_keywords: None,
                whitelist_keywords: None,
                keyword_case_sensitive: false,
                min_duration_seconds: None,
                max_duration_seconds: None,
                published_after: None,
                published_before: None,
                audio_only: false,
                audio_only_m4a_only: false,
                flat_folder: false,
                split_chapters_after_download: false,
                download_charge_videos: true,
                download_danmaku: true,
                download_subtitle: true,
                download_ai_subtitle: true,
                ai_subtitle_language: "zh-CN".to_string(),
                ai_rename: false,
                ai_rename_video_prompt: String::new(),
                ai_rename_audio_prompt: String::new(),
                ai_rename_enable_multi_page: false,
                ai_rename_enable_collection: false,
                ai_rename_enable_bangumi: false,
                ai_rename_rename_parent_dir: false,
            },
        );
        let client = crate::bilibili::BiliClient::new(String::new());
        let count = check_favorite_multipage_videos_for_new_parts(&client, &source, &db)
            .await
            .expect("非收藏夹源应直接返回 0");
        assert_eq!(count, 0);
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("bili-sync-model-{}-{}", prefix, uuid::Uuid::new_v4()));
        dir
    }

    async fn create_test_db(prefix: &str) -> DatabaseConnection {
        let dir = unique_temp_dir(prefix);
        fs::create_dir_all(&dir).expect("应能创建临时数据库目录");
        let db_path = dir.join("data.sqlite");

        let options = SqliteConnectOptions::new()
            .filename(&db_path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .expect("应能连接测试数据库");
        let db = SqlxSqliteConnector::from_sqlx_sqlite_pool(pool);

        Migrator::up(&db, None).await.expect("应能完成测试数据库迁移");
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "ALTER TABLE page ADD COLUMN ai_renamed INTEGER".to_string(),
        ))
        .await
        .ok();
        db
    }

    async fn insert_test_video(db: &DatabaseConnection, id: i32, title: &str) {
        let test_time = chrono::DateTime::from_timestamp(1_640_995_200, 0).unwrap().naive_utc();

        video::ActiveModel {
            id: Set(id),
            collection_id: Set(None),
            favorite_id: Set(None),
            watch_later_id: Set(None),
            submission_id: Set(Some(1)),
            source_id: Set(None),
            source_type: Set(Some(4)),
            upper_id: Set(2000 + i64::from(id)),
            upper_name: Set("测试UP".to_string()),
            upper_face: Set(String::new()),
            staff_info: Set(None),
            source_submission_id: Set(None),
            name: Set(title.to_string()),
            path: Set(format!("/tmp/video-{id}")),
            category: Set(1),
            bvid: Set(format!("BV{id:010}")),
            intro: Set(String::new()),
            cover: Set(String::new()),
            ctime: Set(test_time),
            pubtime: Set(test_time),
            favtime: Set(test_time),
            download_status: Set(0),
            valid: Set(true),
            tags: Set(None),
            single_page: Set(Some(true)),
            created_at: Set("2026-03-30 00:00:00".to_string()),
            season_id: Set(None),
            submission_membership_state: Set(0),
            submission_membership_checked_at: Set(None),
            ep_id: Set(None),
            season_number: Set(None),
            episode_number: Set(None),
            deleted: Set(0),
            share_copy: Set(None),
            show_season_type: Set(None),
            actors: Set(None),
            auto_download: Set(false),
            cid: Set(None),
            is_charge_video: Set(false),
            charge_can_play: Set(false),
            total_file_size_bytes: Set(None),
            skip_reason: Set(None),
        }
        .insert(db)
        .await
        .expect("应能插入测试视频");
    }

    async fn insert_test_page(db: &DatabaseConnection, id: i32, video_id: i32, path: Option<String>) {
        page::ActiveModel {
            id: Set(id),
            video_id: Set(video_id),
            cid: Set(1000 + i64::from(id)),
            pid: Set(id),
            name: Set(format!("P{id}")),
            width: Set(Some(1920)),
            height: Set(Some(1080)),
            duration: Set(120),
            path: Set(path),
            file_size_bytes: Set(None),
            video_stream_size_bytes: Set(None),
            audio_stream_size_bytes: Set(None),
            image: Set(None),
            download_status: Set(0),
            created_at: Set("2026-03-30 00:00:00".to_string()),
            play_video_streams: Set(None),
            play_audio_streams: Set(None),
            play_subtitle_streams: Set(None),
            play_streams_updated_at: Set(None),
            danmaku_last_synced_at: Set(None),
            danmaku_sync_generation: Set(0),
            danmaku_cid_snapshot: Set(None),
            danmaku_last_write_count: Set(0),
            ai_renamed: NotSet,
        }
        .insert(db)
        .await
        .expect("应能插入测试分页");
    }

    /// 收藏夹源检测到 B站 当前分P数大于本地分P数时，应重置视频
    /// 的 single_page/download_status，使其重新进入详情填充阶段。
    #[tokio::test]
    async fn check_favorite_videos_for_new_parts_resets_video_when_page_count_increased() {
        use crate::bilibili::VideoInfo;
        use chrono::TimeZone;

        let db = create_test_db("favorite-page-reset").await;
        let ts = chrono::Utc.with_ymd_and_hms(2026, 8, 1, 0, 0, 0).unwrap();

        // 插入收藏夹源
        let fav_id = 10001;
        favorite::ActiveModel {
            id: Set(fav_id),
            f_id: Set(1087857200),
            name: Set("测试收藏夹".to_string()),
            path: Set("/tmp/fav".to_string()),
            created_at: Set("2026-08-01 00:00:00".to_string()),
            latest_row_at: Set("1970-01-01 00:00:00".to_string()),
            enabled: Set(true),
            scan_deleted_videos: Set(false),
            scan_deleted_videos_once: Set(false),
            filter_option: Set(None),
            keyword_filters: Set(None),
            keyword_filter_mode: Set(None),
            blacklist_keywords: Set(None),
            whitelist_keywords: Set(None),
            keyword_case_sensitive: Set(false),
            min_duration_seconds: Set(None),
            max_duration_seconds: Set(None),
            published_after: Set(None),
            published_before: Set(None),
            audio_only: Set(false),
            audio_only_m4a_only: Set(false),
            flat_folder: Set(false),
            split_chapters_after_download: Set(false),
            download_charge_videos: Set(true),
            download_danmaku: Set(true),
            download_subtitle: Set(true),
            download_ai_subtitle: Set(true),
            ai_subtitle_language: Set("zh-CN".to_string()),
            ai_rename: Set(false),
            ai_rename_video_prompt: Set(String::new()),
            ai_rename_audio_prompt: Set(String::new()),
            ai_rename_enable_multi_page: Set(false),
            ai_rename_enable_collection: Set(false),
            ai_rename_enable_bangumi: Set(false),
            ai_rename_rename_parent_dir: Set(false),
        }
        .insert(&db)
        .await
        .expect("应能插入收藏夹源");

        // 插入一个已填充详情的多P视频（本地 4 个分P）
        let video_id = 2001;
        video::ActiveModel {
            id: Set(video_id),
            collection_id: Set(None),
            favorite_id: Set(Some(fav_id)),
            watch_later_id: Set(None),
            submission_id: Set(None),
            source_id: Set(None),
            source_type: Set(None),
            upper_id: Set(1),
            upper_name: Set("UP主".to_string()),
            upper_face: Set(String::new()),
            staff_info: Set(None),
            source_submission_id: Set(None),
            name: Set("多P视频".to_string()),
            path: Set("/tmp/fav/video".to_string()),
            category: Set(2),
            bvid: Set("BV1CV4y1e7qF".to_string()),
            intro: Set(String::new()),
            cover: Set(String::new()),
            ctime: Set(ts.naive_utc()),
            pubtime: Set(ts.naive_utc()),
            favtime: Set(ts.naive_utc()),
            download_status: Set(3), // 已完成的下载状态
            valid: Set(true),
            tags: Set(None),
            single_page: Set(Some(false)),
            created_at: Set("2026-08-01 00:00:00".to_string()),
            season_id: Set(None),
            submission_membership_state: Set(0),
            submission_membership_checked_at: Set(None),
            ep_id: Set(None),
            season_number: Set(None),
            episode_number: Set(None),
            deleted: Set(0),
            share_copy: Set(None),
            show_season_type: Set(None),
            actors: Set(None),
            auto_download: Set(true),
            cid: Set(Some(1224527561)),
            is_charge_video: Set(false),
            charge_can_play: Set(false),
            total_file_size_bytes: Set(None),
            skip_reason: Set(None),
        }
        .insert(&db)
        .await
        .expect("应能插入视频");

        // 插入 4 个已存在分P
        for i in 1..=4 {
            page::ActiveModel {
                id: NotSet,
                video_id: Set(video_id),
                cid: Set(1000 + i64::from(i)),
                pid: Set(i),
                name: Set(format!("P{i}")),
                width: Set(None),
                height: Set(None),
                duration: Set(60),
                image: Set(None),
                download_status: Set(3),
                path: Set(Some(format!("/tmp/fav/video/P{i}.mp4"))),
                file_size_bytes: Set(Some(100)),
                video_stream_size_bytes: Set(None),
                audio_stream_size_bytes: Set(None),
                created_at: Set("2026-08-01 00:00:00".to_string()),
                play_video_streams: Set(None),
                play_audio_streams: Set(None),
                play_subtitle_streams: Set(None),
                play_streams_updated_at: Set(None),
                danmaku_last_synced_at: Set(None),
                danmaku_sync_generation: Set(0),
                danmaku_cid_snapshot: Set(None),
                danmaku_last_write_count: Set(0),
                ai_renamed: NotSet,
            }
            .insert(&db)
            .await
            .expect("应能插入分P");
        }

        let source = VideoSourceEnum::Favorite(
            favorite::Entity::find_by_id(fav_id)
                .one(&db)
                .await
                .expect("查询收藏夹源应成功")
                .expect("收藏夹源应存在"),
        );
        let page_map = collect_favorite_page_counts(&[serde_json::from_value(serde_json::json!({
            "id": 1,
            "type": 2,
            "title": "多P视频",
            "intro": "",
            "cover": "",
            "upper": { "mid": 1, "name": "UP主", "face": "" },
            "ctime": 1620000000,
            "fav_time": 1620000000,
            "pubtime": 1620000000,
            "attr": 0,
            "bvid": "BV1CV4y1e7qF",
            "page": 6,
        })).expect("收藏夹条目应能解析")]);

        assert_eq!(page_map.get("BV1CV4y1e7qF"), Some(&6));
        check_favorite_videos_for_new_parts(page_map, &source, &db)
            .await
            .expect("分P检测应成功");

        let updated = video::Entity::find_by_id(video_id).one(&db).await.expect("应能找到视频");
        let updated = updated.expect("视频应存在");
        assert_eq!(updated.single_page, None, "检测到新增分P后应重置 single_page 以重新拉取详情");
        assert_eq!(updated.download_status, 0, "检测到新增分P后应重置下载状态");
    }

    fn create_file_with_size(root: &Path, name: &str, size: usize) -> String {
        let path = root.join(name);
        fs::write(&path, vec![b'x'; size]).expect("应能写入测试文件");
        path.to_string_lossy().to_string()
    }

    fn selective_submission_source(created_at: &str) -> VideoSourceEnum {
        VideoSourceEnum::Submission(submission::Model {
            id: 1,
            upper_id: 1000,
            upper_name: "测试UP".to_string(),
            path: "/tmp/submission-selective".to_string(),
            created_at: created_at.to_string(),
            latest_row_at: "1970-01-01 00:00:00".to_string(),
            enabled: true,
            scan_deleted_videos: false,
            scan_deleted_videos_once: false,
            filter_option: None,
            selected_videos: Some("[]".to_string()),
            keyword_filters: None,
            keyword_filter_mode: None,
            blacklist_keywords: None,
            whitelist_keywords: None,
            keyword_case_sensitive: false,
            min_duration_seconds: None,
            max_duration_seconds: None,
            published_after: None,
            published_before: None,
            audio_only: false,
            audio_only_m4a_only: false,
            flat_folder: false,
            split_chapters_after_download: false,
            download_charge_videos: true,
            download_danmaku: true,
            download_subtitle: true,
            download_ai_subtitle: true,
            ai_subtitle_language: "zh-CN".to_string(),
            ai_rename: false,
            ai_rename_video_prompt: String::new(),
            ai_rename_audio_prompt: String::new(),
            ai_rename_enable_multi_page: false,
            ai_rename_enable_collection: false,
            ai_rename_enable_bangumi: false,
            ai_rename_rename_parent_dir: false,
            use_dynamic_api: false,
            dynamic_api_full_synced: false,
            last_scan_at: None,
            next_scan_at: None,
            no_update_streak: 0,
        })
    }

    #[tokio::test]
    async fn create_videos_treats_submission_created_at_as_beijing_time_for_selective_download() {
        let db = create_test_db("selective-created-at-beijing").await;
        let source = selective_submission_source("2026-06-03 20:52:11");
        let video_time = chrono::DateTime::parse_from_rfc3339("2026-06-03T14:32:31Z")
            .expect("测试时间应合法")
            .with_timezone(&chrono::Utc);

        create_videos(
            vec![crate::bilibili::VideoInfo::Submission {
                title: "订阅后新投稿".to_string(),
                bvid: "BV1CreatedAtBeijing".to_string(),
                intro: String::new(),
                cover: String::new(),
                ctime: video_time,
                duration: Some(60),
                season_id: None,
            }],
            &source,
            &db,
        )
        .await
        .expect("创建视频应成功");

        let inserted_count = video::Entity::find()
            .filter(video::Column::Bvid.eq("BV1CreatedAtBeijing"))
            .count(&db)
            .await
            .expect("应能统计插入视频");
        assert_eq!(
            inserted_count, 1,
            "北京时间 22:32:31 晚于订阅创建时间 20:52:11，应作为新投稿存储"
        );
    }

    #[tokio::test]
    async fn backfill_video_file_sizes_persists_page_and_video_totals() {
        let db = create_test_db("backfill-size").await;
        insert_test_video(&db, 1, "测试视频").await;

        let root = unique_temp_dir("backfill-files");
        fs::create_dir_all(&root).expect("应能创建测试文件目录");
        let page_one = create_file_with_size(&root, "page-1.m4s", 11);
        let page_two = create_file_with_size(&root, "page-2.m4s", 7);

        insert_test_page(&db, 1, 1, Some(page_one)).await;
        insert_test_page(&db, 2, 1, Some(page_two)).await;

        backfill_video_file_sizes(&[1], &db).await.expect("回填文件大小应成功");

        let pages = page::Entity::find()
            .filter(page::Column::VideoId.eq(1))
            .order_by_asc(page::Column::Id)
            .all(&db)
            .await
            .expect("应能查询分页");
        assert_eq!(
            pages.iter().map(|page| page.file_size_bytes).collect::<Vec<_>>(),
            vec![Some(11), Some(7)]
        );

        let video = video::Entity::find_by_id(1)
            .one(&db)
            .await
            .expect("应能查询视频")
            .expect("视频应存在");
        assert_eq!(video.total_file_size_bytes, Some(18));

        let _ = fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn update_pages_model_updates_page_sizes_without_recomputing_video_total_file_size_bytes() {
        let db = create_test_db("update-page-sizes").await;
        insert_test_video(&db, 1, "测试视频").await;
        insert_test_page(&db, 1, 1, Some("/tmp/page-1.m4s".to_string())).await;
        insert_test_page(&db, 2, 1, Some("/tmp/page-2.m4s".to_string())).await;

        let mut page_one: page::ActiveModel = page::Entity::find_by_id(1)
            .one(&db)
            .await
            .expect("应能查询第一个分页")
            .expect("第一个分页应存在")
            .into();
        page_one.download_status = Set(7);
        page_one.path = Set(Some("/tmp/page-1.m4s".to_string()));
        page_one.file_size_bytes = Set(Some(32));
        page_one.video_stream_size_bytes = Set(Some(20));
        page_one.audio_stream_size_bytes = Set(Some(12));

        let mut page_two: page::ActiveModel = page::Entity::find_by_id(2)
            .one(&db)
            .await
            .expect("应能查询第二个分页")
            .expect("第二个分页应存在")
            .into();
        page_two.download_status = Set(7);
        page_two.path = Set(Some("/tmp/page-2.m4s".to_string()));
        page_two.file_size_bytes = Set(Some(18));
        page_two.video_stream_size_bytes = Set(Some(10));
        page_two.audio_stream_size_bytes = Set(Some(8));

        update_pages_model(vec![page_one, page_two], &db)
            .await
            .expect("更新分页状态应成功");

        let video = video::Entity::find_by_id(1)
            .one(&db)
            .await
            .expect("应能查询视频")
            .expect("视频应存在");
        assert_eq!(video.total_file_size_bytes, None);
    }

    #[tokio::test]
    async fn update_pages_model_handles_concurrent_single_page_updates() {
        let db = create_test_db("update-page-concurrent").await;
        insert_test_video(&db, 1, "测试视频").await;
        insert_test_page(&db, 1, 1, Some("/tmp/page-1.m4s".to_string())).await;
        insert_test_page(&db, 2, 1, Some("/tmp/page-2.m4s".to_string())).await;

        let mut page_one: page::ActiveModel = page::Entity::find_by_id(1)
            .one(&db)
            .await
            .expect("应能查询第一个分页")
            .expect("第一个分页应存在")
            .into();
        page_one.download_status = Set(7);
        page_one.file_size_bytes = Set(Some(32));

        let mut page_two: page::ActiveModel = page::Entity::find_by_id(2)
            .one(&db)
            .await
            .expect("应能查询第二个分页")
            .expect("第二个分页应存在")
            .into();
        page_two.download_status = Set(8);
        page_two.file_size_bytes = Set(Some(18));

        let (first, second) = tokio::join!(
            update_pages_model(vec![page_one], &db),
            update_pages_model(vec![page_two], &db),
        );
        first.expect("第一个并发分页更新应成功");
        second.expect("第二个并发分页更新应成功");

        let pages = page::Entity::find()
            .filter(page::Column::VideoId.eq(1))
            .order_by_asc(page::Column::Id)
            .all(&db)
            .await
            .expect("应能查询分页");
        assert_eq!(pages[0].download_status, 7);
        assert_eq!(pages[0].file_size_bytes, Some(32));
        assert_eq!(pages[1].download_status, 8);
        assert_eq!(pages[1].file_size_bytes, Some(18));
    }

    #[tokio::test]
    async fn update_videos_model_updates_existing_video_fields() {
        let db = create_test_db("update-video-status").await;
        insert_test_video(&db, 1, "测试视频").await;

        let mut video_model: video::ActiveModel = video::Entity::find_by_id(1)
            .one(&db)
            .await
            .expect("应能查询视频")
            .expect("视频应存在")
            .into();
        video_model.download_status = Set(7);
        video_model.path = Set("/tmp/video-1-updated".to_string());
        video_model.total_file_size_bytes = Set(Some(128));

        update_videos_model(vec![video_model], &db)
            .await
            .expect("更新视频状态应成功");

        let updated = video::Entity::find_by_id(1)
            .one(&db)
            .await
            .expect("应能查询更新后视频")
            .expect("更新后视频应存在");
        assert_eq!(updated.download_status, 7);
        assert_eq!(updated.path, "/tmp/video-1-updated");
        assert_eq!(updated.total_file_size_bytes, Some(128));
    }
}
