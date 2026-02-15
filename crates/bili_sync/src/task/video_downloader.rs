use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use chrono::{Duration, NaiveDate, NaiveDateTime, NaiveTime};
use sea_orm::{ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter, Set};
use tracing::{debug, error, info, warn};

use crate::adapter::Args;
use crate::bilibili::{self, BiliClient, CollectionItem, CollectionType};
use crate::config::Config;
use crate::initialization;
use crate::task::TASK_CONTROLLER;
use crate::unified_downloader::UnifiedDownloader;
use crate::utils::file_logger;
use crate::utils::scan_collector::ScanCollector;
use crate::utils::scan_id_tracker::{
    get_last_scanned_ids, group_sources_by_new_old, update_last_scanned_ids, LastScannedIds, MaxIdRecorder, SourceType,
    VideoSourceWithId,
};
use crate::utils::time_format::{now_naive, now_standard_string, parse_time_string, STANDARD_TIME_FORMAT};
use crate::workflow::process_video_source;
use bili_sync_entity::entities;

fn submission_scan_strategy_enabled(config: &Config) -> bool {
    config.submission_scan_strategy.batch_size > 0 || config.submission_scan_strategy.adaptive_enabled
}

fn parse_time_or_min(time_str: Option<&str>) -> NaiveDateTime {
    time_str.and_then(parse_time_string).unwrap_or_else(|| {
        NaiveDateTime::new(
            NaiveDate::from_ymd_opt(1970, 1, 1).unwrap(),
            NaiveTime::from_hms_opt(0, 0, 0).unwrap(),
        )
    })
}

fn calc_submission_next_delay_secs(base_interval_secs: u64, streak: i32, max_hours: u64) -> u64 {
    let multipliers: [u64; 5] = [1, 3, 9, 36, 72];
    let idx = (streak.max(0) as usize).min(multipliers.len() - 1);
    let delay = base_interval_secs.saturating_mul(multipliers[idx]);
    let cap = max_hours.max(1).min(168).saturating_mul(3600);
    delay.min(cap).max(base_interval_secs)
}

async fn update_submission_scan_state_success(
    connection: &DatabaseConnection,
    submission_id: i32,
    new_video_count: usize,
    config: &Config,
) -> Result<()> {
    let Some(model) = entities::submission::Entity::find_by_id(submission_id)
        .one(connection)
        .await?
    else {
        return Ok(());
    };

    let now = now_naive();
    let now_str = now_standard_string();

    let base_interval_secs = config.interval;
    let max_hours = config.submission_scan_strategy.adaptive_max_hours;

    let (next_dt, new_streak) = if new_video_count > 0 {
        (now + Duration::seconds(base_interval_secs as i64), 0)
    } else {
        let streak = model.no_update_streak.saturating_add(1);
        let delay = calc_submission_next_delay_secs(base_interval_secs, streak, max_hours);
        (now + Duration::seconds(delay as i64), streak)
    };

    let mut active: entities::submission::ActiveModel = model.into();
    active.last_scan_at = Set(Some(now_str));
    active.next_scan_at = Set(Some(next_dt.format(STANDARD_TIME_FORMAT).to_string()));
    active.no_update_streak = Set(new_streak);
    active.update(connection).await?;

    Ok(())
}

async fn update_submission_scan_state_error(
    connection: &DatabaseConnection,
    submission_id: i32,
    is_risk_control: bool,
    config: &Config,
) -> Result<()> {
    let Some(model) = entities::submission::Entity::find_by_id(submission_id)
        .one(connection)
        .await?
    else {
        return Ok(());
    };

    let now = now_naive();
    let now_str = now_standard_string();

    let base_interval_secs = config.interval;
    let max_hours = config.submission_scan_strategy.adaptive_max_hours;

    let delay = if is_risk_control {
        calc_submission_next_delay_secs(base_interval_secs, 1, max_hours)
    } else {
        base_interval_secs
    };
    let next_dt = now + Duration::seconds(delay as i64);

    let mut active: entities::submission::ActiveModel = model.into();
    active.last_scan_at = Set(Some(now_str));
    active.next_scan_at = Set(Some(next_dt.format(STANDARD_TIME_FORMAT).to_string()));
    active.update(connection).await?;

    Ok(())
}

/// 从数据库加载所有视频源的函数
async fn load_video_sources_from_db(
    config: &Config,
    connection: &Arc<DatabaseConnection>,
) -> Result<Vec<VideoSourceWithId>, Box<dyn std::error::Error + Send + Sync>> {
    let mut video_sources = Vec::new();

    // 加载合集源（只加载启用的）
    let collections = entities::collection::Entity::find()
        .filter(entities::collection::Column::Enabled.eq(true))
        .all(connection.as_ref())
        .await?;

    for collection in collections {
        // 创建拥有的CollectionItem来匹配现有的Args结构
        let collection_type = if collection.r#type == 1 {
            CollectionType::Series
        } else {
            CollectionType::Season
        };

        let collection_item = CollectionItem {
            mid: collection.m_id.to_string(),
            sid: collection.s_id.to_string(),
            collection_type,
        };

        video_sources.push(VideoSourceWithId {
            id: collection.id,
            args: Args::Collection { collection_item },
            path: PathBuf::from(collection.path),
            source_type: SourceType::Collection,
        });
    }

    // 加载收藏夹源（只加载启用的）
    let favorites = entities::favorite::Entity::find()
        .filter(entities::favorite::Column::Enabled.eq(true))
        .all(connection.as_ref())
        .await?;

    for favorite in favorites {
        let fid = favorite.f_id.to_string();
        video_sources.push(VideoSourceWithId {
            id: favorite.id,
            args: Args::Favorite { fid },
            path: PathBuf::from(favorite.path),
            source_type: SourceType::Favorite,
        });
    }

    // 加载UP主投稿源（只加载启用的）
    let mut submissions = entities::submission::Entity::find()
        .filter(entities::submission::Column::Enabled.eq(true))
        .all(connection.as_ref())
        .await?;

    if submission_scan_strategy_enabled(config) && !submissions.is_empty() {
        let total_enabled_submissions = submissions.len();
        let now = now_naive();
        let tracker = crate::bilibili::submission::SUBMISSION_PAGE_TRACKER.read().unwrap();

        let mut checkpoint = Vec::new();
        let mut due = Vec::new();

        for submission in submissions.into_iter() {
            let upper_id_str = submission.upper_id.to_string();

            if tracker.contains_key(&upper_id_str) {
                checkpoint.push(submission);
                continue;
            }

            if config.submission_scan_strategy.adaptive_enabled {
                if let Some(next_scan_at) = submission.next_scan_at.as_deref() {
                    if let Some(next_dt) = parse_time_string(next_scan_at) {
                        if next_dt > now {
                            continue;
                        }
                    }
                }
            }

            due.push(submission);
        }

        if config.submission_scan_strategy.adaptive_enabled {
            due.sort_by_key(|s| (parse_time_or_min(s.next_scan_at.as_deref()), s.id));
        } else {
            due.sort_by_key(|s| (parse_time_or_min(s.last_scan_at.as_deref()), s.id));
        }
        checkpoint.sort_by_key(|s| s.id);

        let mut selected = checkpoint;
        selected.extend(due);

        let batch_size = config.submission_scan_strategy.batch_size;
        if batch_size > 0 && selected.len() > batch_size {
            selected.truncate(batch_size);
        }

        let selected_count = selected.len();
        submissions = selected;

        if selected_count < total_enabled_submissions {
            let batch_size_desc = if config.submission_scan_strategy.batch_size == 0 {
                "不限".to_string()
            } else {
                config.submission_scan_strategy.batch_size.to_string()
            };
            info!(
                "投稿源扫描策略生效：本轮扫描 {}/{} 个投稿源（每轮上限: {}，自适应: {}）",
                selected_count,
                total_enabled_submissions,
                batch_size_desc,
                config.submission_scan_strategy.adaptive_enabled
            );
        }
    }

    for submission in submissions {
        let upper_id = submission.upper_id.to_string();
        video_sources.push(VideoSourceWithId {
            id: submission.id,
            args: Args::Submission { upper_id },
            path: PathBuf::from(submission.path),
            source_type: SourceType::Submission,
        });
    }

    // 加载稍后观看源（只加载启用的）
    let watch_later_sources = entities::watch_later::Entity::find()
        .filter(entities::watch_later::Column::Enabled.eq(true))
        .all(connection.as_ref())
        .await?;

    for watch_later in watch_later_sources {
        video_sources.push(VideoSourceWithId {
            id: watch_later.id,
            args: Args::WatchLater,
            path: PathBuf::from(watch_later.path),
            source_type: SourceType::WatchLater,
        });
    }

    // 加载番剧源（只加载启用的）
    let bangumi_sources = entities::video_source::Entity::find()
        .filter(entities::video_source::Column::Type.eq(1))
        .filter(entities::video_source::Column::Enabled.eq(true))
        .all(connection.as_ref())
        .await?;

    for bangumi in bangumi_sources {
        video_sources.push(VideoSourceWithId {
            id: bangumi.id,
            args: Args::Bangumi {
                season_id: bangumi.season_id,
                media_id: bangumi.media_id,
                ep_id: bangumi.ep_id,
            },
            path: PathBuf::from(bangumi.path),
            source_type: SourceType::Bangumi,
        });
    }

    Ok(video_sources)
}

/// 统计所有视频源的数量（包括禁用的）
async fn count_all_video_sources(
    connection: &Arc<DatabaseConnection>,
) -> Result<usize, Box<dyn std::error::Error + Send + Sync>> {
    let mut total_count = 0;

    // 统计合集源
    let collections_count = entities::collection::Entity::find().count(connection.as_ref()).await?;
    total_count += collections_count as usize;

    // 统计收藏夹源
    let favorites_count = entities::favorite::Entity::find().count(connection.as_ref()).await?;
    total_count += favorites_count as usize;

    // 统计UP主投稿源
    let submissions_count = entities::submission::Entity::find().count(connection.as_ref()).await?;
    total_count += submissions_count as usize;

    // 统计稍后观看源
    let watch_later_count = entities::watch_later::Entity::find().count(connection.as_ref()).await?;
    total_count += watch_later_count as usize;

    // 统计番剧源
    let bangumi_count = entities::video_source::Entity::find()
        .filter(entities::video_source::Column::Type.eq(1))
        .count(connection.as_ref())
        .await?;
    total_count += bangumi_count as usize;

    Ok(total_count)
}

/// 初始化所有视频源的辅助函数
async fn init_all_sources(
    config: &Config,
    connection: &Arc<DatabaseConnection>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // 初始化番剧源
    if let Err(e) = initialization::init_sources(config, connection.as_ref()).await {
        error!("初始化番剧源失败: {}", e);
        return Err(e.into());
    }

    // 注意：其他源的初始化现在依赖数据库，而不是配置文件
    // 这些初始化函数可能需要修改以适应新的架构

    Ok(())
}

/// 启动周期下载视频的任务
pub async fn video_downloader(connection: Arc<DatabaseConnection>) {
    let bili_client = BiliClient::new(String::new());

    // SQLite配置已经在database::setup_database中设置了mmap，不再需要额外的初始化

    // 在启动时初始化所有视频源 - 使用动态配置而非静态CONFIG
    let config = crate::config::reload_config();

    if let Err(e) = init_all_sources(&config, &connection).await {
        error!("启动时初始化视频源失败: {}", e);
    } else {
        debug!("启动时视频源初始化成功");

        // 重新加载配置以获取最新的数据修复设置
        let config = crate::config::reload_config();

        // 检查并填充缺失的视频cid（仅在启用时执行）
        if config.enable_cid_population {
            debug!("检查是否需要填充视频cid...");
            let token = tokio_util::sync::CancellationToken::new();
            if let Err(e) = crate::workflow::populate_missing_video_cids(&bili_client, &connection, token.clone()).await
            {
                error!("填充视频cid失败: {}", e);
            }
        } else {
            debug!("视频CID填充功能已禁用，跳过检查");
        }

        // 修复page表的video_id（仅在启用时执行）
        if config.enable_startup_data_fix {
            debug!("检查是否需要修复page表的video_id...");
            if let Err(e) = crate::workflow::fix_page_video_ids(&connection).await {
                error!("修复page表video_id失败: {}", e);
            }
        } else {
            debug!("启动数据修复功能已禁用，跳过page表video_id修复");
        }
    }

    loop {
        // ========== 扫描任务阶段 ==========
        // 注意：在此阶段不应该中断任务，即使配置更新了也要等待当前扫描完成

        // 检查是否需要暂停扫描任务
        if TASK_CONTROLLER.is_paused() {
            debug!("定时扫描任务已暂停，等待恢复...");
            TASK_CONTROLLER.wait_if_paused().await;
            info!("定时扫描任务已恢复");
        }

        // 每轮开始时轮转日志文件，便于排查问题
        file_logger::rotate_log_files_for_new_round();

        // 重新加载配置
        let config = crate::config::reload_config();

        // 使用直接连接
        let optimized_connection = connection.clone();

        // 重新初始化所有视频源（确保源初始化是幂等的）
        if let Err(e) = init_all_sources(&config, &optimized_connection).await {
            error!("重新初始化视频源失败: {}", e);
            // 即使初始化失败，也继续使用现有配置进行下载
        }

        // 从数据库加载视频源，而不是从配置文件
        let enabled_sources = match load_video_sources_from_db(&config, &optimized_connection).await {
            Ok(sources) => sources,
            Err(e) => {
                error!("从数据库加载视频源失败: {}", e);
                continue;
            }
        };

        // 统计总的视频源数量（包括禁用的）
        let total_sources_count = match count_all_video_sources(&optimized_connection).await {
            Ok(count) => count,
            Err(e) => {
                warn!("统计视频源总数失败: {}", e);
                0
            }
        };

        let enabled_sources_count = enabled_sources.len();
        let disabled_sources_count = total_sources_count.saturating_sub(enabled_sources_count);

        if disabled_sources_count > 0 {
            info!(
                "开始执行本轮视频下载任务，共 {} 个视频源（启用: {}，禁用: {}）",
                total_sources_count, enabled_sources_count, disabled_sources_count
            );
        } else {
            info!("开始执行本轮视频下载任务，共 {} 个启用的视频源", enabled_sources_count);
        }

        'inner: {
            // 如果没有启用的视频源，跳过扫描
            if enabled_sources_count == 0 {
                debug!("没有启用的视频源，跳过本轮扫描");
                break 'inner;
            }

            // 在开始扫描前再次检查是否暂停
            if TASK_CONTROLLER.is_paused() {
                debug!("扫描开始前检测到暂停信号，跳过本轮扫描");
                break 'inner;
            }

            // 标记扫描开始并重置取消令牌
            TASK_CONTROLLER.set_scanning(true);
            TASK_CONTROLLER.reset_cancellation_token().await;

            // 标记任务状态为运行中
            crate::utils::task_notifier::TASK_STATUS_NOTIFIER.set_running();

            match bili_client.wbi_img().await.map(|wbi_img| wbi_img.into()) {
                Ok(Some(mixin_key)) => bilibili::set_global_mixin_key(mixin_key),
                Ok(_) => {
                    error!("解析 mixin key 失败，等待下一轮执行");
                    // 扫描失败，标记扫描结束
                    TASK_CONTROLLER.set_scanning(false);
                    crate::utils::task_notifier::TASK_STATUS_NOTIFIER.set_finished();
                    break 'inner;
                }
                Err(e) => {
                    let error_msg = format!("{:#}", e);
                    // 检查是否是登录状态过期错误（-101错误码）
                    if error_msg.contains("status code: -101") || error_msg.contains("账号未登录") {
                        warn!("检测到登录状态过期或未登录，请检查配置文件中的SESSDATA等认证信息");

                        // 发送登录状态过期日志
                        crate::api::handler::add_log_entry(
                            crate::api::handler::LogLevel::Warn,
                            "检测到登录状态过期或未登录，请更新配置文件中的SESSDATA等认证信息".to_string(),
                            Some("bili_sync::task::video_downloader".to_string()),
                        );
                    } else {
                        error!("解析 mixin key 失败: {:#}", e);

                        // 发送一般性错误日志
                        crate::api::handler::add_log_entry(
                            crate::api::handler::LogLevel::Error,
                            format!("解析 mixin key 失败: {:#}", e),
                            Some("bili_sync::task::video_downloader".to_string()),
                        );
                    }

                    // 扫描失败，标记扫描结束
                    TASK_CONTROLLER.set_scanning(false);
                    crate::utils::task_notifier::TASK_STATUS_NOTIFIER.set_finished();
                    break 'inner;
                }
            }

            // 获取/创建共享的下载器实例，避免每轮扫描重复启动 aria2
            let downloader_arc = if let Some(existing) = TASK_CONTROLLER.get_downloader().await {
                existing
            } else {
                let downloader = UnifiedDownloader::new_smart(bili_client.client.clone()).await;
                let arc = std::sync::Arc::new(downloader);
                TASK_CONTROLLER.set_downloader(Some(arc.clone())).await;
                arc
            };

            // 获取最后扫描的ID记录
            let mut last_scanned_ids = match get_last_scanned_ids(&optimized_connection).await {
                Ok(ids) => ids,
                Err(e) => {
                    warn!("获取最后扫描ID记录失败，将所有源视为旧源: {}", e);
                    LastScannedIds::default()
                }
            };

            // 将视频源按新旧分组
            let (new_sources, mut old_sources) = group_sources_by_new_old(enabled_sources.clone(), &last_scanned_ids);

            // 兜底：如果由于扫描进度记录导致本轮“没有任何可扫描源”，自动重置 last_processed_* 并改为扫描全部源。
            // 典型场景：用户在“新源优先扫描”阶段暂停/恢复，多次切换后 last_processed_* 被推进到较大的 ID，
            // 从而把较小 ID 的旧源都判定为“已处理过”而跳过，最终 old_sources.len() 变成 0。
            if new_sources.is_empty() && old_sources.is_empty() && !enabled_sources.is_empty() {
                warn!("扫描进度导致本轮无可扫描源，自动重置扫描进度并重新扫描全部源");
                last_scanned_ids.reset_all_processed_ids();
                if let Err(e) = update_last_scanned_ids(&optimized_connection, &last_scanned_ids).await {
                    warn!("重置扫描进度失败: {}", e);
                }

                old_sources = enabled_sources.clone();
                old_sources.sort_by_key(|s| s.id);
            }

            if !new_sources.is_empty() {
                info!("检测到 {} 个新添加的视频源，将优先扫描这些新源", new_sources.len());

                // 显示新源的详细信息
                for source in &new_sources {
                    let source_name = match &source.args {
                        crate::adapter::Args::Collection { .. } => "合集",
                        crate::adapter::Args::Favorite { .. } => "收藏夹",
                        crate::adapter::Args::Submission { .. } => "UP主投稿",
                        crate::adapter::Args::WatchLater => "稍后观看",
                        crate::adapter::Args::Bangumi { .. } => "番剧",
                    };
                    debug!("  - {} (ID: {})", source_name, source.id);
                }
            } else {
                info!("未检测到新添加的视频源，将按顺序扫描所有 {} 个源", old_sources.len());
            }

            // 合并新旧源，新源在前
            let ordered_sources = [new_sources, old_sources].concat();

            // 初始化扫描收集器来统计本轮扫描结果
            let mut scan_collector = ScanCollector::new();

            // 初始化ID记录器
            let mut max_id_recorder = MaxIdRecorder::new();

            let mut processed_sources = 0;
            let mut sources_with_new_content = 0;
            let mut is_first_source = true;
            let mut last_successful_source: Option<&VideoSourceWithId> = None; // 记录上一个成功处理的源
            let mut is_interrupted = false; // 标记是否因风控等原因中断

            // 定期同步相关变量
            let mut _videos_since_last_sync = 0; // 自上次同步以来处理的视频数（保留以备将来使用）

            for source in &ordered_sources {
                let args = &source.args;
                let path = &source.path;

                // 在开始扫描当前源之前，保存上一个成功处理的源ID
                if let Some(prev_source) = last_successful_source {
                    max_id_recorder.record(prev_source.source_type, prev_source.id);
                    max_id_recorder.merge_into(&mut last_scanned_ids);

                    if let Err(e) = update_last_scanned_ids(&optimized_connection, &last_scanned_ids).await {
                        warn!("保存扫描进度失败 (源ID: {}): {}", prev_source.id, e);
                    } else {
                        debug!(
                            "已保存扫描进度 (源ID: {}, 类型: {:?})",
                            prev_source.id, prev_source.source_type
                        );
                    }
                }

                // 在处理每个视频源前检查是否暂停
                if TASK_CONTROLLER.is_paused() {
                    debug!("在处理视频源时检测到暂停信号，停止当前轮次扫描");
                    // 重要：暂停时必须重置扫描状态
                    TASK_CONTROLLER.set_scanning(false);
                    crate::utils::task_notifier::TASK_STATUS_NOTIFIER.set_finished();
                    // 用户暂停不属于“风控中断”。这里不设置 is_interrupted，
                    // 让本轮结束时仍会重置 last_processed_id，避免下一轮出现“视频源数量=0”无法继续扫描的问题。
                    break;
                }

                // 视频源间延迟处理（第一个源不延迟）
                if !is_first_source {
                    let delay_seconds = match args {
                        crate::adapter::Args::Submission { .. } => {
                            // UP主投稿使用特殊延迟
                            config.submission_risk_control.submission_source_delay_seconds
                        }
                        _ => {
                            // 其他源使用通用延迟
                            config.submission_risk_control.source_delay_seconds
                        }
                    };

                    if delay_seconds > 0 {
                        let source_type = match args {
                            crate::adapter::Args::Submission { .. } => "UP主投稿",
                            crate::adapter::Args::Favorite { .. } => "收藏夹",
                            crate::adapter::Args::Collection { .. } => "合集",
                            crate::adapter::Args::WatchLater => "稍后再看",
                            crate::adapter::Args::Bangumi { .. } => "番剧",
                        };

                        info!("处理下一个{}前延迟 {} 秒，避免触发风控...", source_type, delay_seconds);
                        tokio::time::sleep(tokio::time::Duration::from_secs(delay_seconds)).await;
                    }
                }
                is_first_source = false;

                // 记录源ID
                max_id_recorder.record(source.source_type, source.id);

                // 获取全局取消令牌，用于下载任务控制
                let cancellation_token = TASK_CONTROLLER.get_cancellation_token().await;

                // 在处理视频源前记录到收集器
                if let Ok((video_source, _)) =
                    crate::adapter::video_source_from(args, path, &bili_client, &optimized_connection, None).await
                {
                    scan_collector.start_source(&video_source);
                }

                match process_video_source(
                    args,
                    &bili_client,
                    path,
                    &optimized_connection,
                    &downloader_arc,
                    cancellation_token,
                )
                .await
                {
                    Ok((new_video_count, new_videos)) => {
                        processed_sources += 1;

                        // 成功处理后，记录为上一个成功的源（不立即保存，等下次循环再保存）
                        last_successful_source = Some(source);

                        // 添加调试日志来跟踪new_videos数据传递
                        debug!(
                            "扫描完成 - new_video_count: {}, new_videos.len(): {}",
                            new_video_count,
                            new_videos.len()
                        );

                        if new_video_count > 0 {
                            sources_with_new_content += 1;
                        }

                        if submission_scan_strategy_enabled(&config) && source.source_type == SourceType::Submission {
                            if let Err(e) = update_submission_scan_state_success(
                                &optimized_connection,
                                source.id,
                                new_video_count,
                                &config,
                            )
                            .await
                            {
                                warn!("更新投稿源扫描状态失败 (ID: {}): {}", source.id, e);
                            }
                        }

                        // 检查是否有新视频信息需要添加到收集器（修复：同时检查数量和向量）
                        if !new_videos.is_empty() {
                            // 获取待删除的视频ID列表，过滤掉充电专享视频
                            let pending_delete_video_ids =
                                crate::task::VIDEO_DELETE_TASK_QUEUE.get_pending_video_ids().await;

                            // 过滤掉待删除队列中的视频
                            let filtered_videos: Vec<_> = new_videos
                                .into_iter()
                                .filter(|video| {
                                    // 如果视频有ID，检查是否在删除队列中
                                    if let Some(video_id) = video.video_id {
                                        let is_pending_delete = pending_delete_video_ids.contains(&video_id);
                                        if is_pending_delete {
                                            debug!("过滤掉待删除的充电视频: {} (ID: {})", video.title, video_id);
                                        }
                                        !is_pending_delete
                                    } else {
                                        // 如果没有video_id，保留该视频
                                        true
                                    }
                                })
                                .collect();

                            let filtered_count = filtered_videos.len();
                            let original_count = new_video_count;
                            if filtered_count < original_count {
                                info!("过滤充电视频: 原始 {} 个，过滤后 {} 个", original_count, filtered_count);
                            }

                            if !filtered_videos.is_empty() {
                                if let Ok((video_source, _)) = crate::adapter::video_source_from(
                                    args,
                                    path,
                                    &bili_client,
                                    &optimized_connection,
                                    None,
                                )
                                .await
                                {
                                    debug!(
                                        "向scan_collector添加 {} 个新视频信息（已过滤充电视频）",
                                        filtered_videos.len()
                                    );
                                    scan_collector.add_new_videos(&video_source, filtered_videos);
                                } else {
                                    warn!("无法获取视频源信息，跳过添加新视频到收集器");
                                }
                            } else {
                                debug!("所有新视频都在删除队列中，跳过推送通知");
                            }
                        } else if new_video_count > 0 {
                            warn!("发现不一致：new_video_count={} 但 new_videos 为空", new_video_count);
                        }

                        // 更新处理的视频计数
                        if new_video_count > 0 {
                            _videos_since_last_sync += new_video_count as u32;
                        }

                        // mmap自动处理数据持久化，不需要手动同步
                    }
                    Err(e) => {
                        let classified_error = crate::error::ErrorClassifier::classify_error(&e);

                        if submission_scan_strategy_enabled(&config) && source.source_type == SourceType::Submission {
                            let is_risk_control = classified_error.error_type == crate::error::ErrorType::RiskControl;
                            if let Err(err) = update_submission_scan_state_error(
                                &optimized_connection,
                                source.id,
                                is_risk_control,
                                &config,
                            )
                            .await
                            {
                                warn!("更新投稿源扫描状态失败 (ID: {}): {}", source.id, err);
                            }
                        }
                        if classified_error.error_type == crate::error::ErrorType::RiskControl {
                            error!("检测到风控，停止所有后续视频源的扫描: {}", classified_error.message);
                            info!("触发风控的源(ID: {})未完成处理，下次扫描将重新处理该源", source.id);
                            is_interrupted = true;
                            break; // 跳出循环，停止处理剩余的视频源
                        }

                        error!("处理过程遇到错误：{:#}", e);
                    }
                }
            }

            // 标记扫描结束
            TASK_CONTROLLER.set_scanning(false);

            // 保存最后一个成功处理的源ID
            if let Some(final_source) = last_successful_source {
                max_id_recorder.record(final_source.source_type, final_source.id);
                max_id_recorder.merge_into(&mut last_scanned_ids);

                // 如果没有被中断，说明扫描完了所有源，需要重置last_processed_id以实现循环
                if !is_interrupted {
                    debug!("本轮扫描完成所有源，重置处理ID以便下次从头开始循环");
                    last_scanned_ids.reset_all_processed_ids();
                }

                if let Err(e) = update_last_scanned_ids(&optimized_connection, &last_scanned_ids).await {
                    warn!("保存最后源的扫描进度失败 (源ID: {}): {}", final_source.id, e);
                } else {
                    debug!(
                        "已保存最后源的扫描进度 (源ID: {}, 类型: {:?})",
                        final_source.id, final_source.source_type
                    );
                }
            }

            debug!("扫描完成，所有进度已保存");

            // mmap自动处理数据持久化，不需要手动同步

            info!("本轮扫描完成 - 视频源数量: {}", ordered_sources.len());

            // 生成扫描摘要并发送推送通知
            let scan_summary = scan_collector.generate_summary();
            if let Err(e) = crate::utils::notification::send_scan_notification(scan_summary).await {
                warn!("发送扫描完成推送失败: {}", e);
            }

            // 标记任务状态为结束
            crate::utils::task_notifier::TASK_STATUS_NOTIFIER.set_finished();

            if processed_sources == ordered_sources.len() {
                if sources_with_new_content > 0 {
                    info!(
                        "本轮任务执行完毕，成功扫描 {} 个视频源，其中 {} 个源有新内容",
                        processed_sources, sources_with_new_content
                    );
                } else {
                    info!(
                        "本轮任务执行完毕，成功扫描 {} 个视频源（均无新内容）",
                        processed_sources
                    );
                }
            } else if processed_sources > 0 {
                if sources_with_new_content > 0 {
                    info!(
                        "本轮任务执行完毕，成功扫描 {} 个视频源（其中 {} 个有新内容），{} 个源处理失败",
                        processed_sources,
                        sources_with_new_content,
                        ordered_sources.len() - processed_sources
                    );
                } else {
                    info!(
                        "本轮任务执行完毕，成功扫描 {} 个视频源（均无新内容），{} 个源处理失败",
                        processed_sources,
                        ordered_sources.len() - processed_sources
                    );
                }
            } else {
                warn!("本轮任务执行完毕，所有 {} 个视频源均处理失败", ordered_sources.len());
            }
        }

        // ========== 扫描后处理阶段 ==========
        // 扫描完成，刷新所有缓冲的日志到文件
        file_logger::flush_file_logger();
        // 只在未暂停时处理后续任务
        if !TASK_CONTROLLER.is_paused() {
            // 安全时机：扫描任务已完成，处理暂存的删除任务
            if let Err(e) = crate::task::process_delete_tasks(connection.clone()).await {
                error!("处理删除任务队列失败: {:#}", e);
            }

            // 处理暂存的视频删除任务
            if let Err(e) = crate::task::process_video_delete_tasks(connection.clone()).await {
                error!("处理视频删除任务队列失败: {:#}", e);
            }

            // 处理暂存的添加任务
            if let Err(e) = crate::task::process_add_tasks(connection.clone()).await {
                error!("处理添加任务队列失败: {:#}", e);
            }

            // 处理暂存的配置任务
            if let Err(e) = crate::task::process_config_tasks(connection.clone()).await {
                error!("处理配置任务队列失败: {:#}", e);
            }

            // mmap自动处理数据持久化，不需要手动同步
        } else {
            debug!("任务已暂停，跳过后处理阶段");
        }

        // ========== 等待阶段 ==========
        // 安全时机：扫描任务已完成，可以安全地检测配置更新并决定是否立即开始下一轮
        // 智能等待：支持配置更新的间隔等待
        // 重要：只在扫描任务完成后才检测配置更新，确保不会中断正在进行的扫描
        let wait_interval = config.interval;
        let check_frequency = 5; // 每5秒检查一次配置是否更新
        let mut remaining_time = wait_interval;

        info!("本轮扫描任务已完成，开始等待 {} 秒后进行下一轮扫描", wait_interval);

        while remaining_time > 0 {
            // 检查是否暂停
            if TASK_CONTROLLER.is_paused() {
                debug!("等待期间检测到暂停信号，等待恢复...");
                TASK_CONTROLLER.wait_if_paused().await;

                // 检查是否刚刚恢复，如果是则立即开始新扫描
                if TASK_CONTROLLER.take_just_resumed() {
                    info!("任务恢复，立即开始新一轮扫描");
                    break; // 跳出等待循环，立即开始新扫描
                }

                info!("等待期间暂停任务已恢复，继续等待");
                continue; // 暂停期间不计入等待时间
            }

            let sleep_duration = remaining_time.min(check_frequency);
            tokio::time::sleep(tokio::time::Duration::from_secs(sleep_duration)).await;
            remaining_time = remaining_time.saturating_sub(sleep_duration);

            // 检查是否刚刚恢复，如果是则立即开始新扫描
            if TASK_CONTROLLER.take_just_resumed() {
                info!("检测到任务恢复信号，立即开始新一轮扫描");
                break; // 跳出等待循环，立即开始新扫描
            }

            // 检查配置是否更新了（通过比较interval值）
            let current_config = crate::config::reload_config();
            if current_config.interval != wait_interval {
                info!(
                    "检测到扫描间隔时间配置更新：{} -> {} 秒，等待本轮结束后立即开始下一轮扫描",
                    wait_interval, current_config.interval
                );
                break; // 配置更新了，立即开始下一轮
            }

            // 显示剩余等待时间（只在较长等待时显示）
            if remaining_time > 0 && remaining_time % 30 == 0 && remaining_time >= 30 {
                debug!("距离下一轮扫描还有 {} 秒", remaining_time);
            }
        }
    }
}
