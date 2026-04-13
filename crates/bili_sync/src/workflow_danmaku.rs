//! 弹幕增量更新工作流。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use bili_sync_entity::{collection, favorite, page, submission, video, video_source, watch_later};
use chrono::{DateTime, TimeZone, Utc};
use sea_orm::entity::prelude::*;
use sea_orm::{ActiveModelTrait, Condition, QueryFilter, QuerySelect, Set};
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::bilibili::{
    parse_event_name, BiliClient, DanmakuElem, DanmakuWriter, Dimension, PageInfo as BiliPageInfo, Video, VideoInfo,
};
use crate::config::Config;
use crate::utils::danmaku_schedule::{should_sync_danmaku, Decision, Stage};
use crate::utils::status::STATUS_OK;
use crate::utils::time_format::{beijing_timezone, parse_time_string, to_standard_string};

/// 弹幕子任务在 download_status 中的位偏移（与 PageStatus 保持一致）。
const DANMAKU_STATUS_OFFSET: usize = 3;

#[derive(Debug, Default)]
struct ExistingDanmakuCursor {
    max_sent_at: Option<i64>,
    known_source_ids: HashSet<String>,
}

fn is_bili_request_failed_with_codes(err: &anyhow::Error, codes: &[i64]) -> bool {
    err.chain().any(|cause| {
        cause.downcast_ref::<crate::bilibili::BiliError>().is_some_and(|e| match e {
            crate::bilibili::BiliError::RequestFailed(code, _) => codes.contains(code),
            _ => false,
        })
    })
}

fn should_fallback_to_stored_pages(err: &anyhow::Error) -> bool {
    is_bili_request_failed_with_codes(err, &[-404, 62002, 62012])
}

fn build_stored_page_info(page_model: &page::Model) -> Result<BiliPageInfo> {
    let cid = page_model.danmaku_cid_snapshot.unwrap_or(page_model.cid);
    if cid <= 0 {
        bail!("分页 pid={} 缺少可用 cid，无法回退到数据库分页信息刷新弹幕", page_model.pid);
    }

    let dimension = match (page_model.width, page_model.height) {
        (Some(width), Some(height)) => Some(Dimension {
            width,
            height,
            rotate: 0,
        }),
        _ => None,
    };

    Ok(BiliPageInfo {
        cid,
        page: page_model.pid,
        name: page_model.name.clone(),
        duration: page_model.duration,
        first_frame: None,
        dimension,
    })
}

async fn load_fresh_pages_or_fallback(
    bili_video: &Video<'_>,
    video_model: &video::Model,
    db_pages: &[page::Model],
    error_context: &str,
) -> Result<Vec<BiliPageInfo>> {
    match bili_video.get_view_info().await {
        Ok(VideoInfo::Detail { pages, .. }) => Ok(pages),
        Ok(_) => {
            warn!(
                "视频「{}」({}) 的 view_info 返回了非 Detail 类型，改用数据库已存分页信息继续刷新弹幕",
                video_model.name, video_model.bvid
            );
            db_pages.iter().map(build_stored_page_info).collect()
        }
        Err(err) if should_fallback_to_stored_pages(&err) => {
            warn!(
                "视频「{}」({}) 获取 view_info 失败，改用数据库已存分页信息继续刷新弹幕：{:#}",
                video_model.name, video_model.bvid, err
            );
            db_pages.iter().map(build_stored_page_info).collect()
        }
        Err(err) => Err(err).with_context(|| error_context.to_string()),
    }
}

pub async fn refresh_danmaku_incremental(
    bili_client: &BiliClient,
    connection: &DatabaseConnection,
    config: &Config,
) -> Result<()> {
    if !config.danmaku_update_policy.enabled {
        return Ok(());
    }

    info!("开始执行本轮弹幕增量更新");

    let candidates = load_candidate_videos(connection).await?;
    let now = Utc::now();
    let mut processed = 0usize;
    let mut refreshed = 0usize;

    for (video_model, pages) in candidates {
        let pubtime = stored_beijing_naive_to_utc(video_model.pubtime);
        let selected = pages
            .into_iter()
            .filter_map(|page_model| {
                let last_synced = page_model
                    .danmaku_last_synced_at
                    .as_deref()
                    .and_then(parse_stored_datetime);

                match should_sync_danmaku(
                    &config.danmaku_update_policy,
                    pubtime,
                    last_synced,
                    page_model.danmaku_sync_generation,
                    now,
                ) {
                    Decision::Sync { next_stage } => Some((page_model, Some(next_stage))),
                    Decision::Skip => None,
                }
            })
            .collect::<Vec<_>>();

        if selected.is_empty() {
            continue;
        }

        match refresh_video_pages(bili_client, connection, config, &video_model, selected, now).await {
            Ok(count) => {
                processed += 1;
                refreshed += count;
            }
            Err(err) => {
                error!(
                    "刷新视频「{}」({}) 的弹幕失败：{:#}",
                    video_model.name, video_model.bvid, err
                );
            }
        }
    }

    info!("弹幕增量更新结束：处理视频 {} 个，刷新分页 {} 个", processed, refreshed);
    Ok(())
}

pub async fn refresh_danmaku_for_video(
    video_id: i32,
    bili_client: &BiliClient,
    connection: &DatabaseConnection,
    config: &Config,
) -> Result<usize> {
    let video_model = video::Entity::find_by_id(video_id)
        .one(connection)
        .await?
        .ok_or_else(|| anyhow!("video {} 不存在", video_id))?;
    let pages = page::Entity::find()
        .filter(page::Column::VideoId.eq(video_id))
        .all(connection)
        .await?;

    if pages.is_empty() {
        return Ok(0);
    }

    let now = Utc::now();
    let selected = pages.into_iter().map(|page_model| (page_model, None)).collect();
    refresh_video_pages(bili_client, connection, config, &video_model, selected, now).await
}

pub async fn refresh_danmaku_for_page(
    page_id: i32,
    bili_client: &BiliClient,
    connection: &DatabaseConnection,
    config: &Config,
) -> Result<usize> {
    let page_model = page::Entity::find_by_id(page_id)
        .one(connection)
        .await?
        .ok_or_else(|| anyhow!("page {} 不存在", page_id))?;
    let video_model = video::Entity::find_by_id(page_model.video_id)
        .one(connection)
        .await?
        .ok_or_else(|| anyhow!("page {} 的宿主 video 不存在", page_id))?;

    let bili_video = Video::new(bili_client, video_model.bvid.clone());
    let fresh_pages = load_fresh_pages_or_fallback(
        &bili_video,
        &video_model,
        std::slice::from_ref(&page_model),
        &format!("获取视频 {} 的 view_info 失败", video_model.bvid),
    )
    .await?;
    let fresh = fresh_pages
        .iter()
        .find(|page_info| page_info.page == page_model.pid)
        .ok_or_else(|| {
            anyhow!(
                "视频「{}」({}) 的分页 pid={} 在最新 view_info 中已不存在",
                video_model.name,
                video_model.bvid,
                page_model.pid
            )
        })?;

    refresh_one_page(
        &bili_video,
        connection,
        config,
        &video_model,
        page_model,
        fresh,
        None,
        Utc::now(),
    )
    .await?;

    Ok(1)
}

async fn load_candidate_videos(connection: &DatabaseConnection) -> Result<Vec<(video::Model, Vec<page::Model>)>> {
    let favorite_ids: Vec<i32> = favorite::Entity::find()
        .filter(favorite::Column::Enabled.eq(true))
        .filter(favorite::Column::DownloadDanmaku.eq(true))
        .select_only()
        .column(favorite::Column::Id)
        .into_tuple()
        .all(connection)
        .await
        .context("加载启用的收藏夹源失败")?;
    let collection_ids: Vec<i32> = collection::Entity::find()
        .filter(collection::Column::Enabled.eq(true))
        .filter(collection::Column::DownloadDanmaku.eq(true))
        .select_only()
        .column(collection::Column::Id)
        .into_tuple()
        .all(connection)
        .await
        .context("加载启用的合集源失败")?;
    let submission_ids: Vec<i32> = submission::Entity::find()
        .filter(submission::Column::Enabled.eq(true))
        .filter(submission::Column::DownloadDanmaku.eq(true))
        .select_only()
        .column(submission::Column::Id)
        .into_tuple()
        .all(connection)
        .await
        .context("加载启用的投稿源失败")?;
    let watch_later_ids: Vec<i32> = watch_later::Entity::find()
        .filter(watch_later::Column::Enabled.eq(true))
        .filter(watch_later::Column::DownloadDanmaku.eq(true))
        .select_only()
        .column(watch_later::Column::Id)
        .into_tuple()
        .all(connection)
        .await
        .context("加载启用的稍后再看源失败")?;
    let bangumi_ids: Vec<i32> = video_source::Entity::find()
        .filter(video_source::Column::Type.eq(1))
        .filter(video_source::Column::Enabled.eq(true))
        .filter(video_source::Column::DownloadDanmaku.eq(true))
        .select_only()
        .column(video_source::Column::Id)
        .into_tuple()
        .all(connection)
        .await
        .context("加载启用的番剧源失败")?;

    if favorite_ids.is_empty()
        && collection_ids.is_empty()
        && submission_ids.is_empty()
        && watch_later_ids.is_empty()
        && bangumi_ids.is_empty()
    {
        return Ok(Vec::new());
    }

    let mut source_filter = Condition::any();
    if !favorite_ids.is_empty() {
        source_filter = source_filter.add(video::Column::FavoriteId.is_in(favorite_ids));
    }
    if !collection_ids.is_empty() {
        source_filter = source_filter.add(video::Column::CollectionId.is_in(collection_ids));
    }
    if !submission_ids.is_empty() {
        source_filter = source_filter.add(video::Column::SubmissionId.is_in(submission_ids));
    }
    if !watch_later_ids.is_empty() {
        source_filter = source_filter.add(video::Column::WatchLaterId.is_in(watch_later_ids));
    }
    if !bangumi_ids.is_empty() {
        source_filter = source_filter.add(
            Condition::all()
                .add(video::Column::SourceType.eq(1))
                .add(video::Column::SourceId.is_in(bangumi_ids)),
        );
    }

    video::Entity::find()
        .filter(Condition::all().add(video::Column::Valid.eq(true)).add(source_filter))
        .find_with_related(page::Entity)
        .all(connection)
        .await
        .context("加载弹幕增量更新候选视频失败")
        .map(|rows| {
            rows.into_iter()
                .map(|(video_model, pages)| {
                    let filtered_pages = pages
                        .into_iter()
                        .filter(|page_model| {
                            danmaku_subtask_completed(page_model.download_status)
                                && page_model.path.as_deref().is_some_and(|path| !path.is_empty())
                        })
                        .collect::<Vec<_>>();
                    (video_model, filtered_pages)
                })
                .filter(|(_, pages)| !pages.is_empty())
                .collect()
        })
}

fn danmaku_subtask_completed(status: u32) -> bool {
    let slot = (status >> (DANMAKU_STATUS_OFFSET * 3)) & 0b111;
    slot == STATUS_OK
}

fn reset_non_danmaku_subtasks(status: u32) -> u32 {
    let mut next_status = status;
    for offset in 0..5 {
        if offset == DANMAKU_STATUS_OFFSET {
            continue;
        }
        next_status &= !(0b111 << (offset * 3));
    }
    next_status & !(1 << 31)
}

fn reset_video_for_page_redownload(status: u32) -> u32 {
    const PAGE_DOWNLOAD_OFFSET: usize = 4;
    let cleared = status & !(0b111 << (PAGE_DOWNLOAD_OFFSET * 3));
    cleared & !(1 << 31)
}

async fn refresh_video_pages(
    bili_client: &BiliClient,
    connection: &DatabaseConnection,
    config: &Config,
    video_model: &video::Model,
    selected: Vec<(page::Model, Option<Stage>)>,
    now: DateTime<Utc>,
) -> Result<usize> {
    let bili_video = Video::new(bili_client, video_model.bvid.clone());
    let selected_pages = selected
        .iter()
        .map(|(page_model, _)| page_model.clone())
        .collect::<Vec<_>>();
    let fresh_pages = load_fresh_pages_or_fallback(
        &bili_video,
        video_model,
        &selected_pages,
        &format!("刷新视频 {} 时获取 view_info 失败", video_model.bvid),
    )
    .await?;

    let mut success = 0usize;
    for (db_page, next_stage) in selected {
        let fresh = fresh_pages.iter().find(|page_info| page_info.page == db_page.pid);
        let Some(fresh) = fresh else {
            warn!(
                "视频「{}」({}) 的分页 pid={} 在最新 view_info 中不存在，跳过",
                video_model.name, video_model.bvid, db_page.pid
            );
            continue;
        };

        if let Err(err) = refresh_one_page(
            &bili_video,
            connection,
            config,
            video_model,
            db_page,
            fresh,
            next_stage,
            now,
        )
        .await
        {
            error!(
                "刷新视频「{}」({}) 分页 pid={} 弹幕失败：{:#}",
                video_model.name, video_model.bvid, fresh.page, err
            );
            continue;
        }
        success += 1;
    }

    Ok(success)
}

async fn refresh_one_page(
    bili_video: &Video<'_>,
    connection: &DatabaseConnection,
    config: &Config,
    video_model: &video::Model,
    db_page: page::Model,
    fresh: &BiliPageInfo,
    next_stage: Option<Stage>,
    now: DateTime<Utc>,
) -> Result<()> {
    if fresh.cid <= 0 {
        bail!("分页 pid={} 返回了无效 cid", fresh.page);
    }

    let pubtime = stored_beijing_naive_to_utc(video_model.pubtime);
    let resolved_stage = next_stage.unwrap_or_else(|| {
        crate::utils::danmaku_schedule::stage_for_age(&config.danmaku_update_policy, pubtime, now, false)
    });
    let danmaku_path = resolve_danmaku_path(&db_page)?;
    let (fresh_width, fresh_height) = extract_dimension(fresh.dimension.as_ref());
    let fresh_duration = if fresh.duration > 0 {
        fresh.duration
    } else {
        db_page.duration
    };
    let fresh_name = if fresh.name.is_empty() {
        db_page.name.clone()
    } else {
        fresh.name.clone()
    };

    let cid_changed = db_page.cid != fresh.cid;
    let duration_changed = db_page.duration != fresh_duration;
    let dimension_changed = fresh_width != db_page.width || fresh_height != db_page.height;
    let name_changed = db_page.name != fresh_name;

    if cid_changed {
        warn!(
            "检测到视频「{}」({}) 分页 pid={} 的 cid 发生变化 ({} -> {})，已重置相关下载状态",
            video_model.name, video_model.bvid, fresh.page, db_page.cid, fresh.cid
        );
    }

    let page_info_for_danmaku = BiliPageInfo {
        cid: fresh.cid,
        page: fresh.page,
        name: fresh_name.clone(),
        duration: fresh_duration,
        first_frame: fresh.first_frame.clone(),
        dimension: fresh.dimension.as_ref().map(|dimension| Dimension {
            width: dimension.width,
            height: dimension.height,
            rotate: dimension.rotate,
        }),
    };

    let last_synced = db_page
        .danmaku_last_synced_at
        .as_deref()
        .and_then(parse_stored_datetime);
    let danmaku_elems = bili_video
        .get_danmaku_elements(&page_info_for_danmaku, CancellationToken::new())
        .await?;
    let file_exists = tokio::fs::metadata(&danmaku_path).await.is_ok();
    let fetched_danmaku_count = danmaku_elems.len() as u32;
    let last_write_count = if file_exists && !cid_changed {
        let existing_cursor = load_existing_danmaku_cursor(&danmaku_path).await?;
        let cutoff = incremental_cutoff_timestamp(&danmaku_path, last_synced, &existing_cursor).await?;
        let incremental_elems = filter_incremental_danmaku(danmaku_elems, cutoff, &existing_cursor);
        let incremental_count = incremental_elems.len() as u32;
        if !incremental_elems.is_empty() {
            let writer = DanmakuWriter::new(
                &page_info_for_danmaku,
                incremental_elems.into_iter().map(Into::into).collect(),
            );
            writer.append(danmaku_path.clone()).await?;
        }
        incremental_count
    } else {
        let tmp_path = make_tmp_path(&danmaku_path);
        let writer = DanmakuWriter::new(
            &page_info_for_danmaku,
            danmaku_elems.into_iter().map(Into::into).collect(),
        );
        writer.write(tmp_path.clone()).await?;
        tokio::fs::rename(&tmp_path, &danmaku_path)
            .await
            .with_context(|| format!("重命名弹幕文件 {:?} -> {:?} 失败", tmp_path, danmaku_path))?;
        fetched_danmaku_count
    };

    let now_str = to_standard_string(now.with_timezone(&beijing_timezone()));
    let mut active: page::ActiveModel = db_page.clone().into();
    active.danmaku_last_synced_at = Set(Some(now_str));
    active.danmaku_sync_generation = Set(resolved_stage.as_generation());
    active.danmaku_cid_snapshot = Set(Some(fresh.cid));
    active.danmaku_last_write_count = Set(last_write_count);

    if cid_changed {
        active.cid = Set(fresh.cid);
        active.download_status = Set(reset_non_danmaku_subtasks(db_page.download_status));
        active.file_size_bytes = Set(None);
        active.video_stream_size_bytes = Set(None);
        active.audio_stream_size_bytes = Set(None);
        active.play_video_streams = Set(None);
        active.play_audio_streams = Set(None);
        active.play_subtitle_streams = Set(None);
        active.play_streams_updated_at = Set(None);

        let mut video_active: video::ActiveModel = video_model.clone().into();
        let new_video_status = reset_video_for_page_redownload(video_model.download_status);
        let mut should_update_video = false;
        if new_video_status != video_model.download_status {
            video_active.download_status = Set(new_video_status);
            should_update_video = true;
        }
        if video_model.single_page == Some(true) && video_model.cid != Some(fresh.cid) {
            video_active.cid = Set(Some(fresh.cid));
            should_update_video = true;
        }
        if should_update_video {
            video_active
                .update(connection)
                .await
                .context("cid 变化后更新 video 状态失败")?;
        }
    }

    if duration_changed {
        active.duration = Set(fresh_duration);
    }
    if dimension_changed {
        active.width = Set(fresh_width);
        active.height = Set(fresh_height);
    }
    if name_changed {
        active.name = Set(fresh_name);
    }

    active.update(connection).await.context("更新 page 弹幕同步状态失败")?;

    info!(
        "视频「{}」({}) 分页 pid={} 弹幕已刷新 -> stage={:?}",
        video_model.name, video_model.bvid, fresh.page, resolved_stage
    );

    Ok(())
}

fn extract_dimension(dimension: Option<&Dimension>) -> (Option<u32>, Option<u32>) {
    match dimension {
        Some(dimension) if dimension.rotate == 0 => (Some(dimension.width), Some(dimension.height)),
        Some(dimension) => (Some(dimension.height), Some(dimension.width)),
        None => (None, None),
    }
}

fn resolve_danmaku_path(page_model: &page::Model) -> Result<PathBuf> {
    let video_path = page_model
        .path
        .as_deref()
        .filter(|path| !path.is_empty())
        .ok_or_else(|| anyhow!("page 未记录下载路径，无法推断弹幕位置"))?;
    let video_path = Path::new(video_path);
    let parent = video_path.parent().context("invalid page path format")?;
    let file_stem = video_path
        .file_stem()
        .context("invalid page path format")?
        .to_string_lossy();
    Ok(parent.join(format!("{}.zh-CN.default.ass", file_stem)))
}

fn make_tmp_path(target: &Path) -> PathBuf {
    let mut value = target.as_os_str().to_os_string();
    value.push(".tmp");
    PathBuf::from(value)
}

async fn load_existing_danmaku_cursor(path: &Path) -> Result<ExistingDanmakuCursor> {
    let content = String::from_utf8_lossy(&tokio::fs::read(path).await?).into_owned();
    Ok(parse_existing_danmaku_cursor(&content))
}

fn parse_existing_danmaku_cursor(content: &str) -> ExistingDanmakuCursor {
    let mut cursor = ExistingDanmakuCursor::default();
    for line in content.lines() {
        let Some(value) = line.strip_prefix("Dialogue: ") else {
            continue;
        };
        let mut columns = value.splitn(10, ',');
        let _layer = columns.next();
        let _start = columns.next();
        let _end = columns.next();
        let _style = columns.next();
        let Some(name) = columns.next() else {
            continue;
        };
        if let Some((source_id, sent_at)) = parse_event_name(name) {
            cursor.max_sent_at = Some(cursor.max_sent_at.map_or(sent_at, |current| current.max(sent_at)));
            cursor.known_source_ids.insert(source_id.to_string());
        }
    }
    cursor
}

async fn incremental_cutoff_timestamp(
    danmaku_path: &Path,
    last_synced: Option<DateTime<Utc>>,
    existing_cursor: &ExistingDanmakuCursor,
) -> Result<Option<i64>> {
    let mut cutoff = last_synced.map(|value| value.timestamp());
    if let Some(max_sent_at) = existing_cursor.max_sent_at {
        cutoff = Some(cutoff.map_or(max_sent_at, |value| value.max(max_sent_at)));
    }
    if cutoff.is_some() {
        return Ok(cutoff);
    }

    let modified = tokio::fs::metadata(danmaku_path)
        .await?
        .modified()
        .ok()
        .map(DateTime::<Utc>::from)
        .map(|value| value.timestamp());
    Ok(modified)
}

fn filter_incremental_danmaku(
    danmaku_elems: Vec<DanmakuElem>,
    cutoff: Option<i64>,
    existing_cursor: &ExistingDanmakuCursor,
) -> Vec<DanmakuElem> {
    danmaku_elems
        .into_iter()
        .filter(|elem| {
            let Some(source_id) = normalized_source_id(elem) else {
                return match cutoff {
                    Some(value) => elem.ctime > value,
                    None => true,
                };
            };
            if existing_cursor.known_source_ids.contains(&source_id) {
                return false;
            }

            match cutoff {
                Some(value) if elem.ctime > value => true,
                Some(value) if existing_cursor.max_sent_at == Some(value) && elem.ctime == value => true,
                Some(_) => false,
                None => true,
            }
        })
        .collect()
}

fn normalized_source_id(elem: &DanmakuElem) -> Option<String> {
    if !elem.dmid_str.is_empty() {
        Some(elem.dmid_str.clone())
    } else if elem.id > 0 {
        Some(elem.id.to_string())
    } else {
        None
    }
}

fn stored_beijing_naive_to_utc(value: chrono::NaiveDateTime) -> DateTime<Utc> {
    beijing_timezone()
        .from_local_datetime(&value)
        .single()
        .map(|dt| dt.with_timezone(&Utc))
        .unwrap_or_else(|| Utc.from_utc_datetime(&value))
}

fn parse_stored_datetime(value: &str) -> Option<DateTime<Utc>> {
    parse_time_string(value).map(stored_beijing_naive_to_utc)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn danmaku_completed_detects_ok() {
        let with_danmaku_ok: u32 = STATUS_OK << 9;
        assert!(danmaku_subtask_completed(with_danmaku_ok));
        let without: u32 = STATUS_OK << 6;
        assert!(!danmaku_subtask_completed(without));
    }

    #[test]
    fn reset_non_danmaku_subtasks_keeps_only_danmaku_ok() {
        let all_ok_completed: u32 = (1u32 << 31)
            | (0..5)
                .map(|index| STATUS_OK << (index * 3))
                .fold(0u32, |acc, value| acc | value);
        let reset = reset_non_danmaku_subtasks(all_ok_completed);
        assert_eq!((reset >> 9) & 0b111, STATUS_OK);
        for index in [0usize, 1, 2, 4] {
            assert_eq!((reset >> (index * 3)) & 0b111, 0);
        }
        assert_eq!(reset >> 31, 0);
    }

    #[test]
    fn reset_video_for_page_redownload_clears_page_download_and_completed_bit() {
        let video_done: u32 = (1u32 << 31)
            | (0..5)
                .map(|index| STATUS_OK << (index * 3))
                .fold(0u32, |acc, value| acc | value);
        let reset = reset_video_for_page_redownload(video_done);
        assert_eq!((reset >> 12) & 0b111, 0);
        for index in [0usize, 1, 2, 3] {
            assert_eq!((reset >> (index * 3)) & 0b111, STATUS_OK);
        }
        assert_eq!(reset >> 31, 0);
    }

    #[test]
    fn parse_stored_datetime_uses_beijing_timezone() {
        let parsed = parse_stored_datetime("2026-04-13 10:20:30").expect("parse ok");
        assert_eq!(parsed, Utc.with_ymd_and_hms(2026, 4, 13, 2, 20, 30).unwrap());
    }

    #[test]
    fn parse_existing_danmaku_cursor_collects_metadata() {
        let cursor = parse_existing_danmaku_cursor(
            "[Events]\n\
             Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\n\
             Dialogue: 2,0:00:01.00,0:00:05.00,Float,bsync-dm|100|1710000000,0,0,0,,foo\n\
             Dialogue: 2,0:00:02.00,0:00:06.00,Float,bsync-dm|101|1710001234,0,0,0,,bar\n",
        );
        assert_eq!(cursor.max_sent_at, Some(1710001234));
        assert!(cursor.known_source_ids.contains("100"));
        assert!(cursor.known_source_ids.contains("101"));
    }

    #[test]
    fn filter_incremental_danmaku_uses_cutoff_and_dedup() {
        let existing_cursor = ExistingDanmakuCursor {
            max_sent_at: Some(1710000100),
            known_source_ids: ["100".to_string()].into_iter().collect(),
        };
        let filtered = filter_incremental_danmaku(
            vec![
                DanmakuElem {
                    id: 100,
                    progress: 1000,
                    mode: 1,
                    fontsize: 25,
                    color: 0xffffff,
                    mid_hash: String::new(),
                    content: "old".to_string(),
                    ctime: 1710000000,
                    weight: 0,
                    action: String::new(),
                    pool: 0,
                    dmid_str: "100".to_string(),
                    attr: 0,
                },
                DanmakuElem {
                    id: 101,
                    progress: 1200,
                    mode: 1,
                    fontsize: 25,
                    color: 0xffffff,
                    mid_hash: String::new(),
                    content: "same-second-new".to_string(),
                    ctime: 1710000100,
                    weight: 0,
                    action: String::new(),
                    pool: 0,
                    dmid_str: "101".to_string(),
                    attr: 0,
                },
                DanmakuElem {
                    id: 102,
                    progress: 1300,
                    mode: 1,
                    fontsize: 25,
                    color: 0xffffff,
                    mid_hash: String::new(),
                    content: "new".to_string(),
                    ctime: 1710000200,
                    weight: 0,
                    action: String::new(),
                    pool: 0,
                    dmid_str: "102".to_string(),
                    attr: 0,
                },
            ],
            Some(1710000100),
            &existing_cursor,
        );

        assert_eq!(filtered.len(), 2);
        assert!(filtered.iter().any(|elem| elem.dmid_str == "101"));
        assert!(filtered.iter().any(|elem| elem.dmid_str == "102"));
    }

    #[test]
    fn should_fallback_to_stored_pages_accepts_62012() {
        let err = anyhow!(crate::bilibili::BiliError::RequestFailed(62012, "62012".to_string()));
        assert!(should_fallback_to_stored_pages(&err));
    }
}
