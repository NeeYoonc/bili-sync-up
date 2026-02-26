use anyhow::{Context, Result};
use bili_sync_entity::*;
use sea_orm::entity::prelude::*;
use sea_orm::sea_query::{OnConflict, SimpleExpr};
use sea_orm::DatabaseTransaction;
use std::collections::HashSet;
use tracing::{debug, info};

use crate::adapter::{VideoSource, VideoSourceEnum};
use crate::bilibili::{PageInfo, VideoInfo};
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
    // 向后兼容：旧的单列表模式
    let keyword_filters = video_source.get_keyword_filters();
    let keyword_filter_mode = video_source.get_keyword_filter_mode();

    // 判断是否有任何过滤配置
    let has_dual_list = blacklist_keywords.is_some() || whitelist_keywords.is_some();
    let has_legacy = keyword_filters.is_some();

    let final_videos_info = if has_dual_list || has_legacy {
        use crate::utils::keyword_filter::{should_filter_video_dual_list, should_filter_video_with_mode};

        let before_count = final_videos_info.len();
        let filtered_videos: Vec<VideoInfo> = final_videos_info
            .into_iter()
            .filter(|info| {
                let title = extract_title(info);

                // 优先使用新的双列表模式
                let should_filter = if has_dual_list {
                    should_filter_video_dual_list(&title, &blacklist_keywords, &whitelist_keywords, case_sensitive)
                } else {
                    // 向后兼容：使用旧的单列表模式
                    should_filter_video_with_mode(&title, &keyword_filters, &keyword_filter_mode)
                };

                if should_filter {
                    info!("视频 '{}' 被关键词过滤器过滤，跳过: {}", title, extract_bvid(info));
                }
                !should_filter
            })
            .collect();

        let filtered_count = before_count - filtered_videos.len();
        if filtered_count > 0 {
            info!(
                "关键词过滤完成：原视频 {} 个，过滤 {} 个，剩余 {} 个",
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
            // 选择性下载逻辑：针对 submission 类型视频源 - 需要在 into_simple_model() 之前获取信息
            let should_store_video = if let Some(selected_videos) = video_source.get_selected_videos() {
                // 获取创建时间来判断是否为新投稿
                let is_new_submission = if let Some(created_at) = video_source.get_created_at() {
                    // 如果视频发布时间晚于订阅创建时间，则为新投稿，自动下载
                    video_info.release_datetime() > &created_at
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

                        let update_model = video::ActiveModel {
                            id: Unchanged(existing.id),
                            valid: model.valid.clone(),
                            share_copy: model.share_copy.clone(),
                            show_season_type: model.show_season_type.clone(),
                            actors: model.actors.clone(),
                            name: new_name,
                            intro: model.intro.clone(),
                            cover: model.cover.clone(),
                            ..Default::default()
                        };

                        // 详细的数据库更新调试日志
                        tracing::info!(
                            "即将执行数据库更新(启用扫描删除): 视频={}, actors字段={:?}, share_copy={:?}, show_season_type={:?}",
                            existing.name, update_model.actors, update_model.share_copy, update_model.show_season_type
                        );

                        update_model.save(connection).await?;
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
                video::Entity::insert(model)
                    .on_conflict(OnConflict::new().do_nothing().to_owned())
                    .do_nothing()
                    .exec(connection)
                    .await?;
            }
        }
    } else {
        // 未启用扫描已删除视频：使用原有逻辑，但增加 share_copy 更新检查
        for video_info in final_videos_info {
            // 选择性下载逻辑：针对 submission 类型视频源 - 需要在 into_simple_model() 之前获取信息
            let should_store_video = if let Some(selected_videos) = video_source.get_selected_videos() {
                // 获取创建时间来判断是否为新投稿
                let is_new_submission = if let Some(created_at) = video_source.get_created_at() {
                    // 如果视频发布时间晚于订阅创建时间，则为新投稿，自动下载
                    video_info.release_datetime() > &created_at
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
                video::Entity::insert(model.clone())
                    .on_conflict(OnConflict::new().do_nothing().to_owned())
                    .do_nothing()
                    .exec(connection)
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

                            let update_model = video::ActiveModel {
                                id: Unchanged(existing.id),
                                valid: model.valid.clone(),
                                share_copy: model.share_copy.clone(),
                                show_season_type: model.show_season_type.clone(),
                                actors: model.actors.clone(),
                                name: new_name,
                                intro: model.intro.clone(),
                                cover: model.cover.clone(),
                                ..Default::default()
                            };

                            // 详细的数据库更新调试日志
                            tracing::info!(
                                "即将执行数据库更新(未启用扫描删除): 视频={}, actors字段={:?}, share_copy={:?}, show_season_type={:?}",
                                existing.name, update_model.actors, update_model.share_copy, update_model.show_season_type
                            );

                            update_model.save(connection).await?;
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
    video::Entity::insert_many(videos)
        .on_conflict(
            OnConflict::column(video::Column::Id)
                .update_columns([video::Column::DownloadStatus, video::Column::Path])
                .to_owned(),
        )
        .exec(connection)
        .await?;

    Ok(())
}

/// 更新视频页 model 的下载状态
pub async fn update_pages_model(pages: Vec<page::ActiveModel>, connection: &DatabaseConnection) -> Result<()> {
    let query = page::Entity::insert_many(pages).on_conflict(
        OnConflict::column(page::Column::Id)
            .update_columns([page::Column::DownloadStatus, page::Column::Path])
            .to_owned(),
    );
    query.exec(connection).await?;

    Ok(())
}
