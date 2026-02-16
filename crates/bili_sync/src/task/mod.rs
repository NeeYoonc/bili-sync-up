mod http_server;
pub mod video_downloader;

pub use http_server::http_server;
pub use video_downloader::video_downloader;

use crate::utils::time_format::now_standard_string;
use anyhow::Result;
use bili_sync_entity::task_queue::{self, Entity as TaskQueueEntity, TaskStatus, TaskType};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder, Set,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info};

/// 删除视频源任务结构体
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteVideoSourceTask {
    pub source_type: String,
    pub source_id: i32,
    pub delete_local_files: bool,
    pub task_id: String, // 唯一任务ID，用于追踪
}

/// 删除视频任务结构体
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteVideoTask {
    pub video_id: i32,
    pub task_id: String, // 唯一任务ID，用于追踪
}

/// 添加视频源任务结构体
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AddVideoSourceTask {
    pub source_type: String,
    pub name: String,
    pub source_id: String,
    pub path: String,
    pub up_id: Option<String>,
    pub collection_type: Option<String>,
    pub media_id: Option<String>,
    pub ep_id: Option<String>,
    pub download_all_seasons: Option<bool>,
    pub selected_seasons: Option<Vec<String>>,
    pub task_id: String, // 唯一任务ID，用于追踪
}

/// 更新配置任务结构体
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateConfigTask {
    pub video_name: Option<String>,
    pub page_name: Option<String>,
    pub multi_page_name: Option<String>,
    pub bangumi_name: Option<String>,
    pub folder_structure: Option<String>,
    pub bangumi_folder_name: Option<String>,
    pub collection_folder_mode: Option<String>,
    pub collection_unified_name: Option<String>,
    pub time_format: Option<String>,
    pub interval: Option<u64>,
    pub nfo_time_type: Option<String>,
    pub parallel_download_enabled: Option<bool>,
    pub parallel_download_threads: Option<usize>,
    pub parallel_download_use_aria2: Option<bool>,
    // 视频质量设置
    pub video_max_quality: Option<String>,
    pub video_min_quality: Option<String>,
    pub audio_max_quality: Option<String>,
    pub audio_min_quality: Option<String>,
    pub codecs: Option<Vec<String>>,
    pub no_dolby_video: Option<bool>,
    pub no_dolby_audio: Option<bool>,
    pub no_hdr: Option<bool>,
    pub no_hires: Option<bool>,
    // 弹幕设置
    pub danmaku_duration: Option<f64>,
    pub danmaku_font: Option<String>,
    pub danmaku_font_size: Option<u32>,
    pub danmaku_width_ratio: Option<f64>,
    pub danmaku_horizontal_gap: Option<f64>,
    pub danmaku_lane_size: Option<u32>,
    pub danmaku_float_percentage: Option<f64>,
    pub danmaku_bottom_percentage: Option<f64>,
    pub danmaku_opacity: Option<u8>,
    pub danmaku_bold: Option<bool>,
    pub danmaku_outline: Option<f64>,
    pub danmaku_time_offset: Option<f64>,
    // 并发控制设置
    pub concurrent_video: Option<usize>,
    pub concurrent_page: Option<usize>,
    pub rate_limit: Option<usize>,
    pub rate_duration: Option<u64>,
    // 其他设置
    pub cdn_sorting: Option<bool>,
    // UP主投稿风控配置
    pub large_submission_threshold: Option<usize>,
    pub base_request_delay: Option<u64>,
    pub large_submission_delay_multiplier: Option<u64>,
    pub enable_progressive_delay: Option<bool>,
    pub max_delay_multiplier: Option<u64>,
    pub enable_incremental_fetch: Option<bool>,
    pub incremental_fallback_to_full: Option<bool>,
    pub enable_batch_processing: Option<bool>,
    pub batch_size: Option<usize>,
    pub batch_delay_seconds: Option<u64>,
    pub enable_auto_backoff: Option<bool>,
    pub auto_backoff_base_seconds: Option<u64>,
    pub auto_backoff_max_multiplier: Option<u64>,
    pub source_delay_seconds: Option<u64>,
    pub submission_source_delay_seconds: Option<u64>,
    // UP主投稿源扫描策略
    pub submission_scan_batch_size: Option<usize>,
    pub submission_adaptive_scan: Option<bool>,
    pub submission_adaptive_max_hours: Option<u64>,
    // 多P视频目录结构配置
    pub multi_page_use_season_structure: Option<bool>,
    // 合集目录结构配置
    pub collection_use_season_structure: Option<bool>,
    // 番剧目录结构配置
    pub bangumi_use_season_structure: Option<bool>,
    // UP主头像保存路径
    pub upper_path: Option<String>,
    pub ai_rename_rename_parent_dir: Option<bool>,
    pub task_id: String, // 唯一任务ID，用于追踪
}

/// 重载配置任务结构体
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReloadConfigTask {
    pub task_id: String, // 唯一任务ID，用于追踪
}

/// 删除任务队列管理器
pub struct DeleteTaskQueue {
    /// 待处理的删除任务队列（内存缓存）
    queue: Mutex<VecDeque<DeleteVideoSourceTask>>,
    /// 是否正在处理删除任务
    is_processing: AtomicBool,
}

impl DeleteTaskQueue {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            is_processing: AtomicBool::new(false),
        }
    }

    /// 添加删除任务到队列（同时保存到数据库）
    pub async fn enqueue_task(&self, task: DeleteVideoSourceTask, connection: &DatabaseConnection) -> Result<()> {
        // 保存到数据库
        let task_data = serde_json::to_string(&task)?;
        let active_model = task_queue::ActiveModel {
            task_type: Set(TaskType::DeleteVideoSource),
            task_data: Set(task_data),
            status: Set(TaskStatus::Pending),
            retry_count: Set(0),
            created_at: Set(now_standard_string()),
            updated_at: Set(now_standard_string()),
            ..Default::default()
        };

        let result = active_model.insert(connection).await?;

        // 添加到内存队列
        let mut queue = self.queue.lock().await;
        info!(
            "删除任务已加入队列: {} ID={}, 队列长度: {} (数据库ID: {})",
            task.source_type,
            task.source_id,
            queue.len() + 1,
            result.id
        );
        queue.push_back(task);

        Ok(())
    }

    /// 从队列中取出下一个任务
    pub async fn dequeue_task(&self) -> Option<DeleteVideoSourceTask> {
        let mut queue = self.queue.lock().await;
        queue.pop_front()
    }

    /// 标记任务为已完成（更新数据库状态）
    pub async fn mark_task_completed(
        &self,
        task: &DeleteVideoSourceTask,
        connection: &DatabaseConnection,
    ) -> Result<()> {
        let task_data = serde_json::to_string(task)?;

        // 查找并更新数据库中的任务状态
        if let Some(db_task) = TaskQueueEntity::find()
            .filter(task_queue::Column::TaskType.eq(TaskType::DeleteVideoSource))
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

    /// 标记任务为失败（更新数据库状态）
    pub async fn mark_task_failed(&self, task: &DeleteVideoSourceTask, connection: &DatabaseConnection) -> Result<()> {
        // 获取优化的数据库连接（写穿透模式下为主数据库）

        let task_data = serde_json::to_string(task)?;

        // 查找并更新数据库中的任务状态
        if let Some(db_task) = TaskQueueEntity::find()
            .filter(task_queue::Column::TaskType.eq(TaskType::DeleteVideoSource))
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

    /// 获取队列长度
    pub async fn queue_length(&self) -> usize {
        let queue = self.queue.lock().await;
        queue.len()
    }

    /// 获取当前内存队列任务快照
    pub async fn list_tasks(&self) -> Vec<DeleteVideoSourceTask> {
        let queue = self.queue.lock().await;
        queue.iter().cloned().collect()
    }

    /// 取消待处理任务（仅支持还在内存等待队列中的任务）
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
            .filter(task_queue::Column::TaskType.eq(TaskType::DeleteVideoSource))
            .filter(task_queue::Column::TaskData.eq(task_data))
            .filter(task_queue::Column::Status.eq(TaskStatus::Pending))
            .exec(connection)
            .await?;

        Ok(true)
    }

    /// 检查是否正在处理删除任务
    pub fn is_processing(&self) -> bool {
        self.is_processing.load(Ordering::SeqCst)
    }

    /// 设置处理状态
    pub fn set_processing(&self, is_processing: bool) {
        self.is_processing.store(is_processing, Ordering::SeqCst);
    }

    /// 处理队列中的所有删除任务
    pub async fn process_all_tasks(&self, db: Arc<DatabaseConnection>) -> Result<u32, anyhow::Error> {
        use crate::api::handler::delete_video_source_internal;

        if self.is_processing() {
            debug!("删除任务队列正在处理中，跳过重复处理");
            return Ok(0);
        }

        let queue_length = self.queue_length().await;
        if queue_length == 0 {
            return Ok(0);
        }

        self.set_processing(true);
        let mut processed_count = 0u32;

        info!("开始处理暂存的删除任务，当前队列长度: {}", queue_length);

        while let Some(task) = self.dequeue_task().await {
            info!(
                "正在处理删除任务: {} ID={} (是否删除本地文件: {})",
                task.source_type, task.source_id, task.delete_local_files
            );

            match delete_video_source_internal(
                db.clone(),
                task.source_type.clone(),
                task.source_id,
                task.delete_local_files,
            )
            .await
            {
                Ok(response) => {
                    info!("删除任务执行成功: {}", response.message);
                    processed_count += 1;

                    // 标记数据库任务为已完成
                    if let Err(e) = self.mark_task_completed(&task, &db).await {
                        error!("更新任务完成状态失败: {:#}", e);
                    }
                }
                Err(e) => {
                    error!(
                        "删除任务执行失败: {} ID={}, 错误: {:#?}",
                        task.source_type, task.source_id, e
                    );

                    // 发送删除任务失败通知（异步执行，不阻塞主流程）
                    let source_type = task.source_type.clone();
                    let source_id = task.source_id;
                    let error_msg = format!("{:#?}", e);
                    tokio::spawn(async move {
                        use crate::utils::notification::send_error_notification;
                        if let Err(notify_err) = send_error_notification(
                            "删除任务失败",
                            &error_msg,
                            Some(&format!("类型: {}\nID: {}", source_type, source_id)),
                        )
                        .await
                        {
                            tracing::warn!("发送删除任务失败通知失败: {}", notify_err);
                        }
                    });

                    // 标记数据库任务为失败
                    if let Err(e) = self.mark_task_failed(&task, &db).await {
                        error!("更新任务失败状态失败: {:#}", e);
                    }
                }
            }

            // 每个任务之间稍作间隔，避免过于频繁的数据库操作
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }

        self.set_processing(false);

        info!("删除任务队列处理完成，共处理 {} 个任务", processed_count);

        Ok(processed_count)
    }
}

/// 单个视频删除任务队列管理器
pub struct VideoDeleteTaskQueue {
    /// 待处理的视频删除任务队列
    queue: Mutex<VecDeque<DeleteVideoTask>>,
    /// 是否正在处理视频删除任务
    is_processing: AtomicBool,
}

impl VideoDeleteTaskQueue {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            is_processing: AtomicBool::new(false),
        }
    }

    /// 检查视频是否已有待处理的删除任务
    pub async fn has_pending_delete_task(&self, video_id: i32, connection: &DatabaseConnection) -> Result<bool> {
        let count = TaskQueueEntity::find()
            .filter(task_queue::Column::TaskType.eq(TaskType::DeleteVideo))
            .filter(task_queue::Column::Status.eq(TaskStatus::Pending))
            .count(connection)
            .await?;

        if count == 0 {
            return Ok(false);
        }

        // 检查待处理任务中是否包含该视频ID
        let pending_tasks = TaskQueueEntity::find()
            .filter(task_queue::Column::TaskType.eq(TaskType::DeleteVideo))
            .filter(task_queue::Column::Status.eq(TaskStatus::Pending))
            .all(connection)
            .await?;

        for task_record in pending_tasks {
            if let Ok(task_data) = serde_json::from_str::<DeleteVideoTask>(&task_record.task_data) {
                if task_data.video_id == video_id {
                    return Ok(true);
                }
            }
        }

        Ok(false)
    }

    /// 添加视频删除任务到队列（同时保存到数据库，带重复检查）
    pub async fn enqueue_task(&self, task: DeleteVideoTask, connection: &DatabaseConnection) -> Result<()> {
        // 检查是否已有该视频的待处理删除任务
        if self.has_pending_delete_task(task.video_id, connection).await? {
            debug!("视频ID={} 已有待处理的删除任务，跳过重复创建", task.video_id);
            return Ok(());
        }

        // 保存到数据库
        let task_data = serde_json::to_string(&task)?;
        let active_model = task_queue::ActiveModel {
            task_type: Set(TaskType::DeleteVideo),
            task_data: Set(task_data),
            status: Set(TaskStatus::Pending),
            retry_count: Set(0),
            created_at: Set(now_standard_string()),
            updated_at: Set(now_standard_string()),
            ..Default::default()
        };

        let result = active_model.insert(connection).await?;

        // 添加到内存队列
        let mut queue = self.queue.lock().await;
        info!(
            "视频删除任务已加入队列: 视频ID={}, 队列长度: {} (数据库ID: {})",
            task.video_id,
            queue.len() + 1,
            result.id
        );
        queue.push_back(task);

        Ok(())
    }

    /// 从队列中取出下一个任务
    pub async fn dequeue_task(&self) -> Option<DeleteVideoTask> {
        let mut queue = self.queue.lock().await;
        queue.pop_front()
    }

    /// 标记任务为已完成（更新数据库状态）
    pub async fn mark_task_completed(&self, task: &DeleteVideoTask, connection: &DatabaseConnection) -> Result<()> {
        let task_data = serde_json::to_string(task)?;

        // 查找并更新数据库中的任务状态
        if let Some(db_task) = TaskQueueEntity::find()
            .filter(task_queue::Column::TaskType.eq(TaskType::DeleteVideo))
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

    /// 标记任务为失败（更新数据库状态）
    pub async fn mark_task_failed(&self, task: &DeleteVideoTask, connection: &DatabaseConnection) -> Result<()> {
        let task_data = serde_json::to_string(task)?;

        // 查找并更新数据库中的任务状态
        if let Some(db_task) = TaskQueueEntity::find()
            .filter(task_queue::Column::TaskType.eq(TaskType::DeleteVideo))
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

    /// 获取队列长度
    pub async fn queue_length(&self) -> usize {
        let queue = self.queue.lock().await;
        queue.len()
    }

    /// 获取当前内存队列任务快照
    pub async fn list_tasks(&self) -> Vec<DeleteVideoTask> {
        let queue = self.queue.lock().await;
        queue.iter().cloned().collect()
    }

    /// 取消待处理任务（仅支持还在内存等待队列中的任务）
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
            .filter(task_queue::Column::TaskType.eq(TaskType::DeleteVideo))
            .filter(task_queue::Column::TaskData.eq(task_data))
            .filter(task_queue::Column::Status.eq(TaskStatus::Pending))
            .exec(connection)
            .await?;

        Ok(true)
    }

    /// 获取所有待删除的视频ID
    pub async fn get_pending_video_ids(&self) -> HashSet<i32> {
        let queue = self.queue.lock().await;
        queue.iter().map(|task| task.video_id).collect()
    }

    /// 检查是否正在处理视频删除任务
    pub fn is_processing(&self) -> bool {
        self.is_processing.load(Ordering::SeqCst)
    }

    /// 设置处理状态
    pub fn set_processing(&self, is_processing: bool) {
        self.is_processing.store(is_processing, Ordering::SeqCst);
    }

    /// 处理队列中的所有视频删除任务
    pub async fn process_all_tasks(&self, db: Arc<DatabaseConnection>) -> Result<u32, anyhow::Error> {
        if self.is_processing() {
            debug!("视频删除任务队列正在处理中，跳过重复处理");
            return Ok(0);
        }

        let queue_length = self.queue_length().await;
        if queue_length == 0 {
            return Ok(0);
        }

        self.set_processing(true);
        let mut processed_count = 0u32;

        info!("开始处理暂存的视频删除任务，当前队列长度: {}", queue_length);

        while let Some(task) = self.dequeue_task().await {
            info!("正在处理视频删除任务: 视频ID={}", task.video_id);

            // 执行软删除操作
            match delete_video_internal(db.clone(), task.video_id).await {
                Ok(_) => {
                    info!("视频删除任务执行成功: 视频ID={}", task.video_id);
                    processed_count += 1;

                    // 标记数据库任务为已完成
                    if let Err(e) = self.mark_task_completed(&task, &db).await {
                        error!("更新任务完成状态失败: {:#}", e);
                    }
                }
                Err(e) => {
                    let error_msg = e.to_string();

                    // 对于“视频已经被删除”的错误，使用INFO级别记录
                    if error_msg.contains("视频已经被删除") {
                        info!("视频删除任务跳过: 视频ID={} 已经被删除", task.video_id);
                        processed_count += 1; // 对于已删除的视频也认为是成功处理

                        // 标记为已完成，因为目标已达成（视频已经不存在）
                        if let Err(e) = self.mark_task_completed(&task, &db).await {
                            error!("更新任务完成状态失败: {:#}", e);
                        }
                    } else {
                        error!("视频删除任务执行失败: 视频ID={}, 错误: {:#?}", task.video_id, e);

                        // 发送视频删除失败通知（异步执行，不阻塞主流程）
                        let video_id = task.video_id;
                        let error_msg = format!("{:#?}", e);
                        tokio::spawn(async move {
                            use crate::utils::notification::send_error_notification;
                            if let Err(notify_err) = send_error_notification(
                                "视频删除失败",
                                &error_msg,
                                Some(&format!("视频ID: {}", video_id)),
                            )
                            .await
                            {
                                tracing::warn!("发送视频删除失败通知失败: {}", notify_err);
                            }
                        });

                        // 标记数据库任务为失败
                        if let Err(e) = self.mark_task_failed(&task, &db).await {
                            error!("更新任务失败状态失败: {:#}", e);
                        }
                    }
                }
            }

            // 每个任务之间稍作间隔，避免过于频繁的数据库操作
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }

        self.set_processing(false);

        info!("视频删除任务队列处理完成，共处理 {} 个任务", processed_count);

        Ok(processed_count)
    }
}

/// 视频软删除内部实现
async fn delete_video_internal(db: Arc<DatabaseConnection>, video_id: i32) -> Result<(), anyhow::Error> {
    use bili_sync_entity::{page, video};
    use sea_orm::*;

    // 检查视频是否存在
    let video = video::Entity::find_by_id(video_id)
        .one(db.as_ref())
        .await
        .map_err(|e| anyhow::anyhow!("查询视频失败: {}", e))?;

    let video = match video {
        Some(v) => v,
        None => {
            return Err(anyhow::anyhow!("视频不存在: ID={}", video_id));
        }
    };

    // 检查是否已经删除
    if video.deleted == 1 {
        return Err(anyhow::anyhow!("视频已经被删除: ID={}", video_id));
    }

    // 删除本地文件 - 根据page表中的路径精确删除
    let deleted_files = delete_video_files_from_pages_task(db.clone(), video_id).await?;

    if deleted_files > 0 {
        info!("已删除 {} 个视频文件", deleted_files);

        // 检查视频文件夹是否为空，如果为空则删除文件夹
        let normalized_video_path = normalize_file_path_task(&video.path);
        let video_path = std::path::Path::new(&normalized_video_path);
        if video_path.exists() {
            match tokio::fs::read_dir(&normalized_video_path).await {
                Ok(mut entries) => {
                    if entries.next_entry().await.unwrap_or(None).is_none() {
                        // 文件夹为空，删除它
                        if let Err(e) = std::fs::remove_dir(&normalized_video_path) {
                            warn!("删除空文件夹失败: {} - {}", normalized_video_path, e);
                        } else {
                            info!("已删除空文件夹: {}", normalized_video_path);
                        }
                    }
                }
                Err(e) => {
                    warn!("读取文件夹失败: {} - {}", normalized_video_path, e);
                }
            }
        }
    } else {
        debug!("未找到需要删除的文件，视频ID: {}", video_id);
    }

    // 在软删除video之前，先删除page表记录
    page::Entity::delete_many()
        .filter(page::Column::VideoId.eq(video_id))
        .exec(db.as_ref())
        .await
        .map_err(|e| anyhow::anyhow!("删除page记录失败: {}", e))?;

    info!("已删除video_id={}的所有page记录", video_id);

    // 执行软删除：将deleted字段设为1
    video::Entity::update_many()
        .col_expr(video::Column::Deleted, sea_orm::prelude::Expr::value(1))
        .filter(video::Column::Id.eq(video_id))
        .exec(db.as_ref())
        .await
        .map_err(|e| anyhow::anyhow!("更新视频删除状态失败: {}", e))?;

    info!("视频已成功删除: ID={}, 名称={}", video_id, video.name);

    Ok(())
}

/// 标准化文件路径格式
fn normalize_file_path_task(path: &str) -> String {
    // 将所有反斜杠转换为正斜杠，保持路径一致性
    path.replace('\\', "/")
}

/// 根据page表精确删除视频文件（任务队列版本）
async fn delete_video_files_from_pages_task(
    db: Arc<DatabaseConnection>,
    video_id: i32,
) -> Result<usize, anyhow::Error> {
    use bili_sync_entity::{page, video};
    use sea_orm::*;
    use tokio::fs;

    // 获取该视频的所有页面（分P）
    let pages = page::Entity::find()
        .filter(page::Column::VideoId.eq(video_id))
        .all(db.as_ref())
        .await
        .map_err(|e| anyhow::anyhow!("查询页面信息失败: {}", e))?;

    let mut deleted_count = 0;

    for page in pages {
        // 删除视频文件
        if let Some(file_path) = &page.path {
            let path = std::path::Path::new(file_path);
            info!("尝试删除视频文件: {}", file_path);
            if path.exists() {
                match fs::remove_file(path).await {
                    Ok(_) => {
                        debug!("已删除视频文件: {}", file_path);
                        deleted_count += 1;
                    }
                    Err(e) => {
                        warn!("删除视频文件失败: {} - {}", file_path, e);
                    }
                }
            } else {
                debug!("文件不存在，跳过删除: {}", file_path);
            }
        }

        // 同时删除封面图片（如果存在且是本地文件）
        if let Some(image_path) = &page.image {
            // 跳过HTTP URL，只处理本地文件路径
            if !image_path.starts_with("http://") && !image_path.starts_with("https://") {
                let path = std::path::Path::new(image_path);
                info!("尝试删除封面图片: {}", image_path);
                if path.exists() {
                    match fs::remove_file(path).await {
                        Ok(_) => {
                            info!("已删除封面图片: {}", image_path);
                            deleted_count += 1;
                        }
                        Err(e) => {
                            warn!("删除封面图片失败: {} - {}", image_path, e);
                        }
                    }
                } else {
                    debug!("封面图片文件不存在，跳过删除: {}", image_path);
                }
            } else {
                debug!("跳过远程封面图片URL: {}", image_path);
            }
        }
    }

    // 还要删除视频的NFO文件和其他可能的相关文件
    let video = video::Entity::find_by_id(video_id)
        .one(db.as_ref())
        .await
        .map_err(|e| anyhow::anyhow!("查询视频信息失败: {}", e))?;

    if let Some(video) = video {
        // 重新获取页面信息来删除基于视频文件名的相关文件
        let pages_for_cleanup = page::Entity::find()
            .filter(page::Column::VideoId.eq(video_id))
            .all(db.as_ref())
            .await
            .map_err(|e| anyhow::anyhow!("查询页面信息失败: {}", e))?;

        for page in &pages_for_cleanup {
            if let Some(file_path) = &page.path {
                let video_file = std::path::Path::new(file_path);
                if let Some(parent_dir) = video_file.parent() {
                    if let Some(file_stem) = video_file.file_stem() {
                        let file_stem_str = file_stem.to_string_lossy();

                        // 删除同名的NFO文件
                        let nfo_path = parent_dir.join(format!("{}.nfo", file_stem_str));
                        if nfo_path.exists() {
                            match fs::remove_file(&nfo_path).await {
                                Ok(_) => {
                                    debug!("已删除NFO文件: {:?}", nfo_path);
                                    deleted_count += 1;
                                }
                                Err(e) => {
                                    warn!("删除NFO文件失败: {:?} - {}", nfo_path, e);
                                }
                            }
                        }

                        // 删除封面文件 (-fanart.jpg, -thumb.jpg等)
                        for suffix in &["fanart", "thumb"] {
                            for ext in &["jpg", "jpeg", "png", "webp"] {
                                let cover_path = parent_dir.join(format!("{}-{}.{}", file_stem_str, suffix, ext));
                                if cover_path.exists() {
                                    match fs::remove_file(&cover_path).await {
                                        Ok(_) => {
                                            debug!("已删除封面文件: {:?}", cover_path);
                                            deleted_count += 1;
                                        }
                                        Err(e) => {
                                            warn!("删除封面文件失败: {:?} - {}", cover_path, e);
                                        }
                                    }
                                }
                            }
                        }

                        // 删除弹幕文件 (.zh-CN.default.ass等)
                        let danmaku_patterns = [
                            format!("{}.zh-CN.default.ass", file_stem_str),
                            format!("{}.ass", file_stem_str),
                            format!("{}.srt", file_stem_str),
                            format!("{}.xml", file_stem_str),
                        ];

                        for pattern in &danmaku_patterns {
                            let danmaku_path = parent_dir.join(pattern);
                            if danmaku_path.exists() {
                                match fs::remove_file(&danmaku_path).await {
                                    Ok(_) => {
                                        debug!("已删除弹幕文件: {:?}", danmaku_path);
                                        deleted_count += 1;
                                    }
                                    Err(e) => {
                                        warn!("删除弹幕文件失败: {:?} - {}", danmaku_path, e);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Season结构检测和根目录元数据文件删除
        if !pages_for_cleanup.is_empty() {
            // 检测是否使用Season结构：比较video.path和page.path
            if let Some(first_page) = pages_for_cleanup.first() {
                if let Some(page_path) = &first_page.path {
                    let video_path = std::path::Path::new(&video.path);
                    let page_path = std::path::Path::new(page_path);

                    // 如果page路径包含Season文件夹，说明使用了Season结构
                    let uses_season_structure = page_path.components().any(|component| {
                        if let std::path::Component::Normal(name) = component {
                            name.to_string_lossy().starts_with("Season ")
                        } else {
                            false
                        }
                    });

                    if uses_season_structure {
                        debug!("检测到Season结构，删除根目录元数据文件");

                        // 获取配置以确定video_base_name生成规则
                        let config = crate::config::reload_config();

                        // 确定是否为合集或多P视频
                        let is_collection = video.collection_id.is_some();
                        let is_single_page = video.single_page.unwrap_or(true);

                        // 检查是否需要处理
                        let should_process = (is_collection && config.collection_use_season_structure)
                            || (!is_single_page && config.multi_page_use_season_structure);

                        if should_process {
                            let video_base_name = if is_collection && config.collection_use_season_structure {
                                // 合集：使用合集名称
                                match bili_sync_entity::collection::Entity::find_by_id(video.collection_id.unwrap_or(0))
                                    .one(db.as_ref())
                                    .await
                                {
                                    Ok(Some(coll)) => coll.name,
                                    _ => "collection".to_string(),
                                }
                            } else {
                                // 多P视频：使用视频名称模板
                                use crate::utils::format_arg::video_format_args;
                                match crate::config::with_config(|bundle| {
                                    bundle.render_video_template(&video_format_args(&video))
                                }) {
                                    Ok(name) => name,
                                    Err(_) => video.name.clone(),
                                }
                            };

                            // 删除根目录的元数据文件
                            let metadata_files = [
                                "tvshow.nfo".to_string(),
                                format!("{}-thumb.jpg", video_base_name),
                                format!("{}-fanart.jpg", video_base_name),
                            ];

                            for metadata_file in &metadata_files {
                                let metadata_path = video_path.join(metadata_file);
                                if metadata_path.exists() {
                                    match fs::remove_file(&metadata_path).await {
                                        Ok(_) => {
                                            info!("已删除Season结构根目录元数据文件: {:?}", metadata_path);
                                            deleted_count += 1;
                                        }
                                        Err(e) => {
                                            warn!("删除Season结构根目录元数据文件失败: {:?} - {}", metadata_path, e);
                                        }
                                    }
                                } else {
                                    debug!("Season结构根目录元数据文件不存在: {:?}", metadata_path);
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(deleted_count)
}

/// 添加任务队列管理器
pub struct AddTaskQueue {
    /// 待处理的添加任务队列
    queue: Mutex<VecDeque<AddVideoSourceTask>>,
    /// 是否正在处理添加任务
    is_processing: AtomicBool,
}

impl AddTaskQueue {
    pub fn new() -> Self {
        Self {
            queue: Mutex::new(VecDeque::new()),
            is_processing: AtomicBool::new(false),
        }
    }

    /// 添加添加任务到队列（同时保存到数据库）
    pub async fn enqueue_task(&self, task: AddVideoSourceTask, connection: &DatabaseConnection) -> Result<()> {
        // 保存到数据库
        let task_data = serde_json::to_string(&task)?;
        let active_model = task_queue::ActiveModel {
            task_type: Set(TaskType::AddVideoSource),
            task_data: Set(task_data),
            status: Set(TaskStatus::Pending),
            retry_count: Set(0),
            created_at: Set(now_standard_string()),
            updated_at: Set(now_standard_string()),
            ..Default::default()
        };

        let result = active_model.insert(connection).await?;

        // 添加到内存队列
        let mut queue = self.queue.lock().await;
        info!(
            "添加任务已加入队列: {} 名称={}, 队列长度: {} (数据库ID: {})",
            task.source_type,
            task.name,
            queue.len() + 1,
            result.id
        );
        queue.push_back(task);

        Ok(())
    }

    /// 从队列中取出下一个任务
    pub async fn dequeue_task(&self) -> Option<AddVideoSourceTask> {
        let mut queue = self.queue.lock().await;
        queue.pop_front()
    }

    /// 标记任务为已完成（更新数据库状态）
    pub async fn mark_task_completed(&self, task: &AddVideoSourceTask, connection: &DatabaseConnection) -> Result<()> {
        let task_data = serde_json::to_string(task)?;

        // 查找并更新数据库中的任务状态
        if let Some(db_task) = TaskQueueEntity::find()
            .filter(task_queue::Column::TaskType.eq(TaskType::AddVideoSource))
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

    /// 标记任务为失败（更新数据库状态）
    pub async fn mark_task_failed(&self, task: &AddVideoSourceTask, connection: &DatabaseConnection) -> Result<()> {
        let task_data = serde_json::to_string(task)?;

        // 查找并更新数据库中的任务状态
        if let Some(db_task) = TaskQueueEntity::find()
            .filter(task_queue::Column::TaskType.eq(TaskType::AddVideoSource))
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

    /// 获取队列长度
    pub async fn queue_length(&self) -> usize {
        let queue = self.queue.lock().await;
        queue.len()
    }

    /// 获取当前内存队列任务快照
    pub async fn list_tasks(&self) -> Vec<AddVideoSourceTask> {
        let queue = self.queue.lock().await;
        queue.iter().cloned().collect()
    }

    /// 取消待处理任务（仅支持还在内存等待队列中的任务）
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
            .filter(task_queue::Column::TaskType.eq(TaskType::AddVideoSource))
            .filter(task_queue::Column::TaskData.eq(task_data))
            .filter(task_queue::Column::Status.eq(TaskStatus::Pending))
            .exec(connection)
            .await?;

        Ok(true)
    }

    /// 检查是否正在处理添加任务
    pub fn is_processing(&self) -> bool {
        self.is_processing.load(Ordering::SeqCst)
    }

    /// 设置处理状态
    pub fn set_processing(&self, is_processing: bool) {
        self.is_processing.store(is_processing, Ordering::SeqCst);
    }

    /// 处理队列中的所有添加任务
    pub async fn process_all_tasks(&self, db: Arc<DatabaseConnection>) -> Result<u32, anyhow::Error> {
        use crate::api::handler::add_video_source_internal;

        if self.is_processing() {
            debug!("添加任务队列正在处理中，跳过重复处理");
            return Ok(0);
        }

        let queue_length = self.queue_length().await;
        if queue_length == 0 {
            return Ok(0);
        }

        self.set_processing(true);
        let mut processed_count = 0u32;

        info!("开始处理暂存的添加任务，当前队列长度: {}", queue_length);

        while let Some(task) = self.dequeue_task().await {
            info!("正在处理添加任务: {} 名称={}", task.source_type, task.name);

            // 将任务转换为AddVideoSourceRequest
            let request = crate::api::request::AddVideoSourceRequest {
                source_type: task.source_type.clone(),
                name: task.name.clone(),
                source_id: task.source_id.clone(),
                path: task.path.clone(),
                up_id: task.up_id.clone(),
                collection_type: task.collection_type.clone(),
                media_id: task.media_id.clone(),
                ep_id: task.ep_id.clone(),
                download_all_seasons: task.download_all_seasons,
                selected_seasons: task.selected_seasons.clone(),
                selected_videos: None,             // 任务队列中暂时不支持选择性视频
                cover: None,                       // 任务队列中暂时不支持封面，等前端传递
                merge_to_source_id: None,          // 任务队列中暂时不支持合并功能
                keyword_filters: None,             // 任务队列中暂时不支持关键词过滤器
                keyword_filter_mode: None,         // 任务队列中暂时不支持过滤模式
                audio_only: None,                  // 任务队列中使用默认值
                download_danmaku: None,            // 任务队列中使用默认值
                download_subtitle: None,           // 任务队列中使用默认值
                ai_rename: None,                   // 任务队列中使用默认值
                ai_rename_video_prompt: None,      // 任务队列中使用默认值
                ai_rename_audio_prompt: None,      // 任务队列中使用默认值
                ai_rename_enable_multi_page: None, // 任务队列中使用默认值
                ai_rename_enable_collection: None, // 任务队列中使用默认值
                ai_rename_enable_bangumi: None,    // 任务队列中使用默认值
                ai_rename_rename_parent_dir: None, // 任务队列中使用默认值
                audio_only_m4a_only: None,         // 任务队列中使用默认值
                flat_folder: None,                 // 任务队列中使用默认值
                use_dynamic_api: None,             // 任务队列中使用默认值
            };

            match add_video_source_internal(db.clone(), request).await {
                Ok(response) => {
                    info!("添加任务执行成功: {}", response.message);
                    processed_count += 1;

                    // 标记数据库任务为已完成
                    if let Err(e) = self.mark_task_completed(&task, &db).await {
                        error!("更新任务完成状态失败: {:#}", e);
                    }
                }
                Err(e) => {
                    error!(
                        "添加任务执行失败: {} 名称={}, 错误: {:#?}",
                        task.source_type, task.name, e
                    );

                    // 标记数据库任务为失败
                    if let Err(e) = self.mark_task_failed(&task, &db).await {
                        error!("更新任务失败状态失败: {:#}", e);
                    }
                }
            }

            // 每个任务之间稍作间隔，避免过于频繁的数据库操作
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }

        self.set_processing(false);

        info!("添加任务队列处理完成，共处理 {} 个任务", processed_count);

        Ok(processed_count)
    }
}

/// 配置任务队列管理器
pub struct ConfigTaskQueue {
    /// 待处理的更新配置任务队列
    update_queue: Mutex<VecDeque<UpdateConfigTask>>,
    /// 待处理的重载配置任务队列
    reload_queue: Mutex<VecDeque<ReloadConfigTask>>,
    /// 是否正在处理配置任务
    is_processing: AtomicBool,
}

impl ConfigTaskQueue {
    pub fn new() -> Self {
        Self {
            update_queue: Mutex::new(VecDeque::new()),
            reload_queue: Mutex::new(VecDeque::new()),
            is_processing: AtomicBool::new(false),
        }
    }

    /// 添加更新配置任务到队列（同时保存到数据库）
    pub async fn enqueue_update_task(&self, task: UpdateConfigTask, connection: &DatabaseConnection) -> Result<()> {
        // 保存到数据库
        let task_data = serde_json::to_string(&task)?;
        let active_model = task_queue::ActiveModel {
            task_type: Set(TaskType::UpdateConfig),
            task_data: Set(task_data),
            status: Set(TaskStatus::Pending),
            retry_count: Set(0),
            created_at: Set(now_standard_string()),
            updated_at: Set(now_standard_string()),
            ..Default::default()
        };

        let result = active_model.insert(connection).await?;

        // 添加到内存队列
        let mut queue = self.update_queue.lock().await;
        info!(
            "更新配置任务已加入队列, 队列长度: {} (数据库ID: {})",
            queue.len() + 1,
            result.id
        );
        queue.push_back(task);

        Ok(())
    }

    /// 添加重载配置任务到队列（同时保存到数据库）
    pub async fn enqueue_reload_task(&self, task: ReloadConfigTask, connection: &DatabaseConnection) -> Result<()> {
        // 保存到数据库
        let task_data = serde_json::to_string(&task)?;
        let active_model = task_queue::ActiveModel {
            task_type: Set(TaskType::ReloadConfig),
            task_data: Set(task_data),
            status: Set(TaskStatus::Pending),
            retry_count: Set(0),
            created_at: Set(now_standard_string()),
            updated_at: Set(now_standard_string()),
            ..Default::default()
        };

        let result = active_model.insert(connection).await?;

        // 添加到内存队列
        let mut queue = self.reload_queue.lock().await;
        info!(
            "重载配置任务已加入队列, 队列长度: {} (数据库ID: {})",
            queue.len() + 1,
            result.id
        );
        queue.push_back(task);

        Ok(())
    }

    /// 从更新配置队列中取出下一个任务
    pub async fn dequeue_update_task(&self) -> Option<UpdateConfigTask> {
        let mut queue = self.update_queue.lock().await;
        queue.pop_front()
    }

    /// 从重载配置队列中取出下一个任务
    pub async fn dequeue_reload_task(&self) -> Option<ReloadConfigTask> {
        let mut queue = self.reload_queue.lock().await;
        queue.pop_front()
    }

    /// 标记更新配置任务为已完成（更新数据库状态）
    pub async fn mark_update_task_completed(
        &self,
        task: &UpdateConfigTask,
        connection: &DatabaseConnection,
    ) -> Result<()> {
        let task_data = serde_json::to_string(task)?;

        // 查找并更新数据库中的任务状态
        if let Some(db_task) = TaskQueueEntity::find()
            .filter(task_queue::Column::TaskType.eq(TaskType::UpdateConfig))
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

    /// 标记更新配置任务为失败（更新数据库状态）
    pub async fn mark_update_task_failed(
        &self,
        task: &UpdateConfigTask,
        connection: &DatabaseConnection,
    ) -> Result<()> {
        let task_data = serde_json::to_string(task)?;

        // 查找并更新数据库中的任务状态
        if let Some(db_task) = TaskQueueEntity::find()
            .filter(task_queue::Column::TaskType.eq(TaskType::UpdateConfig))
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

    /// 标记重载配置任务为已完成（更新数据库状态）
    pub async fn mark_reload_task_completed(
        &self,
        task: &ReloadConfigTask,
        connection: &DatabaseConnection,
    ) -> Result<()> {
        let task_data = serde_json::to_string(task)?;

        // 查找并更新数据库中的任务状态
        if let Some(db_task) = TaskQueueEntity::find()
            .filter(task_queue::Column::TaskType.eq(TaskType::ReloadConfig))
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

    /// 标记重载配置任务为失败（更新数据库状态）
    pub async fn mark_reload_task_failed(
        &self,
        task: &ReloadConfigTask,
        connection: &DatabaseConnection,
    ) -> Result<()> {
        let task_data = serde_json::to_string(task)?;

        // 查找并更新数据库中的任务状态
        if let Some(db_task) = TaskQueueEntity::find()
            .filter(task_queue::Column::TaskType.eq(TaskType::ReloadConfig))
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

    /// 获取更新配置队列长度
    pub async fn update_queue_length(&self) -> usize {
        let queue = self.update_queue.lock().await;
        queue.len()
    }

    /// 获取重载配置队列长度
    pub async fn reload_queue_length(&self) -> usize {
        let queue = self.reload_queue.lock().await;
        queue.len()
    }

    /// 获取更新配置任务快照
    pub async fn list_update_tasks(&self) -> Vec<UpdateConfigTask> {
        let queue = self.update_queue.lock().await;
        queue.iter().cloned().collect()
    }

    /// 获取重载配置任务快照
    pub async fn list_reload_tasks(&self) -> Vec<ReloadConfigTask> {
        let queue = self.reload_queue.lock().await;
        queue.iter().cloned().collect()
    }

    /// 取消待处理配置任务（仅支持还在内存等待队列中的任务）
    pub async fn cancel_task(&self, task_id: &str, connection: &DatabaseConnection) -> Result<bool> {
        let removed_update_task = {
            let mut queue = self.update_queue.lock().await;
            if let Some(index) = queue.iter().position(|task| task.task_id == task_id) {
                queue.remove(index)
            } else {
                None
            }
        };

        if let Some(task) = removed_update_task {
            let task_data = serde_json::to_string(&task)?;
            TaskQueueEntity::delete_many()
                .filter(task_queue::Column::TaskType.eq(TaskType::UpdateConfig))
                .filter(task_queue::Column::TaskData.eq(task_data))
                .filter(task_queue::Column::Status.eq(TaskStatus::Pending))
                .exec(connection)
                .await?;
            return Ok(true);
        }

        let removed_reload_task = {
            let mut queue = self.reload_queue.lock().await;
            if let Some(index) = queue.iter().position(|task| task.task_id == task_id) {
                queue.remove(index)
            } else {
                None
            }
        };

        if let Some(task) = removed_reload_task {
            let task_data = serde_json::to_string(&task)?;
            TaskQueueEntity::delete_many()
                .filter(task_queue::Column::TaskType.eq(TaskType::ReloadConfig))
                .filter(task_queue::Column::TaskData.eq(task_data))
                .filter(task_queue::Column::Status.eq(TaskStatus::Pending))
                .exec(connection)
                .await?;
            return Ok(true);
        }

        Ok(false)
    }

    /// 检查是否正在处理配置任务
    pub fn is_processing(&self) -> bool {
        self.is_processing.load(Ordering::SeqCst)
    }

    /// 设置处理状态
    pub fn set_processing(&self, is_processing: bool) {
        self.is_processing.store(is_processing, Ordering::SeqCst);
    }

    /// 查询数据库中待处理的更新配置任务数量
    pub async fn get_pending_update_tasks_count(&self, connection: &DatabaseConnection) -> Result<u64, anyhow::Error> {
        let count = TaskQueueEntity::find()
            .filter(task_queue::Column::TaskType.eq(TaskType::UpdateConfig))
            .filter(task_queue::Column::Status.eq(TaskStatus::Pending))
            .count(connection)
            .await?;
        Ok(count)
    }

    /// 查询数据库中待处理的重载配置任务数量
    pub async fn get_pending_reload_tasks_count(&self, connection: &DatabaseConnection) -> Result<u64, anyhow::Error> {
        let count = TaskQueueEntity::find()
            .filter(task_queue::Column::TaskType.eq(TaskType::ReloadConfig))
            .filter(task_queue::Column::Status.eq(TaskStatus::Pending))
            .count(connection)
            .await?;
        Ok(count)
    }

    /// 从数据库恢复配置任务到内存队列
    pub async fn recover_config_tasks_from_db(&self, connection: &DatabaseConnection) -> Result<u32, anyhow::Error> {
        // 查询所有待处理的配置任务
        let pending_tasks = TaskQueueEntity::find()
            .filter(task_queue::Column::Status.eq(TaskStatus::Pending))
            .filter(task_queue::Column::TaskType.is_in([TaskType::UpdateConfig, TaskType::ReloadConfig]))
            .order_by_asc(task_queue::Column::CreatedAt)
            .all(connection)
            .await?;

        let mut recovered_count = 0u32;

        for task_model in pending_tasks {
            match task_model.task_type {
                TaskType::UpdateConfig => {
                    if let Ok(task) = serde_json::from_str::<UpdateConfigTask>(&task_model.task_data) {
                        let mut queue = self.update_queue.lock().await;
                        queue.push_back(task);
                        recovered_count += 1;
                    } else {
                        warn!("无法反序列化更新配置任务数据: {}", task_model.task_data);
                    }
                }
                TaskType::ReloadConfig => {
                    if let Ok(task) = serde_json::from_str::<ReloadConfigTask>(&task_model.task_data) {
                        let mut queue = self.reload_queue.lock().await;
                        queue.push_back(task);
                        recovered_count += 1;
                    } else {
                        warn!("无法反序列化重载配置任务数据: {}", task_model.task_data);
                    }
                }
                _ => {
                    // 忽略其他任务类型
                }
            }
        }

        if recovered_count > 0 {
            info!("从数据库恢复了 {} 个配置任务到内存队列", recovered_count);
        }

        Ok(recovered_count)
    }

    /// 处理队列中的所有配置任务
    pub async fn process_all_tasks(&self, db: Arc<DatabaseConnection>) -> Result<u32, anyhow::Error> {
        use crate::api::handler::{reload_config_internal, update_config_internal};

        if self.is_processing() {
            debug!("配置任务队列正在处理中，跳过重复处理");
            return Ok(0);
        }

        self.set_processing(true);
        let mut processed_count = 0u32;

        let update_count = self.update_queue_length().await;
        let reload_count = self.reload_queue_length().await;

        // 检查数据库中是否有待处理的配置任务
        let db_update_count = self.get_pending_update_tasks_count(&db).await.unwrap_or(0);
        let db_reload_count = self.get_pending_reload_tasks_count(&db).await.unwrap_or(0);

        // 如果内存队列为空但数据库中有待处理任务，则先恢复
        if update_count == 0 && reload_count == 0 && (db_update_count > 0 || db_reload_count > 0) {
            info!(
                "检测到数据库中有 {} 个更新配置任务和 {} 个重载配置任务，开始恢复到内存队列",
                db_update_count, db_reload_count
            );

            if let Ok(recovered) = self.recover_config_tasks_from_db(&db).await {
                if recovered > 0 {
                    info!("成功恢复 {} 个配置任务，重新获取队列长度", recovered);
                    // 重新获取内存队列长度
                    let new_update_count = self.update_queue_length().await;
                    let new_reload_count = self.reload_queue_length().await;

                    if new_update_count == 0 && new_reload_count == 0 {
                        warn!("恢复任务后内存队列仍为空，可能存在数据不一致问题");
                        self.set_processing(false);
                        return Ok(0);
                    }
                } else {
                    debug!("没有成功恢复任何配置任务");
                    self.set_processing(false);
                    return Ok(0);
                }
            } else {
                error!("恢复配置任务失败");
                self.set_processing(false);
                return Ok(0);
            }
        } else if update_count == 0 && reload_count == 0 {
            // 内存队列和数据库都没有待处理任务
            self.set_processing(false);
            return Ok(0);
        }

        // 重新获取最新的队列长度用于日志
        let final_update_count = self.update_queue_length().await;
        let final_reload_count = self.reload_queue_length().await;

        info!(
            "开始处理暂存的配置任务，更新配置队列长度: {}, 重载配置队列长度: {}",
            final_update_count, final_reload_count
        );

        // 先处理更新配置任务
        while let Some(task) = self.dequeue_update_task().await {
            info!("正在处理更新配置任务");

            // 将任务转换为UpdateConfigRequest
            let request = crate::api::request::UpdateConfigRequest {
                video_name: task.video_name.clone(),
                page_name: task.page_name.clone(),
                multi_page_name: task.multi_page_name.clone(),
                bangumi_name: task.bangumi_name.clone(),
                folder_structure: task.folder_structure.clone(),
                bangumi_folder_name: task.bangumi_folder_name.clone(),
                collection_folder_mode: task.collection_folder_mode.clone(),
                collection_unified_name: task.collection_unified_name.clone(),
                time_format: task.time_format.clone(),
                interval: task.interval,
                nfo_time_type: task.nfo_time_type.clone(),
                parallel_download_enabled: task.parallel_download_enabled,
                parallel_download_threads: task.parallel_download_threads,
                parallel_download_use_aria2: task.parallel_download_use_aria2,
                // 视频质量设置
                video_max_quality: task.video_max_quality.clone(),
                video_min_quality: task.video_min_quality.clone(),
                audio_max_quality: task.audio_max_quality.clone(),
                audio_min_quality: task.audio_min_quality.clone(),
                codecs: task.codecs.clone(),
                no_dolby_video: task.no_dolby_video,
                no_dolby_audio: task.no_dolby_audio,
                no_hdr: task.no_hdr,
                no_hires: task.no_hires,
                // 弹幕设置
                danmaku_duration: task.danmaku_duration,
                danmaku_font: task.danmaku_font.clone(),
                danmaku_font_size: task.danmaku_font_size,
                danmaku_width_ratio: task.danmaku_width_ratio,
                danmaku_horizontal_gap: task.danmaku_horizontal_gap,
                danmaku_lane_size: task.danmaku_lane_size,
                danmaku_float_percentage: task.danmaku_float_percentage,
                danmaku_bottom_percentage: task.danmaku_bottom_percentage,
                danmaku_opacity: task.danmaku_opacity,
                danmaku_bold: task.danmaku_bold,
                danmaku_outline: task.danmaku_outline,
                danmaku_time_offset: task.danmaku_time_offset,
                // 并发控制设置
                concurrent_video: task.concurrent_video,
                concurrent_page: task.concurrent_page,
                rate_limit: task.rate_limit,
                rate_duration: task.rate_duration,
                // 其他设置
                cdn_sorting: task.cdn_sorting,
                // UP主投稿风控配置
                large_submission_threshold: task.large_submission_threshold,
                base_request_delay: task.base_request_delay,
                large_submission_delay_multiplier: task.large_submission_delay_multiplier,
                enable_progressive_delay: task.enable_progressive_delay,
                max_delay_multiplier: task.max_delay_multiplier,
                enable_incremental_fetch: task.enable_incremental_fetch,
                incremental_fallback_to_full: task.incremental_fallback_to_full,
                enable_batch_processing: task.enable_batch_processing,
                batch_size: task.batch_size,
                batch_delay_seconds: task.batch_delay_seconds,
                enable_auto_backoff: task.enable_auto_backoff,
                auto_backoff_base_seconds: task.auto_backoff_base_seconds,
                auto_backoff_max_multiplier: task.auto_backoff_max_multiplier,
                source_delay_seconds: task.source_delay_seconds,
                submission_source_delay_seconds: task.submission_source_delay_seconds,
                submission_scan_batch_size: task.submission_scan_batch_size,
                submission_adaptive_scan: task.submission_adaptive_scan,
                submission_adaptive_max_hours: task.submission_adaptive_max_hours,
                // 系统配置相关字段，任务队列中不使用
                scan_deleted_videos: None,
                // aria2监控配置，任务队列中不使用
                enable_aria2_health_check: None,
                enable_aria2_auto_restart: None,
                aria2_health_check_interval: None,
                // 多P视频目录结构配置
                multi_page_use_season_structure: task.multi_page_use_season_structure,
                // 合集目录结构配置
                collection_use_season_structure: task.collection_use_season_structure,
                // 番剧目录结构配置
                bangumi_use_season_structure: task.bangumi_use_season_structure,
                // UP主头像保存路径
                upper_path: task.upper_path.clone(),
                // 风控验证配置，任务队列中不使用
                risk_control_enabled: None,
                risk_control_mode: None,
                risk_control_timeout: None,
                // 自动验证配置，任务队列中不使用
                risk_control_auto_solve_service: None,
                risk_control_auto_solve_api_key: None,
                risk_control_auto_solve_max_retries: None,
                risk_control_auto_solve_timeout: None,
                // AI重命名配置，任务队列中不使用
                ai_rename_enabled: None,
                ai_rename_provider: None,
                ai_rename_base_url: None,
                ai_rename_api_key: None,
                ai_rename_deepseek_web_token: None,
                ai_rename_model: None,
                ai_rename_timeout_seconds: None,
                ai_rename_video_prompt_hint: None,
                ai_rename_audio_prompt_hint: None,
                ai_rename_enable_multi_page: None,
                ai_rename_enable_collection: None,
                ai_rename_enable_bangumi: None,
                ai_rename_rename_parent_dir: task.ai_rename_rename_parent_dir,
                // 服务器绑定地址，任务队列中不使用
                bind_address: None,
            };

            match update_config_internal(db.clone(), request).await {
                Ok(response) => {
                    info!("更新配置任务执行成功: {}", response.message);
                    // update_config_internal 已经处理了配置重载和内存优化重配置
                    processed_count += 1;

                    // 标记数据库任务为已完成
                    if let Err(e) = self.mark_update_task_completed(&task, &db).await {
                        error!("更新任务完成状态失败: {:#}", e);
                    }
                }
                Err(e) => {
                    error!("更新配置任务执行失败, 错误: {:#?}", e);

                    // 标记数据库任务为失败
                    if let Err(e) = self.mark_update_task_failed(&task, &db).await {
                        error!("更新任务失败状态失败: {:#}", e);
                    }
                }
            }

            // 每个任务之间稍作间隔
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }

        // 再处理重载配置任务
        while let Some(task) = self.dequeue_reload_task().await {
            info!("正在处理重载配置任务");

            match reload_config_internal().await {
                Ok(_) => {
                    info!("重载配置任务执行成功");

                    // mmap配置在启动时设置，不需要动态重配置

                    processed_count += 1;

                    // 标记数据库任务为已完成
                    if let Err(e) = self.mark_reload_task_completed(&task, &db).await {
                        error!("更新任务完成状态失败: {:#}", e);
                    }
                }
                Err(e) => {
                    error!("重载配置任务执行失败, 错误: {:#?}", e);

                    // 标记数据库任务为失败
                    if let Err(e) = self.mark_reload_task_failed(&task, &db).await {
                        error!("更新任务失败状态失败: {:#}", e);
                    }
                }
            }

            // 每个任务之间稍作间隔
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }

        self.set_processing(false);

        // 最终检查是否还有未处理的任务
        let remaining_update_count = self.update_queue_length().await;
        let remaining_reload_count = self.reload_queue_length().await;
        let remaining_db_update_count = self.get_pending_update_tasks_count(&db).await.unwrap_or(0);
        let remaining_db_reload_count = self.get_pending_reload_tasks_count(&db).await.unwrap_or(0);

        info!(
            "配置任务队列处理完成，共处理 {} 个任务，剩余内存队列: 更新({}) 重载({})，剩余数据库队列: 更新({}) 重载({})",
            processed_count, remaining_update_count, remaining_reload_count, remaining_db_update_count, remaining_db_reload_count
        );

        if remaining_update_count > 0
            || remaining_reload_count > 0
            || remaining_db_update_count > 0
            || remaining_db_reload_count > 0
        {
            warn!("配置任务处理完成后仍有剩余任务，可能需要下轮处理");
        }

        Ok(processed_count)
    }
}

/// 全局任务控制器，用于控制定时扫描任务的暂停和恢复
pub struct TaskController {
    /// 是否暂停定时扫描任务
    pub is_paused: AtomicBool,
    /// 是否正在扫描（用于检测扫描状态）
    pub is_scanning: AtomicBool,
    /// 是否刚刚恢复（用于立即开始新扫描）
    pub just_resumed: AtomicBool,
    /// 全局取消令牌，用于取消所有下载任务
    pub cancellation_token: Arc<Mutex<CancellationToken>>,
    /// 下载器的引用，用于暂停时停止下载
    pub downloader: Arc<Mutex<Option<Arc<crate::unified_downloader::UnifiedDownloader>>>>,
}

impl TaskController {
    pub fn new() -> Self {
        Self {
            is_paused: AtomicBool::new(false),
            is_scanning: AtomicBool::new(false),
            just_resumed: AtomicBool::new(false),
            cancellation_token: Arc::new(Mutex::new(CancellationToken::new())),
            downloader: Arc::new(Mutex::new(None)),
        }
    }

    /// 设置下载器引用
    pub async fn set_downloader(&self, downloader: Option<Arc<crate::unified_downloader::UnifiedDownloader>>) {
        let mut guard = self.downloader.lock().await;
        *guard = downloader;
    }

    /// 获取当前下载器的共享引用（如存在）
    pub async fn get_downloader(&self) -> Option<Arc<crate::unified_downloader::UnifiedDownloader>> {
        let guard = self.downloader.lock().await;
        guard.clone()
    }

    /// 暂停定时扫描任务
    pub async fn pause(&self) {
        self.is_paused.store(true, Ordering::SeqCst);
        // 立即重置扫描状态
        self.is_scanning.store(false, Ordering::SeqCst);
        // 重置恢复标志
        self.just_resumed.store(false, Ordering::SeqCst);

        // 取消所有正在进行的下载任务
        if let Ok(token) = self.cancellation_token.try_lock() {
            token.cancel();
        }

        // 停止下载器
        if let Ok(downloader_guard) = self.downloader.try_lock() {
            if let Some(downloader) = downloader_guard.as_ref() {
                if let Err(e) = downloader.shutdown().await {
                    error!("停止下载器失败: {:#}", e);
                } else {
                    info!("下载器已停止");
                }
            }
        }

        info!("定时扫描任务已暂停，所有下载任务已取消");
    }

    /// 恢复定时扫描任务
    pub fn resume(&self) {
        self.is_paused.store(false, Ordering::SeqCst);
        // 设置恢复标志，表示应该立即开始新扫描
        self.just_resumed.store(true, Ordering::SeqCst);
        // 创建新的取消令牌，用于新的下载任务
        if let Ok(mut token) = self.cancellation_token.try_lock() {
            *token = CancellationToken::new();
        }

        // 清理旧的downloader实例，强制下次扫描时创建新的
        if let Ok(mut downloader_guard) = self.downloader.try_lock() {
            *downloader_guard = None;
        }

        info!("定时扫描任务已恢复，将立即开始新一轮扫描");
    }

    /// 触发立即扫描（不等待定时器）
    pub fn trigger_scan_now(&self) {
        self.just_resumed.store(true, Ordering::SeqCst);
    }

    /// 检查是否暂停
    pub fn is_paused(&self) -> bool {
        self.is_paused.load(Ordering::SeqCst)
    }

    /// 检查是否刚刚恢复（并重置标志）
    pub fn take_just_resumed(&self) -> bool {
        self.just_resumed.swap(false, Ordering::SeqCst)
    }

    /// 设置扫描状态
    pub fn set_scanning(&self, is_scanning: bool) {
        self.is_scanning.store(is_scanning, Ordering::SeqCst);
        if is_scanning {
            debug!("扫描任务开始");
        } else {
            debug!("扫描任务结束");
        }
    }

    /// 检查是否正在扫描
    pub fn is_scanning(&self) -> bool {
        self.is_scanning.load(Ordering::SeqCst)
    }

    /// 等待直到任务恢复（非阻塞检查）
    pub async fn wait_if_paused(&self) {
        while self.is_paused() {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }
    }

    /// 获取当前的取消令牌
    pub async fn get_cancellation_token(&self) -> CancellationToken {
        let token = self.cancellation_token.lock().await;
        token.clone()
    }

    /// 重置取消令牌（用于新一轮扫描）
    pub async fn reset_cancellation_token(&self) {
        let mut token = self.cancellation_token.lock().await;
        *token = CancellationToken::new();
    }
}

impl Default for TaskController {
    fn default() -> Self {
        Self::new()
    }
}

/// 全局任务控制器实例
pub static TASK_CONTROLLER: once_cell::sync::Lazy<Arc<TaskController>> =
    once_cell::sync::Lazy::new(|| Arc::new(TaskController::new()));

/// 全局删除任务队列实例
pub static DELETE_TASK_QUEUE: once_cell::sync::Lazy<Arc<DeleteTaskQueue>> =
    once_cell::sync::Lazy::new(|| Arc::new(DeleteTaskQueue::new()));

/// 全局添加任务队列实例
pub static ADD_TASK_QUEUE: once_cell::sync::Lazy<Arc<AddTaskQueue>> =
    once_cell::sync::Lazy::new(|| Arc::new(AddTaskQueue::new()));

/// 全局配置任务队列实例
pub static CONFIG_TASK_QUEUE: once_cell::sync::Lazy<Arc<ConfigTaskQueue>> =
    once_cell::sync::Lazy::new(|| Arc::new(ConfigTaskQueue::new()));

/// 全局视频删除任务队列实例
pub static VIDEO_DELETE_TASK_QUEUE: once_cell::sync::Lazy<Arc<VideoDeleteTaskQueue>> =
    once_cell::sync::Lazy::new(|| Arc::new(VideoDeleteTaskQueue::new()));

/// 暂停定时扫描任务的便捷函数
pub async fn pause_scanning() {
    TASK_CONTROLLER.pause().await;
}

/// 恢复定时扫描任务的便捷函数
pub fn resume_scanning() {
    TASK_CONTROLLER.resume();
}

/// 检查是否正在扫描的便捷函数
pub fn is_scanning() -> bool {
    TASK_CONTROLLER.is_scanning()
}

/// 添加删除任务到队列的便捷函数
pub async fn enqueue_delete_task(task: DeleteVideoSourceTask, connection: &DatabaseConnection) -> Result<()> {
    DELETE_TASK_QUEUE.enqueue_task(task, connection).await
}

/// 处理所有删除任务的便捷函数
pub async fn process_delete_tasks(db: Arc<DatabaseConnection>) -> Result<u32, anyhow::Error> {
    DELETE_TASK_QUEUE.process_all_tasks(db).await
}

/// 添加添加任务到队列的便捷函数
pub async fn enqueue_add_task(task: AddVideoSourceTask, connection: &DatabaseConnection) -> Result<()> {
    ADD_TASK_QUEUE.enqueue_task(task, connection).await
}

/// 处理所有添加任务的便捷函数
pub async fn process_add_tasks(db: Arc<DatabaseConnection>) -> Result<u32, anyhow::Error> {
    ADD_TASK_QUEUE.process_all_tasks(db).await
}

/// 添加更新配置任务到队列的便捷函数
pub async fn enqueue_update_task(task: UpdateConfigTask, connection: &DatabaseConnection) -> Result<()> {
    CONFIG_TASK_QUEUE.enqueue_update_task(task, connection).await
}

/// 添加重载配置任务到队列的便捷函数
pub async fn enqueue_reload_task(task: ReloadConfigTask, connection: &DatabaseConnection) -> Result<()> {
    CONFIG_TASK_QUEUE.enqueue_reload_task(task, connection).await
}

/// 处理所有配置任务的便捷函数
pub async fn process_config_tasks(db: Arc<DatabaseConnection>) -> Result<u32, anyhow::Error> {
    CONFIG_TASK_QUEUE.process_all_tasks(db).await
}

/// 添加视频删除任务到队列的便捷函数
pub async fn enqueue_video_delete_task(task: DeleteVideoTask, connection: &DatabaseConnection) -> Result<()> {
    VIDEO_DELETE_TASK_QUEUE.enqueue_task(task, connection).await
}

/// 处理所有视频删除任务的便捷函数
pub async fn process_video_delete_tasks(db: Arc<DatabaseConnection>) -> Result<u32, anyhow::Error> {
    VIDEO_DELETE_TASK_QUEUE.process_all_tasks(db).await
}

/// 取消指定任务（仅支持待处理且仍在内存等待队列中的任务）
pub async fn cancel_pending_task(task_id: &str, connection: &DatabaseConnection) -> Result<bool, anyhow::Error> {
    if DELETE_TASK_QUEUE.cancel_task(task_id, connection).await? {
        return Ok(true);
    }

    if VIDEO_DELETE_TASK_QUEUE.cancel_task(task_id, connection).await? {
        return Ok(true);
    }

    if ADD_TASK_QUEUE.cancel_task(task_id, connection).await? {
        return Ok(true);
    }

    if CONFIG_TASK_QUEUE.cancel_task(task_id, connection).await? {
        return Ok(true);
    }

    Ok(false)
}

/// 从数据库恢复待处理的任务到内存队列中
pub async fn recover_pending_tasks(connection: &DatabaseConnection) -> Result<(), anyhow::Error> {
    info!("开始恢复数据库中的待处理任务到内存队列");

    // 查询所有待处理状态的任务
    let pending_tasks = TaskQueueEntity::find()
        .filter(task_queue::Column::Status.eq(TaskStatus::Pending))
        .order_by_asc(task_queue::Column::CreatedAt) // 按创建时间排序
        .all(connection)
        .await?;

    let mut recovered_count = 0;

    for db_task in pending_tasks {
        let task_data = &db_task.task_data;

        match db_task.task_type {
            TaskType::DeleteVideoSource => {
                match serde_json::from_str::<DeleteVideoSourceTask>(task_data) {
                    Ok(task) => {
                        // 直接添加到内存队列，不再写入数据库
                        let mut queue = DELETE_TASK_QUEUE.queue.lock().await;
                        queue.push_back(task);
                        recovered_count += 1;
                    }
                    Err(e) => {
                        error!("反序列化删除视频源任务失败: {:#}", e);
                    }
                }
            }
            TaskType::DeleteVideo => match serde_json::from_str::<DeleteVideoTask>(task_data) {
                Ok(task) => {
                    let mut queue = VIDEO_DELETE_TASK_QUEUE.queue.lock().await;
                    queue.push_back(task);
                    recovered_count += 1;
                }
                Err(e) => {
                    error!("反序列化删除视频任务失败: {:#}", e);
                }
            },
            TaskType::AddVideoSource => match serde_json::from_str::<AddVideoSourceTask>(task_data) {
                Ok(task) => {
                    let mut queue = ADD_TASK_QUEUE.queue.lock().await;
                    queue.push_back(task);
                    recovered_count += 1;
                }
                Err(e) => {
                    error!("反序列化添加视频源任务失败: {:#}", e);
                }
            },
            TaskType::UpdateConfig => match serde_json::from_str::<UpdateConfigTask>(task_data) {
                Ok(task) => {
                    let mut queue = CONFIG_TASK_QUEUE.update_queue.lock().await;
                    queue.push_back(task);
                    recovered_count += 1;
                }
                Err(e) => {
                    error!("反序列化更新配置任务失败: {:#}", e);
                }
            },
            TaskType::ReloadConfig => match serde_json::from_str::<ReloadConfigTask>(task_data) {
                Ok(task) => {
                    let mut queue = CONFIG_TASK_QUEUE.reload_queue.lock().await;
                    queue.push_back(task);
                    recovered_count += 1;
                }
                Err(e) => {
                    error!("反序列化重载配置任务失败: {:#}", e);
                }
            },
        }
    }

    if recovered_count > 0 {
        info!("成功恢复 {} 个待处理任务到内存队列", recovered_count);
    } else {
        info!("没有需要恢复的待处理任务");
    }

    Ok(())
}
