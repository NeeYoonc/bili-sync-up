//! 弹幕增量更新工作流。

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use bili_sync_entity::{collection, favorite, page, submission, video, video_source, watch_later};
use chrono::{DateTime, TimeZone, Utc};
use sea_orm::entity::prelude::*;
use sea_orm::sea_query::SimpleExpr;
use sea_orm::{ActiveModelTrait, Condition, QueryFilter, QuerySelect, Set};
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::bilibili::{parse_event_name, DanmakuElem, DanmakuWriter, Dimension, PageInfo as BiliPageInfo, Video};
use crate::config::Config;
use crate::utils::danmaku_schedule::{should_sync_danmaku, stage_for_age, Decision, Stage};
use crate::utils::status::{PageStatus, VideoStatus, STATUS_OK};
use crate::utils::time_format::{beijing_timezone, parse_time_string, to_standard_string};

/// 弹幕子任务在 download_status 中的位偏移（与 PageStatus 保持一致）。
const DANMAKU_STATUS_OFFSET: usize = 3;

#[derive(Debug, Default)]
struct ExistingDanmakuCursor {
    max_sent_at: Option<i64>,
    known_source_ids: HashSet<String>,
}

#[derive(Debug, Clone)]
pub struct PageDanmakuSyncUpdate {
    pub danmaku_last_synced_at: String,
    pub danmaku_sync_generation: u32,
    pub danmaku_cid_snapshot: i64,
    pub danmaku_last_write_count: u32,
    pub duration: Option<u32>,
    pub width: Option<Option<u32>>,
    pub height: Option<Option<u32>>,
    pub name: Option<String>,
}

impl PageDanmakuSyncUpdate {
    pub fn apply_to_active_model(&self, active: &mut page::ActiveModel) {
        active.danmaku_last_synced_at = Set(Some(self.danmaku_last_synced_at.clone()));
        active.danmaku_sync_generation = Set(self.danmaku_sync_generation);
        active.danmaku_cid_snapshot = Set(Some(self.danmaku_cid_snapshot));
        active.danmaku_last_write_count = Set(self.danmaku_last_write_count);

        if let Some(duration) = self.duration {
            active.duration = Set(duration);
        }
        if let Some(width) = self.width {
            active.width = Set(width);
        }
        if let Some(height) = self.height {
            active.height = Set(height);
        }
        if let Some(name) = &self.name {
            active.name = Set(name.clone());
        }
    }
}

#[cfg(test)]
fn is_bili_request_failed_with_codes(err: &anyhow::Error, codes: &[i64]) -> bool {
    err.chain().any(|cause| {
        cause
            .downcast_ref::<crate::bilibili::BiliError>()
            .is_some_and(|e| match e {
                crate::bilibili::BiliError::RequestFailed(code, _) => codes.contains(code),
                _ => false,
            })
    })
}

#[cfg(test)]
fn should_fallback_to_stored_pages(err: &anyhow::Error) -> bool {
    is_bili_request_failed_with_codes(err, &[-404, 62002, 62012])
}

#[cfg(test)]
fn build_stored_page_info(page_model: &page::Model) -> Result<BiliPageInfo> {
    let cid = page_model.danmaku_cid_snapshot.unwrap_or(page_model.cid);
    if cid <= 0 {
        bail!(
            "分页 pid={} 缺少可用 cid，无法回退到数据库分页信息刷新弹幕",
            page_model.pid
        );
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

pub async fn initialize_danmaku_incremental_baseline(
    connection: &DatabaseConnection,
    config: &Config,
) -> Result<usize> {
    if !config.danmaku_update_policy.enabled {
        return Ok(0);
    }

    let candidates = load_candidate_videos(connection).await?;
    let now = Utc::now();
    let now_str = to_standard_string(now.with_timezone(&beijing_timezone()));
    let mut initialized = 0usize;

    for (video_model, pages) in candidates {
        let pubtime = stored_beijing_naive_to_utc(video_model.pubtime);
        let generation = stage_for_age(&config.danmaku_update_policy, pubtime, now, true).as_generation();

        for page_model in pages {
            if page_model.danmaku_last_synced_at.is_some() {
                continue;
            }

            let mut active: page::ActiveModel = page_model.clone().into();
            active.danmaku_last_synced_at = Set(Some(now_str.clone()));
            active.danmaku_sync_generation = Set(generation);
            if page_model.cid > 0 && page_model.danmaku_cid_snapshot != Some(page_model.cid) {
                active.danmaku_cid_snapshot = Set(Some(page_model.cid));
            }
            active
                .update(connection)
                .await
                .with_context(|| format!("初始化分页 {} 的弹幕增量基线失败", page_model.id))?;
            initialized += 1;
        }
    }

    if initialized > 0 {
        info!(
            "弹幕增量更新首次启用：已为 {} 个已完成分页写入基线同步时间，避免立即扫全库",
            initialized
        );
    } else {
        info!("弹幕增量更新首次启用：没有需要初始化基线的已完成分页");
    }

    Ok(initialized)
}

pub async fn schedule_incremental_danmaku_for_source(
    connection: &DatabaseConnection,
    source_filter: SimpleExpr,
    config: &Config,
) -> Result<usize> {
    if !config.danmaku_update_policy.enabled {
        return Ok(0);
    }

    let candidates = load_candidate_videos_with_filter(connection, Condition::all().add(source_filter)).await?;
    let now = Utc::now();
    let mut selected_pages = Vec::new();

    for (video_model, pages) in candidates {
        let pubtime = stored_beijing_naive_to_utc(video_model.pubtime);
        for page_model in pages {
            let last_synced = page_model
                .danmaku_last_synced_at
                .as_deref()
                .and_then(parse_stored_datetime);

            if matches!(
                should_sync_danmaku(
                    &config.danmaku_update_policy,
                    pubtime,
                    last_synced,
                    page_model.danmaku_sync_generation,
                    now,
                ),
                Decision::Sync { .. }
            ) {
                selected_pages.push(page_model);
            }
        }
    }

    let scheduled = mark_pages_danmaku_pending(connection, selected_pages, false).await?;
    if scheduled > 0 {
        info!("已将 {} 个到期分页的弹幕并回主下载状态流，等待本轮下载阶段处理", scheduled);
    }
    Ok(scheduled)
}

pub async fn schedule_video_danmaku_refresh(connection: &DatabaseConnection, video_id: i32) -> Result<usize> {
    video::Entity::find_by_id(video_id)
        .one(connection)
        .await?
        .ok_or_else(|| anyhow!("video {} 不存在", video_id))?;
    let pages = page::Entity::find()
        .filter(page::Column::VideoId.eq(video_id))
        .all(connection)
        .await?;
    mark_pages_danmaku_pending(connection, pages, true).await
}

pub async fn schedule_page_danmaku_refresh(connection: &DatabaseConnection, page_id: i32) -> Result<usize> {
    let page_model = page::Entity::find_by_id(page_id)
        .one(connection)
        .await?
        .ok_or_else(|| anyhow!("page {} 不存在", page_id))?;
    mark_pages_danmaku_pending(connection, vec![page_model], true).await
}

async fn mark_pages_danmaku_pending(
    connection: &DatabaseConnection,
    pages: Vec<page::Model>,
    force_auto_download: bool,
) -> Result<usize> {
    if pages.is_empty() {
        return Ok(0);
    }

    let mut page_updates = Vec::new();
    let mut affected_video_ids = HashSet::new();

    for page_model in pages {
        let mut page_status = PageStatus::from(page_model.download_status);
        let was_pending = page_status.get(DANMAKU_STATUS_OFFSET) == 0;
        if !was_pending {
            page_status.set(DANMAKU_STATUS_OFFSET, 0);
            let mut active: page::ActiveModel = page_model.clone().into();
            active.download_status = Set(page_status.into());
            page_updates.push(active);
        }
        affected_video_ids.insert(page_model.video_id);
    }

    if page_updates.is_empty() && !force_auto_download {
        return Ok(0);
    }

    let videos = video::Entity::find()
        .filter(video::Column::Id.is_in(affected_video_ids.iter().copied().collect::<Vec<_>>()))
        .all(connection)
        .await?;

    let mut video_updates = Vec::new();
    for video_model in videos {
        let mut video_status = VideoStatus::from(video_model.download_status);
        let mut changed = false;

        if video_status.get(4) != 0 {
            video_status.set(4, 0);
            changed = true;
        }

        if changed || (force_auto_download && !video_model.auto_download) {
            let mut active: video::ActiveModel = video_model.into();
            active.download_status = Set(video_status.into());
            if force_auto_download {
                active.auto_download = Set(true);
            }
            active.valid = Set(true);
            video_updates.push(active);
        }
    }

    if page_updates.is_empty() && video_updates.is_empty() {
        return Ok(0);
    }

    let scheduled_count = page_updates.len();
    let txn = crate::database::begin_traced_transaction(connection, "workflow_danmaku.mark_pages_danmaku_pending").await?;
    for update in video_updates {
        update.update(&txn).await?;
    }
    for update in page_updates {
        update.update(&txn).await?;
    }
    txn.commit().await?;

    Ok(scheduled_count)
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

    load_candidate_videos_with_filter(connection, Condition::all().add(source_filter)).await
}

async fn load_candidate_videos_with_filter(
    connection: &DatabaseConnection,
    filter: Condition,
) -> Result<Vec<(video::Model, Vec<page::Model>)>> {
    video::Entity::find()
        .filter(Condition::all().add(video::Column::Valid.eq(true)).add(filter))
        .find_with_related(page::Entity)
        .all(connection)
        .await
        .context("加载弹幕增量更新候选视频失败")
        .map(|rows| {
            rows.into_iter()
                .map(|(video_model, pages)| {
                    let filtered_pages = pages
                        .into_iter()
                        .filter(|page_model| danmaku_subtask_completed(page_model.download_status))
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

pub async fn sync_page_danmaku(
    bili_video: &Video<'_>,
    config: &Config,
    video_model: &video::Model,
    db_page: &page::Model,
    fresh: &BiliPageInfo,
    danmaku_path: &Path,
    next_stage: Option<Stage>,
    now: DateTime<Utc>,
    token: CancellationToken,
) -> Result<PageDanmakuSyncUpdate> {
    if fresh.cid <= 0 {
        bail!("分页 pid={} 返回了无效 cid", fresh.page);
    }

    let pubtime = stored_beijing_naive_to_utc(video_model.pubtime);
    let resolved_stage = next_stage.unwrap_or_else(|| {
        crate::utils::danmaku_schedule::stage_for_age(&config.danmaku_update_policy, pubtime, now, false)
    });
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
        info!(
            "检测到视频「{}」({}) 分页 pid={} 的 cid 发生变化 ({} -> {})，本次仅更新弹幕快照，不重置视频下载任务，也不改写本地分页元数据",
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
        .get_danmaku_elements(&page_info_for_danmaku, token)
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
            writer.append(danmaku_path.to_path_buf()).await?;
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
    info!(
        "视频「{}」({}) 分页 pid={} 弹幕已刷新 -> 阶段={}",
        video_model.name,
        video_model.bvid,
        fresh.page,
        resolved_stage.label()
    );

    Ok(PageDanmakuSyncUpdate {
        danmaku_last_synced_at: now_str,
        danmaku_sync_generation: resolved_stage.as_generation(),
        danmaku_cid_snapshot: fresh.cid,
        danmaku_last_write_count: last_write_count,
        duration: (!cid_changed && duration_changed).then_some(fresh_duration),
        width: (!cid_changed && dimension_changed).then_some(fresh_width),
        height: (!cid_changed && dimension_changed).then_some(fresh_height),
        name: (!cid_changed && name_changed).then_some(fresh_name),
    })
}

fn extract_dimension(dimension: Option<&Dimension>) -> (Option<u32>, Option<u32>) {
    match dimension {
        Some(dimension) if dimension.rotate == 0 => (Some(dimension.width), Some(dimension.height)),
        Some(dimension) => (Some(dimension.height), Some(dimension.width)),
        None => (None, None),
    }
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
    use bili_sync_migration::{Migrator, MigratorTrait};
    use sea_orm::sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
    use sea_orm::{ActiveModelTrait, DatabaseBackend, DatabaseConnection, Set, SqlxSqliteConnector, Statement};
    use std::fs;
    use std::path::PathBuf;

    use crate::config::DanmakuUpdatePolicy;
    use crate::utils::status::STATUS_COMPLETED;

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("bili-sync-danmaku-{}-{}", prefix, uuid::Uuid::new_v4()));
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
            DatabaseBackend::Sqlite,
            "ALTER TABLE page ADD COLUMN ai_renamed INTEGER DEFAULT 0",
        ))
        .await
        .ok();
        db
    }

    async fn insert_test_submission(db: &DatabaseConnection, id: i32) {
        submission::ActiveModel {
            id: Set(id),
            upper_id: Set(10_000 + i64::from(id)),
            upper_name: Set(format!("测试UP{id}")),
            path: Set(format!("/tmp/submission-{id}")),
            created_at: Set("2026-04-15 00:00:00".to_string()),
            latest_row_at: Set("2026-04-15 00:00:00".to_string()),
            enabled: Set(true),
            scan_deleted_videos: Set(false),
            scan_deleted_videos_once: Set(false),
            selected_videos: Set(None),
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
            download_danmaku: Set(true),
            download_subtitle: Set(false),
            ai_rename: Set(false),
            ai_rename_video_prompt: Set(String::new()),
            ai_rename_audio_prompt: Set(String::new()),
            ai_rename_enable_multi_page: Set(false),
            ai_rename_enable_collection: Set(false),
            ai_rename_enable_bangumi: Set(false),
            ai_rename_rename_parent_dir: Set(false),
            use_dynamic_api: Set(false),
            dynamic_api_full_synced: Set(false),
            last_scan_at: Set(None),
            next_scan_at: Set(None),
            no_update_streak: Set(0),
        }
        .insert(db)
        .await
        .expect("应能插入测试投稿源");
    }

    async fn insert_test_video(
        db: &DatabaseConnection,
        id: i32,
        submission_id: i32,
        bvid: &str,
        title: &str,
        pubtime: chrono::DateTime<Utc>,
    ) {
        let naive = pubtime.naive_utc();
        video::ActiveModel {
            id: Set(id),
            collection_id: Set(None),
            favorite_id: Set(None),
            watch_later_id: Set(None),
            submission_id: Set(Some(submission_id)),
            source_id: Set(None),
            source_type: Set(None),
            upper_id: Set(10_000 + i64::from(submission_id)),
            upper_name: Set(format!("测试UP{submission_id}")),
            upper_face: Set(String::new()),
            staff_info: Set(None),
            source_submission_id: Set(None),
            name: Set(title.to_string()),
            path: Set("/tmp/video".to_string()),
            category: Set(0),
            bvid: Set(bvid.to_string()),
            intro: Set(String::new()),
            cover: Set(String::new()),
            ctime: Set(naive),
            pubtime: Set(naive),
            favtime: Set(naive),
            download_status: Set(completed_page_status()),
            valid: Set(true),
            tags: Set(None),
            single_page: Set(Some(true)),
            created_at: Set("2026-04-15 00:00:00".to_string()),
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
        }
        .insert(db)
        .await
        .expect("应能插入测试视频");
    }

    async fn insert_test_page(
        db: &DatabaseConnection,
        id: i32,
        video_id: i32,
        pid: i32,
        cid: i64,
        path: &str,
        last_synced: Option<&str>,
        generation: u32,
    ) {
        page::ActiveModel {
            id: Set(id),
            video_id: Set(video_id),
            cid: Set(cid),
            pid: Set(pid),
            name: Set(format!("P{pid}")),
            width: Set(Some(1920)),
            height: Set(Some(1080)),
            duration: Set(120),
            path: Set(Some(path.to_string())),
            file_size_bytes: Set(Some(123)),
            video_stream_size_bytes: Set(Some(456)),
            audio_stream_size_bytes: Set(Some(789)),
            image: Set(None),
            download_status: Set(completed_page_status()),
            created_at: Set("2026-04-15 00:00:00".to_string()),
            play_video_streams: Set(Some("[\"cached-video\"]".to_string())),
            play_audio_streams: Set(Some("[\"cached-audio\"]".to_string())),
            play_subtitle_streams: Set(Some("[\"cached-subtitle\"]".to_string())),
            play_streams_updated_at: Set(Some("2026-04-15 00:00:00".to_string())),
            danmaku_last_synced_at: Set(last_synced.map(ToString::to_string)),
            danmaku_sync_generation: Set(generation),
            danmaku_cid_snapshot: Set(Some(cid)),
            danmaku_last_write_count: Set(0),
            ai_renamed: Set(Some(0)),
        }
        .insert(db)
        .await
        .expect("应能插入测试分页");
    }

    fn completed_page_status() -> u32 {
        STATUS_COMPLETED | (0..5).map(|index| STATUS_OK << (index * 3)).fold(0u32, |acc, item| acc | item)
    }

    fn enabled_policy() -> DanmakuUpdatePolicy {
        DanmakuUpdatePolicy {
            enabled: true,
            ..DanmakuUpdatePolicy::default()
        }
    }

    #[test]
    fn danmaku_completed_detects_ok() {
        let with_danmaku_ok: u32 = STATUS_OK << 9;
        assert!(danmaku_subtask_completed(with_danmaku_ok));
        let without: u32 = STATUS_OK << 6;
        assert!(!danmaku_subtask_completed(without));
    }

    #[tokio::test]
    async fn initialize_baseline_marks_existing_completed_pages_as_synced() {
        let db = create_test_db("baseline").await;
        insert_test_submission(&db, 3903).await;

        let media_dir = unique_temp_dir("baseline-media");
        fs::create_dir_all(&media_dir).expect("应能创建测试媒体目录");
        let video_path = media_dir.join("baseline.mp4");
        fs::write(&video_path, []).expect("应能创建测试视频文件");

        let pubtime = Utc.with_ymd_and_hms(2026, 4, 14, 0, 0, 0).unwrap();
        insert_test_video(&db, 1, 3903, "BVBASELINE0001", "基线测试", pubtime).await;
        insert_test_page(
            &db,
            1,
            1,
            1,
            334951837,
            &video_path.to_string_lossy(),
            None,
            Stage::Initial.as_generation(),
        )
        .await;

        let config = Config {
            danmaku_update_policy: enabled_policy(),
            ..Config::default()
        };
        let initialized = initialize_danmaku_incremental_baseline(&db, &config)
            .await
            .expect("初始化基线应成功");
        assert_eq!(initialized, 1);

        let page_model = page::Entity::find_by_id(1)
            .one(&db)
            .await
            .expect("查询分页应成功")
            .expect("分页应存在");
        assert!(page_model.danmaku_last_synced_at.is_some());
        assert_eq!(page_model.danmaku_sync_generation, Stage::Fresh.as_generation());
        assert_eq!(page_model.danmaku_cid_snapshot, Some(334951837));
    }

    #[tokio::test]
    async fn schedule_page_refresh_marks_existing_status_flow_as_pending() {
        let db = create_test_db("schedule-page-refresh").await;
        insert_test_submission(&db, 3904).await;

        let media_dir = unique_temp_dir("schedule-page-refresh-media");
        fs::create_dir_all(&media_dir).expect("应能创建测试媒体目录");
        let video_path = media_dir.join("refresh.mp4");
        fs::write(&video_path, []).expect("应能创建测试视频文件");

        let pubtime = Utc.with_ymd_and_hms(2026, 4, 14, 0, 0, 0).unwrap();
        insert_test_video(&db, 2, 3904, "BVSCHEDULE0001", "待刷新测试", pubtime).await;
        insert_test_page(
            &db,
            2,
            2,
            1,
            334951838,
            &video_path.to_string_lossy(),
            Some("2026-04-14 08:00:00"),
            Stage::Fresh.as_generation(),
        )
        .await;

        let scheduled = schedule_page_danmaku_refresh(&db, 2)
            .await
            .expect("分页弹幕刷新应能重新标记为待处理");
        assert_eq!(scheduled, 1);

        let page_model = page::Entity::find_by_id(2)
            .one(&db)
            .await
            .expect("查询分页应成功")
            .expect("分页应存在");
        let video_model = video::Entity::find_by_id(2)
            .one(&db)
            .await
            .expect("查询视频应成功")
            .expect("视频应存在");

        let page_status = PageStatus::from(page_model.download_status);
        let video_status = VideoStatus::from(video_model.download_status);
        assert_eq!(page_status.get(DANMAKU_STATUS_OFFSET), 0);
        assert_eq!(page_status.get(2), STATUS_OK);
        assert_eq!(video_status.get(4), 0);
        assert!(video_model.auto_download);
        assert!(video_model.valid);
    }

    #[test]
    fn build_stored_page_info_prefers_snapshot_cid() {
        let page_model = page::Model {
            cid: 100,
            pid: 1,
            name: "P1".to_string(),
            duration: 42,
            danmaku_cid_snapshot: Some(200),
            ..Default::default()
        };

        let page_info = build_stored_page_info(&page_model).expect("build page info");
        assert_eq!(page_info.cid, 200);
        assert_eq!(page_info.page, 1);
        assert_eq!(page_info.name, "P1");
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
