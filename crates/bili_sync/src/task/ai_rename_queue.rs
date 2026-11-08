//! AI 批量重命名任务队列
//!
//! `POST /api/{source_type}/{id}/ai-rename-history` 原本在 HTTP 请求里同步跑完整批 AI
//! 重命名，100 个文件可能要 1~30 分钟；客户端或反向代理在 30s 后断开连接时，axum 会丢弃
//! 正在等待的 handler future，任务直接丢失且没有任何进度。
//!
//! 这里把执行过程挪进任务队列：接口只做参数校验并写入 `task_queue` 表后立即返回，真正的
//! 扫描与 AI 调用由 `process_ai_rename_history_tasks` 在后台串行完成；客户端断开不再影响
//! 执行。队列结构（内存队列 + 数据库持久化 + 去重 + 取消 + 重启恢复）沿用
//! `RefreshDanmakuTaskQueue` 的范式。

use crate::utils::live_updates::{notify_queue_status_changed, notify_videos_changed};
use crate::utils::time_format::now_standard_string;
use anyhow::{anyhow, Result};
use bili_sync_entity::task_queue::{self, Entity as TaskQueueEntity, TaskStatus, TaskType};
use bili_sync_entity::{collection, favorite, page, submission, video, video_source, watch_later, youtube_source};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, Set,
};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info};

/// AI 批量重命名任务（序列化后存 `task_queue.task_data`）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AiRenameHistoryTask {
    /// 视频源类型：collection/favorite/submission/watch_later/bangumi/pugv/manga/youtube/douyin/tiktok
    pub source_type: String,
    /// 视频源 ID
    pub source_id: i32,
    /// 唯一任务 ID，用于追踪与取消
    pub task_id: String,
    /// 视频重命名提示词（为空时执行阶段回退到视频源配置）
    pub video_prompt: String,
    /// 音频重命名提示词（为空时执行阶段回退到视频源配置）
    pub audio_prompt: String,
    /// 是否对多 P 视频启用 AI 重命名（入队时已按请求/全局配置解析为确定值）
    pub enable_multi_page: bool,
    /// 是否对合集视频启用 AI 重命名
    pub enable_collection: bool,
    /// 是否对番剧启用 AI 重命名
    pub enable_bangumi: bool,
    /// 是否允许重命名上级目录
    pub rename_parent_dir: bool,
}

impl AiRenameHistoryTask {
    /// 任务日志/队列描述里使用的稳定标识
    pub fn source_key(&self) -> String {
        format!("{}_{}", self.source_type, self.source_id)
    }
}

/// AI 批量重命名执行结果
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct AiRenameHistoryOutcome {
    pub renamed_count: usize,
    pub skipped_count: usize,
    pub failed_count: usize,
}

impl AiRenameHistoryOutcome {
    fn describe(&self) -> String {
        format!(
            "重命名 {} 个，跳过 {} 个，失败 {} 个",
            self.renamed_count, self.skipped_count, self.failed_count
        )
    }
}

impl From<crate::utils::ai_rename::BatchRenameResult> for AiRenameHistoryOutcome {
    fn from(result: crate::utils::ai_rename::BatchRenameResult) -> Self {
        Self {
            renamed_count: result.renamed_count,
            skipped_count: result.skipped_count,
            failed_count: result.failed_count,
        }
    }
}

/// AI 批量重命名任务队列管理器
pub struct AiRenameHistoryTaskQueue {
    /// 待处理任务的内存队列
    queue: Mutex<VecDeque<AiRenameHistoryTask>>,
    /// 是否正在处理（防止 handler 重复 spawn 造成并发执行）
    is_processing: AtomicBool,
}

impl AiRenameHistoryTaskQueue {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            is_processing: AtomicBool::new(false),
        }
    }

    /// 同一视频源是否已经有待处理任务（按 source_type + source_id 去重）
    pub async fn has_pending_task(&self, task: &AiRenameHistoryTask, connection: &DatabaseConnection) -> Result<bool> {
        let pending_count = TaskQueueEntity::find()
            .filter(task_queue::Column::TaskType.eq(TaskType::AiRenameHistory))
            .filter(task_queue::Column::Status.eq(TaskStatus::Pending))
            .count(connection)
            .await?;

        if pending_count == 0 {
            return Ok(false);
        }

        let pending_tasks = TaskQueueEntity::find()
            .filter(task_queue::Column::TaskType.eq(TaskType::AiRenameHistory))
            .filter(task_queue::Column::Status.eq(TaskStatus::Pending))
            .all(connection)
            .await?;

        for task_record in pending_tasks {
            if let Ok(task_data) = serde_json::from_str::<AiRenameHistoryTask>(&task_record.task_data) {
                if task_data.source_type == task.source_type && task_data.source_id == task.source_id {
                    return Ok(true);
                }
            }
        }

        Ok(false)
    }

    /// 加入任务队列（同时持久化到数据库）；返回 `true` 表示本次真正入队，`false` 表示同源任务已存在
    pub async fn enqueue_task(&self, task: AiRenameHistoryTask, connection: &DatabaseConnection) -> Result<bool> {
        if self.has_pending_task(&task, connection).await? {
            debug!("AI 批量重命名任务已存在，跳过重复创建: {}", task.source_key());
            return Ok(false);
        }

        let task_data = serde_json::to_string(&task)?;
        let active_model = task_queue::ActiveModel {
            task_type: Set(TaskType::AiRenameHistory),
            task_data: Set(task_data),
            status: Set(TaskStatus::Pending),
            retry_count: Set(0),
            created_at: Set(now_standard_string()),
            updated_at: Set(now_standard_string()),
            ..Default::default()
        };

        let result = active_model.insert(connection).await?;

        let mut queue = self.queue.lock().await;
        info!(
            "AI 批量重命名任务已加入队列: {}, 队列长度: {} (数据库ID: {})",
            task.source_key(),
            queue.len() + 1,
            result.id
        );
        queue.push_back(task);
        drop(queue);
        notify_queue_status_changed();

        Ok(true)
    }

    /// 从内存队列取出下一个任务
    pub async fn dequeue_task(&self) -> Option<AiRenameHistoryTask> {
        let mut queue = self.queue.lock().await;
        let task = queue.pop_front();
        if task.is_some() {
            drop(queue);
            notify_queue_status_changed();
        }
        task
    }

    /// 标记任务完成（数据库状态改为 completed）
    pub async fn mark_task_completed(&self, task: &AiRenameHistoryTask, connection: &DatabaseConnection) -> Result<()> {
        let task_data = serde_json::to_string(task)?;
        if let Some(db_task) = TaskQueueEntity::find()
            .filter(task_queue::Column::TaskType.eq(TaskType::AiRenameHistory))
            .filter(task_queue::Column::TaskData.eq(&task_data))
            .filter(task_queue::Column::Status.eq(TaskStatus::Pending))
            .one(connection)
            .await?
        {
            let mut active_model: task_queue::ActiveModel = db_task.into();
            active_model.status = Set(TaskStatus::Completed);
            active_model.updated_at = Set(now_standard_string());
            active_model.update(connection).await?;
        }

        Ok(())
    }

    /// 标记任务失败（数据库状态改为 failed，重试次数 +1）
    pub async fn mark_task_failed(&self, task: &AiRenameHistoryTask, connection: &DatabaseConnection) -> Result<()> {
        let task_data = serde_json::to_string(task)?;
        if let Some(db_task) = TaskQueueEntity::find()
            .filter(task_queue::Column::TaskType.eq(TaskType::AiRenameHistory))
            .filter(task_queue::Column::TaskData.eq(&task_data))
            .filter(task_queue::Column::Status.eq(TaskStatus::Pending))
            .one(connection)
            .await?
        {
            let retry_count = db_task.retry_count;
            let mut active_model: task_queue::ActiveModel = db_task.into();
            active_model.status = Set(TaskStatus::Failed);
            active_model.retry_count = Set(retry_count + 1);
            active_model.updated_at = Set(now_standard_string());
            active_model.update(connection).await?;
        }

        Ok(())
    }

    /// 当前内存队列长度
    pub async fn queue_length(&self) -> usize {
        let queue = self.queue.lock().await;
        queue.len()
    }

    /// 列出内存队列中的任务（用于队列状态接口）
    pub async fn list_tasks(&self) -> Vec<AiRenameHistoryTask> {
        let queue = self.queue.lock().await;
        queue.iter().cloned().collect()
    }

    /// 取消指定任务（仅支持仍在内存队列中等待的任务）
    pub async fn cancel_task(&self, task_id: &str, connection: &DatabaseConnection) -> Result<bool> {
        let removed_task = {
            let mut queue = self.queue.lock().await;
            if let Some(index) = queue.iter().position(|task| task.task_id == task_id) {
                queue.remove(index)
            } else {
                None
            }
        };

        let Some(task) = removed_task else {
            return Ok(false);
        };

        let task_data = serde_json::to_string(&task)?;
        TaskQueueEntity::delete_many()
            .filter(task_queue::Column::TaskType.eq(TaskType::AiRenameHistory))
            .filter(task_queue::Column::TaskData.eq(task_data))
            .filter(task_queue::Column::Status.eq(TaskStatus::Pending))
            .exec(connection)
            .await?;

        notify_queue_status_changed();
        Ok(true)
    }

    /// 是否正在处理任务
    pub fn is_processing(&self) -> bool {
        self.is_processing.load(Ordering::SeqCst)
    }

    /// 更新处理状态并通知前端刷新队列状态
    pub fn set_processing(&self, is_processing: bool) {
        self.is_processing.store(is_processing, Ordering::SeqCst);
        notify_queue_status_changed();
    }

    /// 把从数据库恢复的任务直接推回内存队列（不重复写数据库）
    pub async fn recover_task(&self, task: AiRenameHistoryTask) {
        let mut queue = self.queue.lock().await;
        queue.push_back(task);
        drop(queue);
        notify_queue_status_changed();
    }

    /// 依次执行队列中的全部任务
    pub async fn process_all_tasks(&self, db: Arc<DatabaseConnection>) -> Result<u32, anyhow::Error> {
        if self.is_processing() {
            debug!("AI 批量重命名任务队列正在处理中，跳过重复处理");
            return Ok(0);
        }

        let queue_length = self.queue_length().await;
        if queue_length == 0 {
            return Ok(0);
        }

        self.set_processing(true);
        let mut processed_count = 0u32;

        info!("开始处理 AI 批量重命名任务，当前队列长度: {}", queue_length);

        while let Some(task) = self.dequeue_task().await {
            let source_key = task.source_key();

            match execute_ai_rename_history_task(db.as_ref(), &task).await {
                Ok(outcome) => {
                    info!("AI 批量重命名任务执行成功: {}, {}", source_key, outcome.describe());
                    processed_count += 1;

                    if let Err(e) = self.mark_task_completed(&task, &db).await {
                        error!("更新 AI 批量重命名任务完成状态失败: {:#}", e);
                    }
                }
                Err(e) => {
                    error!("AI 批量重命名任务执行失败: {}, 错误: {:#}", source_key, e);

                    if let Err(mark_err) = self.mark_task_failed(&task, &db).await {
                        error!("更新 AI 批量重命名任务失败状态失败: {:#}", mark_err);
                    }
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }

        self.set_processing(false);
        info!("AI 批量重命名任务队列处理完成，共处理 {} 个任务", processed_count);
        Ok(processed_count)
    }
}

impl Default for AiRenameHistoryTaskQueue {
    fn default() -> Self {
        Self::new()
    }
}

/// 扫描恢复守卫：无论任务成功还是失败，退出作用域时都会恢复定时扫描
struct ScanResumeGuard;

impl Drop for ScanResumeGuard {
    fn drop(&mut self) {
        info!("AI 批量重命名任务结束，恢复扫描任务...");
        crate::task::resume_scanning();
    }
}

/// 执行单个 AI 批量重命名任务
///
/// 该函数在任务队列的后台 worker 中运行，内部负责暂停/恢复定时扫描，并按视频源类型
/// 分流到 B 站（合集/收藏夹/投稿/稍后观看/番剧/课程/漫画）或外部平台（YouTube/抖音/
/// TikTok）的重命名实现。
pub async fn execute_ai_rename_history_task(
    db: &DatabaseConnection,
    task: &AiRenameHistoryTask,
) -> Result<AiRenameHistoryOutcome> {
    let config = crate::config::reload_config();
    let mut ai_config = config.ai_rename.clone();

    if !ai_config.enabled {
        return Err(anyhow!("AI 重命名功能未启用，请在系统设置中开启"));
    }

    ai_config.enable_multi_page = task.enable_multi_page;
    ai_config.enable_collection = task.enable_collection;
    ai_config.enable_bangumi = task.enable_bangumi;
    ai_config.rename_parent_dir = task.rename_parent_dir;

    let source_key = task.source_key();
    info!("[{}] AI 批量重命名任务开始执行，暂停扫描任务...", source_key);
    crate::task::pause_scanning().await;
    let _guard = ScanResumeGuard;

    // 外部平台（YouTube/抖音/TikTok）使用 platform 适配层的 AI 重命名实现
    if matches!(task.source_type.as_str(), "youtube" | "douyin" | "tiktok") {
        let source = youtube_source::Entity::find_by_id(task.source_id)
            .one(db)
            .await?
            .ok_or_else(|| anyhow!("未找到指定的外部平台视频源"))?;

        let result = crate::youtube::ai_rename_external_history(
            db,
            &source,
            &task.video_prompt,
            &task.audio_prompt,
            task.rename_parent_dir,
        )
        .await?;
        notify_videos_changed();

        let outcome = AiRenameHistoryOutcome::from(result);
        info!("[{}] AI 批量重命名完成：{}", source_key, outcome.describe());
        return Ok(outcome);
    }

    // B 站视频源：取源配置（提示词回退 + 目录结构）和已下载视频列表
    let (source_video_prompt, source_audio_prompt, flat_folder) =
        load_bilibili_source_config(db, &task.source_type, task.source_id).await?;

    let video_prompt = if task.video_prompt.is_empty() {
        source_video_prompt
    } else {
        task.video_prompt.clone()
    };
    let audio_prompt = if task.audio_prompt.is_empty() {
        source_audio_prompt
    } else {
        task.audio_prompt.clone()
    };

    let videos = get_videos_with_pages_for_source(db, &task.source_type, task.source_id).await?;
    if videos.is_empty() {
        info!("[{}] 该视频源没有已下载的视频，跳过 AI 批量重命名", source_key);
        return Ok(AiRenameHistoryOutcome::default());
    }

    info!("[{}] 开始批量 AI 重命名，共 {} 个视频", source_key, videos.len());
    if !video_prompt.is_empty() {
        info!("[{}] 视频提示词: {}", source_key, video_prompt);
    }
    if !audio_prompt.is_empty() {
        info!("[{}] 音频提示词: {}", source_key, audio_prompt);
    }

    let structure_hint = compute_structure_hint(&task.source_type, &config);
    let result =
        crate::utils::ai_rename::batch_rename_history_files(crate::utils::ai_rename::BatchRenameHistoryInput {
            connection: db,
            source_key: &source_key,
            videos,
            config: &ai_config,
            video_prompt: &video_prompt,
            audio_prompt: &audio_prompt,
            flat_folder,
            structure_hint,
        })
        .await?;
    notify_videos_changed();

    let outcome = AiRenameHistoryOutcome::from(result);
    info!("[{}] AI 批量重命名完成：{}", source_key, outcome.describe());
    Ok(outcome)
}

/// 读取 B 站视频源的 AI 重命名提示词与目录模式（供执行阶段使用）
async fn load_bilibili_source_config(
    db: &DatabaseConnection,
    source_type: &str,
    source_id: i32,
) -> Result<(String, String, bool)> {
    match source_type {
        "collection" => {
            let source = collection::Entity::find_by_id(source_id)
                .one(db)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的合集"))?;
            Ok((
                source.ai_rename_video_prompt,
                source.ai_rename_audio_prompt,
                source.flat_folder,
            ))
        }
        "favorite" => {
            let source = favorite::Entity::find_by_id(source_id)
                .one(db)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的收藏夹"))?;
            Ok((
                source.ai_rename_video_prompt,
                source.ai_rename_audio_prompt,
                source.flat_folder,
            ))
        }
        "submission" => {
            let source = submission::Entity::find_by_id(source_id)
                .one(db)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的UP主投稿"))?;
            Ok((
                source.ai_rename_video_prompt,
                source.ai_rename_audio_prompt,
                source.flat_folder,
            ))
        }
        "watch_later" => {
            let source = watch_later::Entity::find_by_id(source_id)
                .one(db)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的稍后观看"))?;
            Ok((
                source.ai_rename_video_prompt,
                source.ai_rename_audio_prompt,
                source.flat_folder,
            ))
        }
        "bangumi" | "pugv" | "manga" => {
            let source = video_source::Entity::find_by_id(source_id)
                .one(db)
                .await?
                .ok_or_else(|| anyhow!("未找到指定的番剧"))?;
            Ok((
                source.ai_rename_video_prompt,
                source.ai_rename_audio_prompt,
                source.flat_folder,
            ))
        }
        _ => Err(anyhow!("不支持的视频源类型: {}", source_type)),
    }
}

/// 根据源类型计算目录结构提示（帮助 AI 按单 P/多 P/番剧结构生成与文件一一对应的文件名）
fn compute_structure_hint(source_type: &str, config: &crate::config::Config) -> &'static str {
    match source_type {
        // 漫画按「系列目录 + 每话一个 CBZ」落盘，不参与 S/E 结构
        "manga" => "",
        "bangumi" | "pugv" => {
            if config.bangumi_use_season_structure {
                "番剧Season结构"
            } else {
                "番剧季文件夹"
            }
        }
        "collection" => match config.collection_folder_mode.as_ref() {
            Some("unified") => "合集统一模式（SxxExx）",
            Some("up_seasonal") => "UP分季结构（SxxExx）",
            _ => "",
        },
        _ => {
            if config.multi_page_use_season_structure {
                "Season 结构"
            } else {
                ""
            }
        }
    }
}

/// 获取视频源下所有已下载视频及其分页
async fn get_videos_with_pages_for_source(
    db: &DatabaseConnection,
    source_type: &str,
    source_id: i32,
) -> Result<Vec<(video::Model, Vec<page::Model>)>> {
    // 根据源类型查询视频（按发布时间正序排列，便于 AI 生成连续的集数编号）
    let videos = match source_type {
        "collection" => {
            video::Entity::find()
                .filter(video::Column::CollectionId.eq(source_id))
                .order_by_asc(video::Column::Pubtime)
                .all(db)
                .await?
        }
        "favorite" => {
            video::Entity::find()
                .filter(video::Column::FavoriteId.eq(source_id))
                .order_by_asc(video::Column::Pubtime)
                .all(db)
                .await?
        }
        "submission" => {
            video::Entity::find()
                .filter(video::Column::SubmissionId.eq(source_id))
                .order_by_asc(video::Column::Pubtime)
                .all(db)
                .await?
        }
        "watch_later" => {
            video::Entity::find()
                .filter(video::Column::WatchLaterId.eq(source_id))
                .order_by_asc(video::Column::Pubtime)
                .all(db)
                .await?
        }
        "bangumi" | "pugv" | "manga" => {
            video::Entity::find()
                .filter(video::Column::SourceId.eq(source_id))
                .order_by_asc(video::Column::Pubtime)
                .all(db)
                .await?
        }
        _ => return Err(anyhow!("不支持的视频源类型: {}", source_type)),
    };

    // 获取每个视频的已下载分页
    let mut result = Vec::new();
    for video_model in videos {
        // 查询已下载的分页（download_status > 0 表示至少有部分下载完成）
        let pages = page::Entity::find()
            .filter(page::Column::VideoId.eq(video_model.id))
            .filter(page::Column::DownloadStatus.gt(0))
            .filter(page::Column::Path.is_not_null())
            .all(db)
            .await?;

        if !pages.is_empty() {
            result.push((video_model, pages));
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bili_sync_migration::{Migrator, MigratorTrait};
    use sea_orm::Database;

    fn sample_task() -> AiRenameHistoryTask {
        AiRenameHistoryTask {
            source_type: "collection".to_string(),
            source_id: 7,
            task_id: "test-task".to_string(),
            video_prompt: String::new(),
            audio_prompt: String::new(),
            enable_multi_page: false,
            enable_collection: true,
            enable_bangumi: false,
            rename_parent_dir: false,
        }
    }

    #[tokio::test]
    async fn ai_rename_enqueue_deduplicates_pending_tasks() {
        let db = Database::connect("sqlite::memory:").await.expect("应能创建内存测试库");
        Migrator::up(&db, None).await.expect("应能完成测试数据库迁移");

        let queue = AiRenameHistoryTaskQueue::new();
        let task = sample_task();

        assert!(
            queue.enqueue_task(task.clone(), &db).await.expect("首次入队应成功"),
            "首次入队应返回 true"
        );
        assert!(
            !queue.enqueue_task(task.clone(), &db).await.expect("重复入队不应报错"),
            "同一视频源的重复任务不应再次入队"
        );

        assert_eq!(queue.queue_length().await, 1, "内存队列只应保留一个任务");

        let pending_count = TaskQueueEntity::find()
            .filter(task_queue::Column::TaskType.eq(TaskType::AiRenameHistory))
            .filter(task_queue::Column::Status.eq(TaskStatus::Pending))
            .count(&db)
            .await
            .expect("应能统计待处理任务");
        assert_eq!(pending_count, 1, "数据库中同一视频源只应保留一条待处理任务");
    }

    #[tokio::test]
    async fn recovered_ai_rename_task_can_be_deserialized_and_enqueued() {
        let queue = AiRenameHistoryTaskQueue::new();
        let task = sample_task();

        let task_data = serde_json::to_string(&task).expect("任务应能序列化");
        let restored: AiRenameHistoryTask = serde_json::from_str(&task_data).expect("任务应能反序列化");
        queue.recover_task(restored).await;

        let tasks = queue.list_tasks().await;
        assert_eq!(tasks.len(), 1, "恢复的任务应进入内存队列");
        assert_eq!(tasks[0].source_type, "collection");
        assert_eq!(tasks[0].source_id, 7);
        assert_eq!(tasks[0].task_id, "test-task");
        assert!(tasks[0].enable_collection, "恢复后选项字段应保持不变");
    }
}
